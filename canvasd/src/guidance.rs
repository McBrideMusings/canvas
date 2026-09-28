//! Guidance override storage: a global text override and one per repo,
//! each optional (`None` means "the caller's own compiled-in default
//! applies"). Persisted as one small JSON file — `guidance.json` — separate
//! from `stream.jsonl`: this isn't a card or a session, and it must not age
//! out on the 24h retention window those go through.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const GUIDANCE_FILE: &str = "guidance.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuidanceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub repos: HashMap<String, String>,
}

impl GuidanceConfig {
    pub fn repo_override(&self, repo: Option<&str>) -> Option<String> {
        repo.and_then(|r| self.repos.get(r)).cloned()
    }
}

pub async fn load(dir: &Path) -> GuidanceConfig {
    match tokio::fs::read(dir.join(GUIDANCE_FILE)).await {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => GuidanceConfig::default(),
    }
}

pub async fn save(dir: &Path, config: &GuidanceConfig) {
    let path = dir.join(GUIDANCE_FILE);
    let Ok(bytes) = serde_json::to_vec_pretty(config) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if tokio::fs::write(&tmp, bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, &path).await;
    }
}
