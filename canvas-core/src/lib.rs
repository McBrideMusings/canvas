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
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

/// Everything one turn of one session produced: explicit posts, images, links
/// and file paths, shown as one card.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCard {
    pub id: String,
    pub session_id: String,
    pub at: String,
    pub html: Vec<String>,
    pub links: Vec<String>,
    pub paths: Vec<String>,
    pub images: Vec<String>,
    pub open: bool,
}

/// Request bodies use snake_case, matching Claude Code hook JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertSessionRequest {
    pub session_id: String,
    pub cwd: String,
}

/// Request bodies use snake_case, matching Claude Code hook JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRequest {
    pub session_id: String,
    pub cwd: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub images: Vec<String>,
}

/// Request bodies use snake_case, matching Claude Code hook JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostRequest {
    pub session_id: String,
    pub cwd: String,
    pub html: String,
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
    pub cards: Vec<TurnCard>,
}
