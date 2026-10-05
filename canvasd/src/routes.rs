use std::convert::Infallible;
use std::path::Path as StdPath;
use std::process::Stdio;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use canvas_core::{
    Agent, AssignGlobalProfileRequest, AssignRepoProfileRequest, Card, EffectiveProfile,
    OpenRequest, PostRequest, ProfilesState, Session, SetProfileModeRequest, SetProfileTextRequest,
    StateResponse, UpdateCardRequest,
};
use futures::stream::Stream;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use uuid::Uuid;

use crate::repo::github_repo;
use crate::state::{AppState, CanvasEvent, SnapshotReply, Theme};

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn cwd_basename(cwd: &str) -> String {
    StdPath::new(cwd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| cwd.to_string())
}

/// Creates `session_id` with `name` set from `cwd`'s basename when the
/// daemon doesn't already know it. A session's first `/api/posts` is what
/// creates it; nothing registers a session at `SessionStart`. Returns the
/// new session so the caller can publish a `SessionUpserted` event; returns `None` when the session already existed,
/// since nothing about it changed. `repo` comes from
/// [`repo_if_unknown`], resolved before the caller took the lock.
fn ensure_session(
    inner: &mut crate::state::Inner,
    session_id: &str,
    cwd: &str,
    agent: Agent,
    repo: Option<String>,
    pid: Option<u32>,
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
        pid,
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
        state.end_session(&mut inner, &id)
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
    let repo = repo_if_unknown(&state, &req.session_id, &req.cwd).await;
    // One lock hold for both writes, so a concurrent DELETE of the session
    // can't land between creating it and adding its card.
    let card = {
        let mut inner = state.inner.write().await;
        let created_session = ensure_session(
            &mut inner,
            &req.session_id,
            &req.cwd,
            req.agent,
            repo,
            req.pid,
        );
        let id = Uuid::new_v4().to_string();
        let card = Card {
            html: resolve_image_placeholders(&req.html, &id),
            id,
            session_id: req.session_id.clone(),
            at: now(),
            updated_at: None,
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

/// Replaces an existing card's content in place — same id, session and `at`,
/// with `updated_at` set to now, which moves it to the top of the Timeline.
/// 404s rather than creating one: an id an agent doesn't already hold is
/// never a valid target, unlike `post_explicit`'s session id, which a
/// restarted daemon may legitimately not know yet.
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
            at: inner.cards[idx].at.clone(),
            updated_at: Some(now()),
            html: resolve_image_placeholders(&req.html, &id),
            images: req.images,
            targets: req.targets,
        };
        inner.upsert_card(card.clone());
        canvas_core::log::info(
            "card updated",
            &[
                ("id", &card.id),
                ("at", &card.at),
                ("updated_at", &card.touched_at()),
            ],
        );
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

/// `canvas data`: pushes one JSON value into a card's running script without
/// rebuilding its iframe. The viewer relays it as
/// `postMessage({type:'canvas-data', value})`; the latest value is kept (last
/// write wins, in memory) so the viewer can replay it into an iframe that
/// reloads. 404 when `id` names no card; 413 past `MAX_DATA_BYTES`.
pub const MAX_DATA_BYTES: usize = 256 * 1024;

pub async fn put_card_data(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(value): Json<serde_json::Value>,
) -> Response {
    let size = serde_json::to_vec(&value).map(|b| b.len()).unwrap_or(0);
    if size > MAX_DATA_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let mut inner = state.inner.write().await;
    if !inner.cards.iter().any(|c| c.id == id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    inner.data.insert(id.clone(), value.clone());
    state.publish(CanvasEvent::CardData { id, value });
    StatusCode::NO_CONTENT.into_response()
}

/// `canvas focus`: asks every open viewer to clear what hides the card,
/// scroll to it and ring it. Answers how many viewers the event reached, so
/// the CLI can fail when nobody saw it; 404 for an unknown card.
pub async fn focus_card(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !state.inner.read().await.cards.iter().any(|c| c.id == id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let viewers = state.publish(CanvasEvent::CardFocus(id.clone()));
    canvas_core::log::info("card focus", &[("card", &id), ("viewers", &viewers)]);
    Json(serde_json::json!({ "viewers": viewers })).into_response()
}

#[derive(serde::Deserialize)]
pub struct ThemeBody {
    theme: Theme,
}

/// `canvas theme <light|dark>`: asks every open viewer to switch theme the
/// way a click on its title-bar button does, so the choice persists. Answers
/// how many viewers the event reached, so the CLI can fail when nobody saw it.
pub async fn set_theme(State(state): State<AppState>, Json(body): Json<ThemeBody>) -> Response {
    let viewers = state.publish(CanvasEvent::ThemeSet(body.theme));
    canvas_core::log::info(
        "theme set",
        &[("theme", &body.theme.as_str()), ("viewers", &viewers)],
    );
    Json(serde_json::json!({ "viewers": viewers })).into_response()
}

/// The viewer reports the theme it shows, on load and after every change.
pub async fn report_theme(State(state): State<AppState>, Json(body): Json<ThemeBody>) -> Response {
    *state.viewer_theme.lock().unwrap_or_else(|e| e.into_inner()) = Some(body.theme);
    canvas_core::log::info("viewer theme", &[("theme", &body.theme.as_str())]);
    StatusCode::NO_CONTENT.into_response()
}

/// `canvas theme`: the theme a viewer last reported, `null` before any has or
/// while no viewer is connected, so a closed app never reads as showing one.
pub async fn get_theme(State(state): State<AppState>) -> Response {
    let reported = *state.viewer_theme.lock().unwrap_or_else(|e| e.into_inner());
    let theme = reported.filter(|_| state.events.receiver_count() > 0);
    Json(serde_json::json!({ "theme": theme })).into_response()
}

/// How long `snapshot_card` waits for a viewer to answer: the viewer waits
/// up to 3s for the card to settle, the app up to 5s each for the capture
/// and the upload.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(20);

/// The response header that says the PNG stops at the window's edge because
/// the card is taller than the window.
pub const CLIPPED_HEADER: &str = "x-canvas-clipped";

/// Removes a snapshot request from the waiting map however its handler ends,
/// including when the CLI hangs up and axum drops the handler mid-wait.
struct Waiting<'a> {
    state: &'a AppState,
    request: &'a str,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.state.snapshots.lock().unwrap().remove(self.request);
    }
}

/// `canvas snapshot`: asks the open viewers to capture the card as rendered
/// and answers the first PNG one sends back to `deliver_snapshot`, with
/// `x-canvas-clipped: true` when the card ran past the window. 409 when no
/// viewer is open, 422 with the viewer's reason when it could not capture the
/// card (filtered out, its session hidden), 504 when no viewer answered in time.
pub async fn snapshot_card(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if !state.inner.read().await.cards.iter().any(|c| c.id == id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    snapshot(&state, Snapshotted::Card, id).await
}

/// What a snapshot captures: a card in the stream, or an artifact's pane.
#[derive(Clone, Copy)]
pub enum Snapshotted {
    Card,
    Artifact,
}

impl Snapshotted {
    fn noun(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::Artifact => "artifact",
        }
    }
}

/// Publishes the capture request and waits for the first viewer's answer,
/// for `snapshot_card` and `snapshot_artifact` alike: the PNG, 409 when no
/// viewer is open, 422 with the viewer's reason, 504 when none answered.
pub async fn snapshot(state: &AppState, what: Snapshotted, id: String) -> Response {
    let request = Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    state.snapshots.lock().unwrap().insert(request.clone(), tx);
    let _waiting = Waiting {
        state,
        request: &request,
    };
    let (event_id, event_request) = (id.clone(), request.clone());
    let viewers = state.publish(match what {
        Snapshotted::Card => CanvasEvent::CardSnapshot {
            id: event_id,
            request: event_request,
        },
        Snapshotted::Artifact => CanvasEvent::ArtifactSnapshot {
            id: event_id,
            request: event_request,
        },
    });
    let noun = what.noun();
    canvas_core::log::info(
        "snapshot requested",
        &[(noun, &id), ("request", &request), ("viewers", &viewers)],
    );
    if viewers == 0 {
        return (
            StatusCode::CONFLICT,
            format!("no Canvas viewer is open to capture the {noun}"),
        )
            .into_response();
    }
    match tokio::time::timeout(SNAPSHOT_TIMEOUT, rx).await {
        Ok(Ok(SnapshotReply::Png { png, clipped })) => {
            canvas_core::log::info(
                "snapshot captured",
                &[
                    (noun, &id),
                    ("request", &request),
                    ("bytes", &png.len()),
                    ("clipped", &clipped),
                ],
            );
            let mut response = ([(header::CONTENT_TYPE, "image/png")], png).into_response();
            if clipped {
                response
                    .headers_mut()
                    .insert(CLIPPED_HEADER, header::HeaderValue::from_static("true"));
            }
            response
        }
        Ok(Ok(SnapshotReply::Failed(reason))) => {
            canvas_core::log::info(
                "snapshot refused",
                &[(noun, &id), ("request", &request), ("reason", &reason)],
            );
            (StatusCode::UNPROCESSABLE_ENTITY, reason).into_response()
        }
        _ => {
            canvas_core::log::warn("snapshot timed out", &[(noun, &id), ("request", &request)]);
            (
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "no Canvas viewer answered within {}s",
                    SNAPSHOT_TIMEOUT.as_secs()
                ),
            )
                .into_response()
        }
    }
}

/// A viewer's answer to a `card-snapshot` event: an `image/png` body (with
/// `x-canvas-clipped: true` when the card ran past the window), or
/// `{"error": "..."}` saying why it could not capture the card. 404 when no
/// request with that id is waiting (it timed out, or another viewer answered).
pub async fn deliver_snapshot(
    State(state): State<AppState>,
    Path(request): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let is_png = headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"image/png"));
    let reply = if is_png {
        SnapshotReply::Png {
            png: body.to_vec(),
            clipped: headers
                .get(CLIPPED_HEADER)
                .is_some_and(|v| v.as_bytes() == b"true"),
        }
    } else {
        let reason = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| "the viewer could not capture the card".to_string());
        SnapshotReply::Failed(reason)
    };
    let Some(tx) = state.snapshots.lock().unwrap().remove(&request) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let _ = tx.send(reply);
    StatusCode::NO_CONTENT.into_response()
}

/// The latest value `put_card_data` stored for this card; 404 until one has.
pub async fn get_card_data(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let inner = state.inner.read().await;
    match inner.data.get(&id) {
        Some(value) => (StatusCode::OK, Json(value.clone())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
        let removed = inner.cards.iter().any(|c| c.id == id);
        if removed {
            let event = CanvasEvent::CardRemoved(id);
            inner.apply(event.clone());
            state.publish(event);
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
    for card_id in removed {
        let event = CanvasEvent::CardRemoved(card_id);
        inner.apply(event.clone());
        state.publish(event);
    }
    StatusCode::NO_CONTENT.into_response()
}

pub async fn delete_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let removed = {
        let mut inner = state.inner.write().await;
        if !inner.sessions.contains_key(&id) {
            false
        } else {
            let event = CanvasEvent::SessionRemoved(id);
            inner.apply(event.clone());
            state.publish(event);
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
    let artifacts = state.artifacts.read().await.views();
    Json(StateResponse {
        sessions,
        cards,
        artifacts,
    })
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
        Ok(CanvasEvent::CardData { id, value }) => {
            Some(Ok(SseEvent::default().event("card-data").data(
                serde_json::to_string(&serde_json::json!({"id": id, "value": value}))
                    .unwrap_or_default(),
            )))
        }
        Ok(CanvasEvent::CardFocus(id)) => Some(Ok(SseEvent::default()
            .event("card-focus")
            .data(serde_json::to_string(&serde_json::json!({"id": id})).unwrap_or_default()))),
        Ok(CanvasEvent::CardSnapshot { id, request }) => {
            Some(Ok(SseEvent::default().event("card-snapshot").data(
                serde_json::to_string(&serde_json::json!({"id": id, "request": request}))
                    .unwrap_or_default(),
            )))
        }
        Ok(CanvasEvent::ThemeSet(theme)) => Some(Ok(SseEvent::default()
            .event("theme-set")
            .data(serde_json::json!({ "theme": theme }).to_string()))),
        Ok(CanvasEvent::ArtifactUpserted(artifact)) => Some(Ok(SseEvent::default()
            .event("artifact-upserted")
            .data(serde_json::to_string(&artifact).unwrap_or_default()))),
        Ok(CanvasEvent::ArtifactRemoved(id)) => Some(Ok(SseEvent::default()
            .event("artifact-removed")
            .data(serde_json::json!({ "id": id }).to_string()))),
        Ok(CanvasEvent::ArtifactData { id, value }) => Some(Ok(SseEvent::default()
            .event("artifact-data")
            .data(serde_json::json!({ "id": id, "value": value }).to_string()))),
        Ok(CanvasEvent::ArtifactFocus(id)) => Some(Ok(SseEvent::default()
            .event("artifact-focus")
            .data(serde_json::json!({ "id": id }).to_string()))),
        Ok(CanvasEvent::ArtifactSnapshot { id, request }) => Some(Ok(SseEvent::default()
            .event("artifact-snapshot")
            .data(serde_json::json!({ "id": id, "request": request }).to_string()))),
        Ok(CanvasEvent::ArtifactPane { id, action }) => {
            let mut data = serde_json::to_value(action).unwrap_or_default();
            data["id"] = serde_json::Value::String(id);
            Some(Ok(SseEvent::default()
                .event("artifact-pane")
                .data(data.to_string())))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

const ALLOWED_MEDIA_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", // images
    "mp4", "m4v", "mov", "webm", // video
];

/// Longest slice served for one request. A player asking for `bytes=0-` gets
/// this much and asks again for the rest, so a large clip is never read whole.
const MAX_RANGE_BYTES: u64 = 8 * 1024 * 1024;

/// What a `Range` header asks of a file of `len` bytes.
enum ByteRange {
    /// Inclusive `(start, end)`, already clamped to the file and `MAX_RANGE_BYTES`.
    Span(u64, u64),
    /// A well-formed single range that starts past the end: answer 416.
    Unsatisfiable,
    /// Not a single `bytes=` range: ignore the header and send the whole file.
    Ignore,
}

fn parse_byte_range(header: &str, len: u64) -> ByteRange {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return ByteRange::Ignore;
    };
    let Some((a, b)) = spec.split_once('-') else {
        return ByteRange::Ignore;
    };
    if spec.contains(',') {
        return ByteRange::Ignore;
    }
    let (start, end) = if a.is_empty() {
        match b.parse::<u64>() {
            Ok(n) if n > 0 && len > 0 => (len.saturating_sub(n), len - 1),
            Ok(_) => return ByteRange::Unsatisfiable,
            Err(_) => return ByteRange::Ignore,
        }
    } else {
        let Ok(start) = a.parse::<u64>() else {
            return ByteRange::Ignore;
        };
        let end = if b.is_empty() {
            u64::MAX
        } else {
            match b.parse::<u64>() {
                Ok(e) => e,
                Err(_) => return ByteRange::Ignore,
            }
        };
        if start > end {
            return ByteRange::Ignore;
        }
        (start, end)
    };
    if start >= len {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Span(start, end.min(len - 1).min(start + MAX_RANGE_BYTES - 1))
}

/// Serves the `index`th image or video of a stored card, honouring a single
/// `Range` request (a video element will not play without one). The request
/// names a card, not a path, so no caller can read a file no card shows — and
/// a card id is a random UUID another origin can't learn, since `/api/state`
/// and `/api/events` send no CORS headers.
pub async fn get_card_image(
    State(state): State<AppState>,
    Path((id, index)): Path<(String, usize)>,
    headers: HeaderMap,
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

    if !ALLOWED_MEDIA_EXTS.contains(&ext.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let content_type = (header::CONTENT_TYPE, mime.essence_str().to_string());
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(len) = file.metadata().await.map(|m| m.len()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let span = match range.map(|r| parse_byte_range(r, len)) {
        None | Some(ByteRange::Ignore) => None,
        Some(ByteRange::Span(start, end)) => Some((start, end)),
        Some(ByteRange::Unsatisfiable) => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{len}"))],
            )
                .into_response();
        }
    };
    let Some((start, end)) = span else {
        return match tokio::fs::read(path).await {
            Ok(bytes) => (
                StatusCode::OK,
                [content_type, (header::ACCEPT_RANGES, "bytes".to_string())],
                bytes,
            )
                .into_response(),
            Err(_) => StatusCode::NOT_FOUND.into_response(),
        };
    };
    let mut buf = vec![0u8; (end - start + 1) as usize];
    let read = async {
        file.seek(std::io::SeekFrom::Start(start)).await?;
        file.read_exact(&mut buf).await
    };
    if read.await.is_err() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        StatusCode::PARTIAL_CONTENT,
        [
            content_type,
            (header::ACCEPT_RANGES, "bytes".to_string()),
            (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
        ],
        buf,
    )
        .into_response()
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
