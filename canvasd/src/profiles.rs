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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileSet {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub profiles: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub repos: HashMap<String, String>,
}

impl ProfileSet {
    /// The name of the profile that applies for `repo`: its own assignment,
    /// else the global one, else `None`.
    pub fn effective_profile(&self, repo: Option<&str>) -> Option<&str> {
        repo.and_then(|r| self.repos.get(r))
            .or(self.global.as_ref())
            .map(String::as_str)
    }

    pub fn effective_text(&self, repo: Option<&str>) -> Option<&str> {
        self.effective_profile(repo)
            .and_then(|name| self.profiles.get(name))
            .map(String::as_str)
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
