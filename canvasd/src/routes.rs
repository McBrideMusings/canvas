use std::convert::Infallible;
use std::path::Path as StdPath;
use std::process::Stdio;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use canvas_core::{
    OpenRequest, PostRequest, Session, StateResponse, TurnCard, TurnRequest, UpsertSessionRequest,
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

/// Same-origin check for the delete endpoints.
///
/// A request with no `Origin` header (curl, the CLI) is allowed — only a
/// browser sets `Origin`, and canvasd never knows the port it's bound to
/// ahead of time in a way that's easy to plumb into every handler. Instead
/// of comparing against a configured port, this compares the `Origin`
/// header's host and port against the request's own `Host` header: the
/// `Host` header is always whichever of `127.0.0.1` or `localhost` the
/// client actually connected to, at the actual bound port, so matching the
/// `Origin`'s port against it is equivalent to checking against the real
/// port without canvasd ever needing to know it. The `Origin`'s hostname
/// still has to be `127.0.0.1` or `localhost` — a cross-origin page loaded
/// from a real domain but proxied so its `Host` header matches must not
/// pass just because the ports line up.
fn is_same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };

    let Ok(origin_uri) = origin.parse::<Uri>() else {
        return false;
    };
    if origin_uri.scheme_str() != Some("http") {
        return false;
    }
    let Some(origin_host) = origin_uri.host() else {
        return false;
    };
    if origin_host != "127.0.0.1" && origin_host != "localhost" {
        return false;
    }

    let Some(host_header) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host_port = host_header.rsplit_once(':').map(|(_, port)| port);
    let origin_port = origin_uri.port_u16().map(|p| p.to_string());

    match (origin_port.as_deref(), host_port) {
        (Some(op), Some(hp)) => op == hp,
        _ => false,
    }
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
        // already-ended session.
        let (started_at, ended_at) = match inner.sessions.get(&req.session_id) {
            Some(existing) => (existing.started_at.clone(), existing.ended_at.clone()),
            None => (now(), None),
        };
        let session = Session {
            id: req.session_id.clone(),
            cwd: req.cwd.clone(),
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
/// depend on is gone, so `/api/turns` and `/api/posts` can arrive for a
/// session that never got a `SessionStart` (e.g. the daemon restarted after
/// the session began). Returns the new session so the caller can publish a
/// `SessionUpserted` event; returns `None` when the session already existed,
/// since nothing about it changed. `repo` comes from
/// [`repo_if_unknown`], resolved before the caller took the lock.
fn ensure_session(
    inner: &mut crate::state::Inner,
    session_id: &str,
    cwd: &str,
    repo: Option<String>,
) -> Option<Session> {
    if inner.sessions.contains_key(session_id) {
        return None;
    }
    let session = Session {
        id: session_id.to_string(),
        cwd: cwd.to_string(),
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

pub async fn post_turn(
    State(state): State<AppState>,
    Json(req): Json<TurnRequest>,
) -> impl IntoResponse {
    let all_empty = req.links.is_empty() && req.paths.is_empty() && req.images.is_empty();

    // One lock hold for both writes, so a concurrent DELETE of the session
    // can't land between creating it and adding its card.
    let repo = repo_if_unknown(&state, &req.session_id, &req.cwd).await;
    let card = {
        let mut inner = state.inner.write().await;
        let created_session = ensure_session(&mut inner, &req.session_id, &req.cwd, repo);
        let card = match inner.open_card_index(&req.session_id) {
            Some(idx) => {
                let card = &mut inner.cards[idx];
                card.links.extend(req.links);
                card.paths.extend(req.paths);
                card.images.extend(req.images);
                card.open = false;
                card.at = now();
                Some(card.clone())
            }
            None => {
                if all_empty {
                    None
                } else {
                    let card = TurnCard {
                        id: Uuid::new_v4().to_string(),
                        session_id: req.session_id.clone(),
                        at: now(),
                        html: Vec::new(),
                        links: req.links,
                        paths: req.paths,
                        images: req.images,
                        open: false,
                    };
                    inner.push_card(card.clone());
                    Some(card)
                }
            }
        };
        // Published under the lock, so the persisted log records changes in
        // the order they were applied.
        if let Some(session) = created_session {
            state.publish(CanvasEvent::SessionUpserted(session));
        }
        if let Some(card) = &card {
            state.publish(CanvasEvent::CardUpserted(card.clone()));
        }
        card
    };

    match card {
        Some(card) => (StatusCode::OK, Json(Some(card))).into_response(),
        None => (StatusCode::OK, Json(Option::<TurnCard>::None)).into_response(),
    }
}

pub async fn post_explicit(
    State(state): State<AppState>,
    Json(req): Json<PostRequest>,
) -> impl IntoResponse {
    // One lock hold for both writes, so a concurrent DELETE of the session
    // can't land between creating it and adding its card.
    let repo = repo_if_unknown(&state, &req.session_id, &req.cwd).await;
    let card = {
        let mut inner = state.inner.write().await;
        let created_session = ensure_session(&mut inner, &req.session_id, &req.cwd, repo);
        let card = match inner.open_card_index(&req.session_id) {
            Some(idx) => {
                let card = &mut inner.cards[idx];
                card.html.push(req.html.clone());
                card.at = now();
                card.clone()
            }
            None => {
                let card = TurnCard {
                    id: Uuid::new_v4().to_string(),
                    session_id: req.session_id.clone(),
                    at: now(),
                    html: vec![req.html.clone()],
                    links: Vec::new(),
                    paths: Vec::new(),
                    images: Vec::new(),
                    open: true,
                };
                inner.push_card(card.clone());
                card
            }
        };
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

pub async fn delete_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !is_same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }

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

pub async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !is_same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }

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
    let cards: Vec<TurnCard> = inner.cards.iter().cloned().collect();
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
