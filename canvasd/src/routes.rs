use std::convert::Infallible;
use std::path::Path as StdPath;
use std::process::Stdio;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use canvas_core::{
    Agent, AssignGlobalProfileRequest, AssignRepoProfileRequest, Card, EffectiveProfile,
    OpenRequest, PostRequest, ProfilesState, Session, SetProfileModeRequest, SetProfileTextRequest,
    StateResponse, UpdateCardRequest, UpsertSessionRequest,
};
use futures::stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use uuid::Uuid;

use crate::repo::github_repo;
use crate::state::{AppState, CanvasEvent};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn cwd_basename(cwd: &str) -> String {
    StdPath::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.to_string())
}

pub async fn upsert_session(
    State(state): State<AppState>,
    Json(req): Json<UpsertSessionRequest>,
) -> impl IntoResponse {
    // Read before taking the lock: it runs `git`, and nothing else should
    // wait on that.
    let repo = github_repo(&req.cwd).await;
    let session = {
        let mut inner = state.inner.write().await;
        // A genuine upsert: a retried or re-fired registration for an id that
        // already exists must not reset when it started or resurrect an
        // already-ended session, or change which agent started it.
        let (started_at, ended_at, agent) = match inner.sessions.get(&req.session_id) {
            Some(existing) => (
                existing.started_at.clone(),
                existing.ended_at.clone(),
                existing.agent,
            ),
            None => (now(), None, req.agent),
        };
        let session = Session {
            id: req.session_id.clone(),
            cwd: req.cwd.clone(),
            agent,
            name: cwd_basename(&req.cwd),
            repo,
            started_at,
            ended_at,
        };
        inner.sessions.insert(session.id.clone(), session.clone());
        state.publish(CanvasEvent::SessionUpserted(session.clone()));
        session
    };

    (StatusCode::OK, Json(session))
}

/// Creates `session_id` with `name` set from `cwd`'s basename when the
/// daemon doesn't already know it — the pid → session lookup this used to
/// depend on is gone, so `/api/posts` can arrive for a session that never
/// got a `SessionStart` (e.g. the daemon restarted after the session
/// began). Returns the new session so the caller can publish a
/// `SessionUpserted` event; returns `None` when the session already existed,
/// since nothing about it changed. `repo` comes from
/// [`repo_if_unknown`], resolved before the caller took the lock.
fn ensure_session(
    inner: &mut crate::state::Inner,
    session_id: &str,
    cwd: &str,
    agent: Agent,
    repo: Option<String>,
) -> Option<Session> {
    if inner.sessions.contains_key(session_id) {
        return None;
    }
    let session = Session {
        id: session_id.to_string(),
        cwd: cwd.to_string(),
        agent,
        name: cwd_basename(cwd),
        repo,
        started_at: now(),
        ended_at: None,
    };
    inner.sessions.insert(session.id.clone(), session.clone());
    Some(session)
}

/// The repo for a session `ensure_session` may be about to create, read
/// only when the daemon doesn't know the session yet, so an ordinary turn
/// never runs `git`.
async fn repo_if_unknown(state: &AppState, session_id: &str, cwd: &str) -> Option<String> {
    if state.inner.read().await.sessions.contains_key(session_id) {
        return None;
    }
    github_repo(cwd).await
}

pub async fn end_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let updated = {
        let mut inner = state.inner.write().await;
        if let Some(session) = inner.sessions.get_mut(&id) {
            session.ended_at = Some(now());
            let session = session.clone();
            state.publish(CanvasEvent::SessionUpserted(session.clone()));
            Some(session)
        } else {
            None
        }
    };

    match updated {
        Some(session) => (StatusCode::OK, Json(Some(session))).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Replaces every `canvas-image:<n>` placeholder the CLI's scan step left in
/// `html` with the image route for this card, now that the card's id is
/// known. Only a placeholder that occupies a whole quoted attribute value —
/// `="canvas-image:<n>"` or `='canvas-image:<n>'`, exactly the shape scan.rs
/// writes — is rewritten, so ordinary prose that happens to contain the same
/// literal text (e.g. a post discussing this very mechanism) is left alone.
/// `n` is unbounded here (no check against how many images the post actually
/// carries) — an out-of-range index just serves 404 at request time, same as
/// any other missing image.
fn resolve_image_placeholders(html: &str, card_id: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        match rest.find("canvas-image:") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(idx) => {
                out.push_str(&rest[..idx]);
                let after = &rest[idx + "canvas-image:".len()..];
                let digits_len = after.bytes().take_while(|b| b.is_ascii_digit()).count();
                let opening_quote = idx
                    .checked_sub(1)
                    .and_then(|i| rest.as_bytes().get(i))
                    .copied()
                    .filter(|&b| b == b'"' || b == b'\'');
                let preceded_by_equals = idx
                    .checked_sub(2)
                    .and_then(|i| rest.as_bytes().get(i))
                    .is_some_and(|&b| b == b'=');
                let closing_quote_matches =
                    opening_quote.is_some_and(|q| after.as_bytes().get(digits_len) == Some(&q));

                if digits_len == 0 || !preceded_by_equals || !closing_quote_matches {
                    // Not a genuine `="canvas-image:<n>"` attribute value —
                    // copy the literal text through unchanged and keep
                    // scanning past it.
                    out.push_str("canvas-image:");
                    rest = after;
                    continue;
                }
                let n = &after[..digits_len];
                out.push_str(&format!("/api/cards/{card_id}/images/{n}"));
                rest = &after[digits_len..];
            }
        }
    }
    out
}

/// Every post creates its own card — no open/closed lifecycle, no merging
/// into a prior card from the same session.
pub async fn post_explicit(
    State(state): State<AppState>,
    Json(req): Json<PostRequest>,
) -> impl IntoResponse {
    // One lock hold for both writes, so a concurrent DELETE of the session
    // can't land between creating it and adding its card.
    let repo = repo_if_unknown(&state, &req.session_id, &req.cwd).await;
    let card = {
        let mut inner = state.inner.write().await;
        let created_session =
            ensure_session(&mut inner, &req.session_id, &req.cwd, req.agent, repo);
        let id = Uuid::new_v4().to_string();
        let card = Card {
            html: resolve_image_placeholders(&req.html, &id),
            id,
            session_id: req.session_id.clone(),
            at: now(),
            images: req.images,
            targets: req.targets,
        };
        inner.push_card(card.clone());
        // Published under the lock, so the persisted log records changes in
        // the order they were applied.
        if let Some(session) = created_session {
            state.publish(CanvasEvent::SessionUpserted(session));
        }
        state.publish(CanvasEvent::CardUpserted(card.clone()));
        card
    };

    (StatusCode::OK, Json(card)).into_response()
}

/// One card as the daemon holds it; 404s when the id names none.
pub async fn get_card(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let inner = state.inner.read().await;
    match inner.cards.iter().find(|c| c.id == id) {
        Some(card) => Json(card.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Replaces an existing card's content in place — same id and session, a
/// fresh `at` (it's the card's last-touched time, same sense `prune_before`
/// uses it in). 404s rather than creating one: an id an agent doesn't
/// already hold is never a valid target, unlike `post_explicit`'s session
/// id, which a restarted daemon may legitimately not know yet.
pub async fn update_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateCardRequest>,
) -> Response {
    let updated = {
        let mut inner = state.inner.write().await;
        let Some(idx) = inner.cards.iter().position(|c| c.id == id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let card = Card {
            id: id.clone(),
            session_id: inner.cards[idx].session_id.clone(),
            at: now(),
            html: resolve_image_placeholders(&req.html, &id),
            images: req.images,
            targets: req.targets,
        };
        inner.upsert_card(card.clone());
        state.publish(CanvasEvent::CardUpserted(card.clone()));
        card
    };

    (StatusCode::OK, Json(updated)).into_response()
}

/// A card's own script cannot reach this route at all — its iframe has no
/// `connect-src` and no `allow-forms` — so the only caller is the viewer,
/// which relays a `postMessage({type:'canvas-reply', value})` it received
/// from that card's iframe, identifying the card by `event.source` rather
/// than trusting an id the message could name. Last write wins, in memory
/// only — no history, gone on restart, same as the rest of a reply's
/// lifetime being tied to the session that's waiting on it. 404s when `id`
/// names no card, same as `update_card`; 413 when the value serializes
/// larger than `MAX_REPLY_BYTES` — a reply is an answer, not a file upload.
const MAX_REPLY_BYTES: usize = 4096;

pub async fn post_card_reply(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    let size = serde_json::to_vec(&value).map(|b| b.len()).unwrap_or(0);
    if size > MAX_REPLY_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let mut inner = state.inner.write().await;
    if !inner.cards.iter().any(|c| c.id == id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    inner.replies.insert(id, value);
    StatusCode::NO_CONTENT.into_response()
}

/// Polled by `canvas wait` and `canvas replies`. 200 with the stored value
/// once `post_card_reply` has been called for this id, 404 until then.
pub async fn get_card_reply(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let inner = state.inner.read().await;
    match inner.replies.get(&id) {
        Some(value) => (StatusCode::OK, Json(value.clone())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn delete_card(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = {
        let mut inner = state.inner.write().await;
        let before = inner.cards.len();
        inner.cards.retain(|c| c.id != id);
        let removed = inner.cards.len() != before;
        if removed {
            state.publish(CanvasEvent::CardRemoved(id));
        }
        removed
    };

    if !removed {
        return StatusCode::NOT_FOUND.into_response();
    }
    StatusCode::OK.into_response()
}

/// Removes every card of a session and leaves the session registered. One
/// `card-removed` event per card, so viewers and the persisted log drop them
/// the same way a single delete does.
pub async fn clear_session_cards(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut inner = state.inner.write().await;
    if !inner.sessions.contains_key(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let removed: Vec<String> = inner
        .cards
        .iter()
        .filter(|c| c.session_id == id)
        .map(|c| c.id.clone())
        .collect();
    inner.cards.retain(|c| c.session_id != id);
    for card_id in &removed {
        state.publish(CanvasEvent::CardRemoved(card_id.clone()));
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = {
        let mut inner = state.inner.write().await;
        if inner.sessions.remove(&id).is_none() {
            false
        } else {
            inner.cards.retain(|c| c.session_id != id);
            state.publish(CanvasEvent::SessionRemoved(id));
            true
        }
    };

    if !removed {
        return StatusCode::NOT_FOUND.into_response();
    }
    StatusCode::OK.into_response()
}

pub async fn get_state(State(state): State<AppState>) -> impl IntoResponse {
    let inner = state.inner.read().await;
    let sessions: Vec<Session> = inner.sessions.values().cloned().collect();
    let cards: Vec<Card> = inner.cards.iter().cloned().collect();
    Json(StateResponse { sessions, cards })
}

pub async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(CanvasEvent::CardUpserted(card)) => Some(Ok(SseEvent::default()
            .event("card-upserted")
            .data(serde_json::to_string(&card).unwrap_or_default()))),
        Ok(CanvasEvent::SessionUpserted(session)) => Some(Ok(SseEvent::default()
            .event("session-upserted")
            .data(serde_json::to_string(&session).unwrap_or_default()))),
        Ok(CanvasEvent::CardRemoved(id)) => Some(Ok(SseEvent::default()
            .event("card-removed")
            .data(serde_json::to_string(&serde_json::json!({"id": id})).unwrap_or_default()))),
        Ok(CanvasEvent::SessionRemoved(id)) => Some(Ok(SseEvent::default()
            .event("session-removed")
            .data(serde_json::to_string(&serde_json::json!({"id": id})).unwrap_or_default()))),
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

const ALLOWED_IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// Serves the `index`th image of a stored card. The request names a card, not
/// a path, so no caller can read a file no card shows — and a card id is a
/// random UUID another origin can't learn, since `/api/state` and
/// `/api/events` send no CORS headers.
pub async fn get_card_image(
    State(state): State<AppState>,
    Path((id, index)): Path<(String, usize)>,
) -> Response {
    let image = {
        let inner = state.inner.read().await;
        inner
            .cards
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.images.get(index).cloned())
    };
    let Some(image) = image else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !image.starts_with('/') {
        return StatusCode::NOT_FOUND.into_response();
    }

    let path = StdPath::new(&image);
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if !ALLOWED_IMAGE_EXTS.contains(&ext.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.essence_str().to_string())],
                bytes,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The settings page's read: every named profile for `kind`, the global
/// assignment and each repo's assignment. Sessions never call this — a
/// growing set of profile texts has no business on the hook's 1s path; they
/// use `get_effective_profile`.
pub async fn get_profiles(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> impl IntoResponse {
    let config = state.profiles.read().await;
    let set = config.kind(&kind);
    let builtin = crate::profiles::builtin_default(&kind).map(str::to_string);
    Json(ProfilesState {
        kind,
        mode: set.mode,
        profiles: set.profiles,
        global: set.global,
        repos: set.repos,
        builtin,
    })
}

/// `cwd` names the directory a session runs in (resolved to its GitHub repo);
/// `repo` names the repo directly and wins when both are given.
#[derive(serde::Deserialize)]
pub struct EffectiveProfileQuery {
    cwd: Option<String>,
    repo: Option<String>,
}

/// The session/CLI read: the text a session in the repo receives, joined per
/// the kind's mode, and the sources it came from. Both are absent when nothing
/// is assigned, and the caller falls back to its own compiled-in default.
pub async fn get_effective_profile(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Query(params): Query<EffectiveProfileQuery>,
) -> impl IntoResponse {
    let repo = match params.repo {
        Some(repo) => Some(repo),
        None => match params.cwd {
            Some(cwd) => github_repo(&cwd).await,
            None => None,
        },
    };
    let config = state.profiles.read().await;
    let set = config.kind(&kind);
    let composed = set.compose(&kind, repo.as_deref());
    Json(EffectiveProfile {
        kind,
        profiles: composed
            .as_ref()
            .map(|c| c.sources.clone())
            .unwrap_or_default(),
        text: composed.map(|c| c.text),
    })
}

/// An assignment naming a profile that isn't in that kind's set would
/// silently fall back to the compiled-in default with no sign why — reject
/// it instead.
fn unknown_profile_response(name: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        format!("no profile named {name:?}"),
    )
        .into_response()
}

/// The settings page and `canvas profile` write a kind's profiles and
/// assignments through these three routes.
pub async fn set_profile_text(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(req): Json<SetProfileTextRequest>,
) -> Response {
    // A stop-triggers profile the hook can't parse would silently stop
    // asking for posts, so refuse it here with the offending line.
    if kind == crate::profiles::KIND_STOP_TRIGGERS {
        if let Some(text) = req.text.as_deref().filter(|t| !t.trim().is_empty()) {
            if let Err(message) = crate::stop_triggers::parse(text) {
                return (StatusCode::BAD_REQUEST, message).into_response();
            }
        }
    }
    {
        let mut config = state.profiles.write().await;
        config.set_profile_text(&kind, &req.name, req.text);
    }
    state.save_profiles().await;
    StatusCode::OK.into_response()
}

pub async fn set_profile_mode(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(req): Json<SetProfileModeRequest>,
) -> Response {
    {
        let mut config = state.profiles.write().await;
        config.set_mode(&kind, req.mode);
    }
    state.save_profiles().await;
    StatusCode::OK.into_response()
}

pub async fn set_global_profile(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(req): Json<AssignGlobalProfileRequest>,
) -> Response {
    {
        let mut config = state.profiles.write().await;
        if let Some(name) = &req.profile {
            if !config.kind(&kind).profiles.contains_key(name) {
                return unknown_profile_response(name);
            }
        }
        config.set_global(&kind, req.profile);
    }
    state.save_profiles().await;
    StatusCode::OK.into_response()
}

pub async fn set_repo_profile(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(req): Json<AssignRepoProfileRequest>,
) -> Response {
    {
        let mut config = state.profiles.write().await;
        if let Some(name) = &req.profile {
            if !config.kind(&kind).profiles.contains_key(name) {
                return unknown_profile_response(name);
            }
        }
        config.set_repo(&kind, &req.repo, req.profile);
    }
    state.save_profiles().await;
    StatusCode::OK.into_response()
}

fn is_openable(path: &str) -> bool {
    // Absolute filesystem paths, or http(s) URLs (macOS `open` handles both) —
    // link rows send a URL, path rows send an absolute path. Deliberately
    // excludes `file://` and other custom schemes, which `open` would also
    // dispatch to arbitrary local files or registered apps.
    path.starts_with('/') || path.starts_with("http://") || path.starts_with("https://")
}

pub async fn open_path(Json(req): Json<OpenRequest>) -> Response {
    if !is_openable(&req.path) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    match tokio::process::Command::new("open")
        .arg(&req.path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
    {
        Ok(status) if status.success() => StatusCode::OK.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
