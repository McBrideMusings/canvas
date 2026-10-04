//! Shared data shapes for canvasd and its clients (canvas CLI, hooks, Tauri app).

use serde::{Deserialize, Serialize};

/// The coding agent a session belongs to. Serialized kebab-case
/// (`"claude-code"`, `"codex"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    /// Every session `stream.jsonl` holds from before this field existed
    /// came from Claude Code, so a stored session missing it reloads as one.
    #[default]
    ClaudeCode,
    Codex,
}

/// One main agent conversation on this Mac, from SessionStart to SessionEnd.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub cwd: String,
    /// Which agent started the session, set by whoever registered it.
    #[serde(default)]
    pub agent: Agent,
    /// cwd basename, computed on upsert.
    pub name: String,
    /// `owner/repo` of the cwd's `origin` remote when it points at
    /// github.com, read once when the daemon first learns of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// The agent process that owns the session, sent with its first post. The
    /// daemon ends the session when this process is gone; a session without
    /// one is never swept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// How long a pinned card lives: until its own session ends (`Session`), or
/// as long as anything in its repo keeps it (`Repo`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinScope {
    #[default]
    Session,
    Repo,
}

/// A card held in a slot: it shows as a widget on the pin shelf instead of in
/// the feed. The slot is unique within the poster's repo (its cwd when it has
/// none), so a post to a held slot replaces that card in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    pub slot: String,
    #[serde(default)]
    pub scope: PinScope,
    /// The small HTML the shelf renders for this card; absent means the shelf
    /// shows a plain tile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_html: Option<String>,
    /// A command the daemon runs to keep the card's data current; absent means
    /// nothing runs and the agent pushes with `canvas data --slot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Refresh>,
    /// The first stderr line (or the reason) of the latest failed refresh run;
    /// set by the daemon, cleared by the next success, ignored in a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_error: Option<String>,
}

/// The shortest `every_secs` a refresh may ask for.
pub const MIN_REFRESH_SECS: u64 = 5;

/// A shell command the daemon runs in the session's cwd every `every_secs`
/// while the pin exists and its session is live. It prints one JSON value to
/// stdout, which reaches the card the way `canvas data` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Refresh {
    pub command: String,
    pub every_secs: u64,
}

impl Card {
    /// The card holds a slot that outlives its session.
    pub fn is_repo_pinned(&self) -> bool {
        self.pin.as_ref().is_some_and(|p| p.scope == PinScope::Repo)
    }

    /// When the card last changed: `updated_at` once `post --update` has set
    /// it, else `at`. The Timeline orders by it and age pruning reads it.
    pub fn touched_at(&self) -> &str {
        self.updated_at.as_deref().unwrap_or(&self.at)
    }
}

/// One deliberate `canvas post`, shown as one card. Every post creates its
/// own card — there is no open/closed lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: String,
    pub session_id: String,
    /// When the card was first posted (RFC 3339).
    pub at: String,
    /// When `canvas post --update` last replaced it (RFC 3339); absent until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub html: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
}

/// Request bodies use snake_case, matching Claude Code hook JSON.
/// `images`/`targets` are empty until a later slice fills them in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostRequest {
    pub session_id: String,
    pub cwd: String,
    /// Recorded on the session only when this post creates it.
    pub agent: Agent,
    pub html: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    /// Holds the card in a slot; see [`Pin`].
    #[serde(default)]
    pub pin: Option<Pin>,
    /// Recorded on the session only when this post creates it.
    #[serde(default)]
    pub pid: Option<u32>,
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
    /// `"additive"` or `"replace"`.
    #[serde(default)]
    pub mode: ProfileMode,
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

/// How a repo's assignment combines with the global one: `Additive` sends the
/// base (global profile, else the built-in default) then the repo's profile;
/// `Replace` sends only the repo's profile, else the global one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileMode {
    #[default]
    Additive,
    Replace,
}

/// Body for `PUT /api/profiles/:kind/mode`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetProfileModeRequest {
    pub mode: ProfileMode,
}

/// What `GET /api/profiles/:kind/effective?cwd=<cwd>` reports: the text a
/// session in that repo receives (already joined per the kind's mode) and the
/// ordered sources it came from — profile names, with `"built-in"` for the
/// compiled-in default. Both are empty when nothing is assigned; a caller then
/// wants its own compiled-in default, and canvasd never ships one of its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveProfile {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
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
    /// Most recently changed first.
    #[serde(default)]
    pub artifacts: Vec<ArtifactView>,
}

/// Where an artifact's files live. Only `owned` exists yet: a folder canvasd
/// keeps under `artifacts/<id>/` in its data directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    #[default]
    Owned,
}

/// One artifact as canvasd persists it in `artifacts.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// `art-` followed by hex digits, so an id says it names an artifact.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: ArtifactKind,
    /// RFC 3339.
    pub created_at: String,
    /// When the record or its files last changed through canvasd (RFC 3339).
    pub updated_at: String,
}

/// The viewport a page asks for with `<meta name="canvas-size" content="WxH">`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSize {
    pub width: u32,
    pub height: u32,
}

/// An artifact plus what canvasd reads from its folder on each request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactView {
    #[serde(flatten)]
    pub artifact: Artifact,
    /// The artifact's folder, absolute.
    pub path: String,
    /// The page the pane opens: `index.html`, else the folder's only
    /// top-level HTML file. Absent while the folder has neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The entry page's declared `canvas-size`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ArtifactSize>,
}

/// Body for `POST /api/artifacts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewArtifactRequest {
    #[serde(default)]
    pub title: Option<String>,
}

/// Body for `POST /api/artifacts/:id/put`: an absolute path to a file, copied
/// into the folder under its own name, or a folder, whose contents are copied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutArtifactRequest {
    pub source: String,
}

pub mod log;
pub mod paths;
pub mod unix_http;
