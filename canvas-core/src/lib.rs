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
