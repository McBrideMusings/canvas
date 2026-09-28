//! Shared data shapes for canvasd and its clients (canvas CLI, hooks, Tauri app).

use serde::{Deserialize, Serialize};

/// One main Claude Code conversation on this Mac, from SessionStart to SessionEnd.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub cwd: String,
    /// cwd basename, computed on upsert.
    pub name: String,
    /// `owner/repo` of the cwd's `origin` remote when it points at
    /// github.com, read once when the daemon first learns of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

/// One deliberate `canvas post`, shown as one card. Every post creates its
/// own card — there is no open/closed lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: String,
    pub session_id: String,
    pub at: String,
    pub html: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
}

/// Request bodies use snake_case, matching Claude Code hook JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertSessionRequest {
    pub session_id: String,
    pub cwd: String,
}

/// Request bodies use snake_case, matching Claude Code hook JSON.
/// `images`/`targets` are empty until a later slice fills them in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostRequest {
    pub session_id: String,
    pub cwd: String,
    pub html: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
}

/// Body for `PUT /api/cards/:id` — replaces an existing card's content in
/// place. No `session_id`/`cwd`: the card already carries those, and the
/// route 404s if `id` doesn't name an existing card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCardRequest {
    pub html: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
}

/// What `GET /api/guidance?cwd=<cwd>` reports: the stored global override,
/// the repo `cwd` resolved to (when it has a GitHub `origin`), and that
/// repo's own override. A caller with neither wants the compiled-in default
/// it already carries — canvasd never ships one of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuidanceState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_override: Option<String>,
}

impl GuidanceState {
    /// The text a session should actually see: the repo override, else the
    /// global one, else `None` (fall back to the compiled-in default).
    pub fn effective(&self) -> Option<&str> {
        self.repo_override.as_deref().or(self.global.as_deref())
    }
}

/// Body for `PUT /api/guidance/global`. `text: None` (or blank) clears the
/// override and reverts every session to its own compiled-in default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetGlobalGuidanceRequest {
    pub text: Option<String>,
}

/// Body for `PUT /api/guidance/repo`. `text: None` (or blank) clears that
/// repo's override.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRepoGuidanceRequest {
    pub repo: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateResponse {
    pub sessions: Vec<Session>,
    /// Newest first.
    pub cards: Vec<Card>,
}
