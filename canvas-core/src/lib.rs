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

/// The shortest `every_secs` a refresh may ask for.
pub const MIN_REFRESH_SECS: u64 = 5;

/// A shell command canvasd runs every `every_secs` to keep an artifact's
/// widget and page current, as `artifact new|put --refresh` asks for it. It
/// prints one JSON value to stdout, which reaches the artifact the way
/// `canvas data` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Refresh {
    pub command: String,
    pub every_secs: u64,
}

/// A [`Refresh`] as an artifact record keeps it: also the directory the
/// command runs in (the asking shell's) and the agent process that asked.
/// canvasd runs it while that process is alive; with no pid (a person at a
/// shell) it runs until replaced or the artifact is deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRefresh {
    pub command: String,
    pub every_secs: u64,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// The latest failed refresh run of an artifact, until the next success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshError {
    /// The first stderr line, else the reason (timed out, not JSON, …).
    pub message: String,
    /// When the run failed (RFC 3339).
    pub at: String,
    /// When canvasd runs it next (RFC 3339).
    pub retry_at: String,
}

impl Card {
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
}

/// Body of `GET /api/cards/:id/export`: one post as a standalone HTML page,
/// plus everything that couldn't be baked into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportResult {
    pub html: String,
    pub warnings: Vec<ExportWarning>,
}

/// Something an export left out. It never fails the export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportWarning {
    pub kind: ExportWarningKind,
    /// The image path or URL the warning is about.
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExportWarningKind {
    /// An image or video file the card names is gone or unreadable.
    MissingImage,
    /// A CDN asset that couldn't be downloaded.
    FetchFailed,
}

/// The extensions `/api/cards/:id/images/:n` serves and an export inlines;
/// no other file is read for a card.
pub const MEDIA_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", // images
    "mp4", "m4v", "mov", "webm", // video
];

/// Standard base64 with padding, for `data:` URIs.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
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

/// Where an artifact's files live, written as `"kind"` on the record:
/// `owned` is a folder canvasd keeps under `artifacts/<id>/` in its data
/// directory; `linked` is an absolute folder or HTML file the person owns,
/// recorded as `link`, which canvasd watches and serves but never writes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ArtifactSource {
    Owned,
    Linked { link: String },
}

/// One artifact as canvasd persists it in `artifacts.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// `art-` followed by hex digits, so an id says it names an artifact.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub source: ArtifactSource,
    /// RFC 3339.
    pub created_at: String,
    /// When the record or its files last changed through canvasd (RFC 3339).
    pub updated_at: String,
    /// The small HTML the artifact's list row shows; absent means the row
    /// shows only its title and time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<ArtifactRefresh>,
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
    /// Where the files are, absolute: the owned folder, or the linked path.
    pub path: String,
    /// True when a linked artifact's path no longer exists.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_missing: bool,
    /// The page the pane opens: `index.html`, else the folder's only
    /// top-level HTML file, or the linked HTML file itself. Absent while
    /// there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The entry page's declared `canvas-size`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ArtifactSize>,
    /// The newest uncaught errors and unhandled rejections its page threw in
    /// a viewer's pane, oldest first, in memory only. Only `GET
    /// /api/artifacts/:id` (`canvas artifact show`) fills it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_errors: Vec<ArtifactScriptError>,
    /// The latest failed refresh run, until the next success; in memory only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_error: Option<RefreshError>,
    /// The latest value a refresh or `canvas data` pushed, in memory only;
    /// the viewer hands it to the widget and the page as `canvas-data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Which kind of page failure an [`ArtifactScriptError`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptErrorKind {
    /// An uncaught exception (`window` `error` event).
    Error,
    /// A promise rejection nothing handled (`unhandledrejection`).
    Rejection,
}

/// Body for `POST /api/artifacts/:id/errors`: one failure the viewer relays
/// from the artifact's pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptErrorReport {
    pub kind: ScriptErrorKind,
    pub message: String,
    /// The script URL the error came from, when the browser names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// One relayed page failure as `canvas artifact show` lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactScriptError {
    /// When canvasd received it (RFC 3339).
    pub at: String,
    #[serde(flatten)]
    pub report: ScriptErrorReport,
}

/// Body for `POST /api/artifacts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewArtifactRequest {
    #[serde(default)]
    pub title: Option<String>,
    /// An absolute folder or HTML file to link instead of making a folder.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(flatten)]
    pub extras: ArtifactExtras,
}

/// The widget and refresh `artifact new|put` may set. Each one given
/// replaces the artifact's own; each one absent leaves it as it was.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactExtras {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Refresh>,
    /// The directory the refresh runs in: the asking shell's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Body for `POST /api/artifacts/:id/relink`: the linked artifact's new
/// absolute folder or HTML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelinkArtifactRequest {
    pub link: String,
}

/// Body for `POST /api/artifacts/:id/put`: an absolute path to a file, copied
/// into the folder under its own name, or a folder, whose contents are copied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutArtifactRequest {
    pub source: String,
    #[serde(flatten)]
    pub extras: ArtifactExtras,
}

pub mod html;
pub mod log;
pub mod paths;
pub mod unix_http;
