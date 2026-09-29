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

/// What `GET /api/profiles/:kind` reports for one kind (today, only
/// `"posting-guidance"` ships), for the settings page: every named profile
/// defined for that kind, which one is assigned globally, and which repos
/// have their own assignment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesState {
    pub kind: String,
    #[serde(default)]
    pub profiles: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub repos: std::collections::HashMap<String, String>,
    /// The compiled-in default text a session falls back to when nothing is
    /// assigned, for kinds that have one — shown read-only alongside the
    /// named profiles so it's visible without leaving the settings page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<String>,
}

/// What `GET /api/profiles/:kind/effective?cwd=<cwd>` reports: only the
/// profile that applies for the repo — its own assignment, else the global
/// one — and that profile's text. Both are `None` when nothing is assigned;
/// a caller then wants its own compiled-in default, and canvasd never ships
/// one of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveProfile {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Body for `PUT /api/profiles/:kind/definitions`. `text: None` (or blank)
/// deletes the named profile — clearing it from every global/repo assignment
/// that pointed at it too, since an assignment naming a profile that no
/// longer exists would silently fall back to the compiled-in default anyway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetProfileTextRequest {
    pub name: String,
    pub text: Option<String>,
}

/// Body for `PUT /api/profiles/:kind/global`. `profile: None` clears the
/// global assignment and reverts every session with no repo override to its
/// own compiled-in default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignGlobalProfileRequest {
    pub profile: Option<String>,
}

/// Body for `PUT /api/profiles/:kind/repos`. `profile: None` clears that
/// repo's assignment, falling back to the global one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignRepoProfileRequest {
    pub repo: String,
    pub profile: Option<String>,
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
