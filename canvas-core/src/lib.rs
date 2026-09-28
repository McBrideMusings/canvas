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

/// What `GET /api/profiles/:kind?cwd=<cwd>` reports for one kind (e.g.
/// `"posting-guidance"`, `"dashboard-style"`): every named profile defined
/// for that kind, which one is assigned globally, which repo has its own
/// assignment, and — resolved for the `cwd`/`repo` the query named — which
/// profile and text actually apply. A caller with no effective profile wants
/// its own compiled-in default; canvasd never ships one of its own.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_profile: Option<String>,
}

impl ProfilesState {
    /// The text a session should actually see: the profile assigned to its
    /// repo, else the one assigned globally, else `None` (fall back to the
    /// caller's own compiled-in default).
    pub fn effective_text(&self) -> Option<&str> {
        self.effective_profile
            .as_deref()
            .and_then(|name| self.profiles.get(name))
            .map(String::as_str)
    }
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
