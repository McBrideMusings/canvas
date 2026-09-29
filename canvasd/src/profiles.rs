//! Named-profile override storage, generalized so a future feature that
//! wants a "global default, overridable per repo" text setting can reuse it
//! instead of growing its own bespoke field. A "kind" owns its own set of
//! named profiles plus which one is assigned globally and which repos have
//! their own assignment. Persisted as one small JSON file — `profiles.json`
//! — separate from `stream.jsonl`: this isn't a card or a session, and it
//! must not age out on the 24h retention window those go through.
//!
//! `posting-guidance` is the one kind actually shipped today — the settings
//! page has no kind switcher and never will until a second kind has a real
//! consumer; the kind parameter exists so that day doesn't need a rewrite.

use std::collections::HashMap;
use std::path::Path;

use canvas_core::ProfileMode;
use serde::{Deserialize, Serialize};

pub const PROFILES_FILE: &str = "profiles.json";

/// The posting guidance block a SessionStart hook prints to an agent's
/// context — the one feature this abstraction replaces the original
/// single-purpose `GuidanceConfig` for.
pub const KIND_POSTING_GUIDANCE: &str = "posting-guidance";

/// Same file `cli/src/guidance.rs` embeds as `canvas::guidance::TEXT` — the
/// two copies stay in sync because there's only one `plugin/guidance.md` to
/// edit, not because either crate depends on the other for it. Shown
/// read-only in the settings page's profile list so the fallback a session
/// gets when nothing is assigned isn't invisible.
const BUILTIN_POSTING_GUIDANCE: &str = include_str!("../../plugin/guidance.md");

/// The compiled-in default for `kind`, for kinds that have one.
pub fn builtin_default(kind: &str) -> Option<&'static str> {
    (kind == KIND_POSTING_GUIDANCE).then_some(BUILTIN_POSTING_GUIDANCE)
}

fn is_default_mode(mode: &ProfileMode) -> bool {
    *mode == ProfileMode::default()
}

/// Source name reported for the compiled-in default text.
pub const BUILTIN_SOURCE: &str = "built-in";

/// The text a session receives, and the ordered sources it was joined from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    pub text: String,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileSet {
    #[serde(default, skip_serializing_if = "is_default_mode")]
    pub mode: ProfileMode,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub profiles: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub repos: HashMap<String, String>,
}

impl ProfileSet {
    /// What a session in `repo` receives for `kind`, or `None` when nothing is
    /// assigned for it (the caller then uses its own compiled-in default).
    pub fn compose(&self, kind: &str, repo: Option<&str>) -> Option<Composed> {
        let repo_name = repo
            .and_then(|r| self.repos.get(r))
            .map(String::as_str)
            .filter(|n| self.profiles.contains_key(*n));
        let global_name = self
            .global
            .as_deref()
            .filter(|n| self.profiles.contains_key(*n));
        let mut parts: Vec<(&str, &str)> = Vec::new();
        match self.mode {
            ProfileMode::Replace => {
                let name = repo_name.or(global_name)?;
                parts.push((name, &self.profiles[name]));
            }
            ProfileMode::Additive => {
                repo_name.or(global_name)?;
                match global_name {
                    Some(name) => parts.push((name, &self.profiles[name])),
                    None => {
                        if let Some(text) = builtin_default(kind) {
                            parts.push((BUILTIN_SOURCE, text));
                        }
                    }
                }
                if let Some(name) = repo_name.filter(|n| Some(*n) != global_name) {
                    parts.push((name, &self.profiles[name]));
                }
            }
        }
        Some(Composed {
            text: parts
                .iter()
                .map(|(_, t)| t.trim_end())
                .collect::<Vec<_>>()
                .join("\n\n"),
            sources: parts.iter().map(|(n, _)| n.to_string()).collect(),
        })
    }

    /// Removes a profile and every global/repo assignment pointing at it —
    /// an assignment naming a profile that no longer exists would otherwise
    /// silently fall back to the compiled-in default with no sign why.
    fn remove_profile(&mut self, name: &str) {
        self.profiles.remove(name);
        if self.global.as_deref() == Some(name) {
            self.global = None;
        }
        self.repos.retain(|_, assigned| assigned != name);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfilesConfig {
    #[serde(default)]
    pub kinds: HashMap<String, ProfileSet>,
}

impl ProfilesConfig {
    pub fn kind(&self, kind: &str) -> ProfileSet {
        self.kinds.get(kind).cloned().unwrap_or_default()
    }

    pub fn kind_mut(&mut self, kind: &str) -> &mut ProfileSet {
        self.kinds.entry(kind.to_string()).or_default()
    }

    pub fn set_profile_text(&mut self, kind: &str, name: &str, text: Option<String>) {
        let set = self.kind_mut(kind);
        match text.filter(|t| !t.trim().is_empty()) {
            Some(text) => {
                set.profiles.insert(name.to_string(), text);
            }
            None => set.remove_profile(name),
        }
    }

    pub fn set_mode(&mut self, kind: &str, mode: ProfileMode) {
        self.kind_mut(kind).mode = mode;
    }

    pub fn set_global(&mut self, kind: &str, profile: Option<String>) {
        self.kind_mut(kind).global = profile;
    }

    pub fn set_repo(&mut self, kind: &str, repo: &str, profile: Option<String>) {
        let set = self.kind_mut(kind);
        match profile {
            Some(profile) => {
                set.repos.insert(repo.to_string(), profile);
            }
            None => {
                set.repos.remove(repo);
            }
        }
    }
}

pub async fn load(dir: &Path) -> ProfilesConfig {
    match tokio::fs::read(dir.join(PROFILES_FILE)).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => ProfilesConfig::default(),
    }
}

pub async fn save(dir: &Path, config: &ProfilesConfig) {
    let path = dir.join(PROFILES_FILE);
    let Ok(bytes) = serde_json::to_vec_pretty(config) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if tokio::fs::write(&tmp, bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, &path).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(mode: ProfileMode, global: Option<&str>, repo: Option<&str>) -> ProfileSet {
        let mut s = ProfileSet {
            mode,
            ..Default::default()
        };
        s.profiles.insert("g".into(), "GLOBAL".into());
        s.profiles.insert("r".into(), "REPO".into());
        s.global = global.map(str::to_string);
        if let Some(r) = repo {
            s.repos.insert("o/x".into(), r.to_string());
        }
        s
    }

    fn compose(s: &ProfileSet) -> Option<Composed> {
        s.compose(KIND_POSTING_GUIDANCE, Some("o/x"))
    }

    #[test]
    fn additive_nothing_assigned_is_none() {
        assert_eq!(compose(&set(ProfileMode::Additive, None, None)), None);
    }

    #[test]
    fn additive_global_only_is_global_text() {
        let c = compose(&set(ProfileMode::Additive, Some("g"), None)).unwrap();
        assert_eq!(
            (c.text.as_str(), c.sources),
            ("GLOBAL", vec!["g".to_string()])
        );
    }

    #[test]
    fn additive_repo_only_is_builtin_then_repo() {
        let c = compose(&set(ProfileMode::Additive, None, Some("r"))).unwrap();
        assert_eq!(c.sources, vec!["built-in", "r"]);
        assert!(c.text.starts_with(BUILTIN_POSTING_GUIDANCE.trim_end()));
        assert!(c.text.ends_with("\n\nREPO"));
    }

    #[test]
    fn additive_global_and_repo_has_no_builtin() {
        let c = compose(&set(ProfileMode::Additive, Some("g"), Some("r"))).unwrap();
        assert_eq!(
            (c.text.as_str(), c.sources),
            ("GLOBAL\n\nREPO", vec!["g".to_string(), "r".to_string()])
        );
    }

    #[test]
    fn additive_same_profile_is_sent_once() {
        let c = compose(&set(ProfileMode::Additive, Some("g"), Some("g"))).unwrap();
        assert_eq!(
            (c.text.as_str(), c.sources),
            ("GLOBAL", vec!["g".to_string()])
        );
    }

    #[test]
    fn replace_repo_wins_else_global_else_none() {
        let c = compose(&set(ProfileMode::Replace, Some("g"), Some("r"))).unwrap();
        assert_eq!(
            (c.text.as_str(), c.sources),
            ("REPO", vec!["r".to_string()])
        );
        let c = compose(&set(ProfileMode::Replace, Some("g"), None)).unwrap();
        assert_eq!(c.text, "GLOBAL");
        assert_eq!(compose(&set(ProfileMode::Replace, None, None)), None);
    }

    #[test]
    fn default_mode_is_omitted_from_json() {
        let json = serde_json::to_string(&set(ProfileMode::Additive, None, None)).unwrap();
        assert!(!json.contains("mode"));
        let json = serde_json::to_string(&set(ProfileMode::Replace, None, None)).unwrap();
        assert!(json.contains("\"mode\":\"replace\""));
    }
}
