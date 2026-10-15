//! The upstream link (ADR-0006): a daemon whose `sharing.toml` names an
//! `upstream` holds that canvasd's `/api/events` open and merges its sessions,
//! cards and artifacts into its own `/api/state` and event stream, so
//! Canvas.app reads one source. The merged items live in [`Remote`], in
//! memory only, never in `stream.jsonl` or `artifacts.json`; they are dropped
//! when the link is lost and refilled from the upstream's snapshot when it
//! returns. Remote items are read-only here: a route that changes an item
//! finds only this machine's own.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::time::Duration;

use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use canvas_core::{ArtifactView, Card, Session, StateResponse, UpstreamStatus};

use crate::state::{AppState, CanvasEvent, CARD_RING_CAPACITY};

/// The longest an event stream may go without a byte before the link counts
/// as lost; the upstream sends a keep-alive every 15s.
const STREAM_IDLE: Duration = Duration::from_secs(45);
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// The most bytes one proxied file may carry.
const PROXY_MAX_BYTES: u64 = 64 << 20;

/// What the upstream holds, as last heard.
#[derive(Default)]
pub struct Remote {
    pub sessions: HashMap<String, Session>,
    /// Newest first.
    pub cards: VecDeque<Card>,
    pub artifacts: HashMap<String, ArtifactView>,
    /// The latest `card-data` value per card.
    pub data: HashMap<String, serde_json::Value>,
    /// `None` when `sharing.toml` names no upstream.
    pub status: Option<UpstreamStatus>,
}

impl Remote {
    pub fn card(&self, id: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.id == id)
    }

    fn clear(&mut self) {
        self.sessions.clear();
        self.cards.clear();
        self.artifacts.clear();
        self.data.clear();
    }

    fn upsert_card(&mut self, card: Card) {
        self.cards.retain(|c| c.id != card.id);
        self.cards.push_front(card);
        while self.cards.len() > CARD_RING_CAPACITY {
            if let Some(evicted) = self.cards.pop_back() {
                self.data.remove(&evicted.id);
            }
        }
    }
}

/// The host part of an upstream URL, which names the upstream's machine when
/// it sent no name of its own.
pub fn host_label(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(v6),
        None => host.split(':').next().unwrap_or(host),
    };
    host.to_string()
}

/// A card as this machine shows an upstream's: its media paths name files on
/// the upstream's disk, so each becomes `upstream:<file name>`, which keeps
/// the extension the viewer reads and is never an absolute path the app's
/// asset scope could be asked to open. The image route fetches the bytes
/// from the upstream.
fn remote_card(mut card: Card) -> Card {
    for image in card.images.iter_mut() {
        let name = image.rsplit('/').next().unwrap_or("");
        *image = format!("upstream:{name}");
    }
    card
}

fn remote_session(mut session: Session, label: &str) -> Session {
    session.machine.get_or_insert_with(|| label.to_string());
    // The pid is the upstream machine's.
    session.pid = None;
    session
}

fn remote_artifact(mut view: ArtifactView, label: &str) -> ArtifactView {
    view.machine.get_or_insert_with(|| label.to_string());
    // Paths and refresh commands belong to the machine that holds them.
    view.path = String::new();
    view.watched = false;
    view
}

impl AppState {
    /// Whether this machine holds `session_id` itself. An upstream's copy of
    /// such a session is a post this daemon forwarded, and is not shown twice.
    async fn holds_session(&self, session_id: &str) -> bool {
        self.inner.read().await.sessions.contains_key(session_id)
    }

    async fn set_upstream_status(&self, online: bool, error: Option<String>) {
        let status = {
            let mut remote = self.remote.write().await;
            let Some(status) = remote.status.as_mut() else {
                return;
            };
            // A retry that fails the way the last one did changes nothing.
            if status.online == online && status.error == error {
                return;
            }
            status.online = online;
            status.error = error;
            status.clone()
        };
        self.send_to_viewers(CanvasEvent::UpstreamChanged(status));
    }

    /// Sends `event` to this machine's viewers without persisting it.
    fn send_to_viewers(&self, event: CanvasEvent) {
        let _ = self.remote_events.send(event);
    }

    /// Replaces everything held from the upstream with its snapshot and tells
    /// viewers to reload, since events missed while the link was down can't be
    /// named.
    async fn load_remote_snapshot(&self, snapshot: StateResponse, label: &str) {
        {
            let mut remote = self.remote.write().await;
            remote.clear();
            for session in snapshot.sessions {
                remote
                    .sessions
                    .insert(session.id.clone(), remote_session(session, label));
            }
            for card in snapshot.cards {
                remote.cards.push_back(remote_card(card));
            }
            for view in snapshot.artifacts {
                remote
                    .artifacts
                    .insert(view.artifact.id.clone(), remote_artifact(view, label));
            }
        }
        self.set_upstream_status(true, None).await;
    }

    /// Drops everything held from the upstream and shows it offline.
    async fn drop_remote(&self, error: String) {
        self.remote.write().await.clear();
        self.set_upstream_status(false, Some(error)).await;
    }

    /// Applies one server-sent event from the upstream and passes it on to
    /// viewers. Only the events that change what the upstream holds are taken;
    /// the rest (focus, snapshot, theme, pane, reset) are that machine's
    /// viewers' commands and never move this window.
    async fn apply_remote_event(&self, name: &str, data: &str, label: &str) {
        let parse = |what: &str| -> Option<serde_json::Value> {
            serde_json::from_str(data)
                .map_err(|e| {
                    canvas_core::log::warn(
                        "upstream event unreadable",
                        &[("event", &name), ("what", &what), ("error", &e)],
                    )
                })
                .ok()
        };
        let id_of = |v: &serde_json::Value| v.get("id").and_then(|i| i.as_str()).map(String::from);
        match name {
            "session-upserted" => {
                let Ok(session) = serde_json::from_str::<Session>(data) else {
                    return;
                };
                if self.holds_session(&session.id).await {
                    return;
                }
                let session = remote_session(session, label);
                self.remote
                    .write()
                    .await
                    .sessions
                    .insert(session.id.clone(), session.clone());
                self.send_to_viewers(CanvasEvent::SessionUpserted(session));
            }
            "card-upserted" => {
                let Ok(card) = serde_json::from_str::<Card>(data) else {
                    return;
                };
                if self.holds_session(&card.session_id).await {
                    return;
                }
                let card = remote_card(card);
                self.remote.write().await.upsert_card(card.clone());
                self.send_to_viewers(CanvasEvent::CardUpserted(card));
            }
            "card-removed" => {
                let Some(id) = parse("card-removed").as_ref().and_then(id_of) else {
                    return;
                };
                let mut remote = self.remote.write().await;
                let held = remote.card(&id).is_some();
                remote.cards.retain(|c| c.id != id);
                remote.data.remove(&id);
                drop(remote);
                if held {
                    self.send_to_viewers(CanvasEvent::CardRemoved(id));
                }
            }
            "session-removed" => {
                let Some(id) = parse("session-removed").as_ref().and_then(id_of) else {
                    return;
                };
                let mut remote = self.remote.write().await;
                let held = remote.sessions.remove(&id).is_some();
                remote.cards.retain(|c| c.session_id != id);
                drop(remote);
                if held {
                    self.send_to_viewers(CanvasEvent::SessionRemoved(id));
                }
            }
            "card-data" => {
                let Some(v) = parse("card-data") else { return };
                let (Some(id), Some(value)) = (id_of(&v), v.get("value").cloned()) else {
                    return;
                };
                let mut remote = self.remote.write().await;
                if remote.card(&id).is_none() {
                    return;
                }
                remote.data.insert(id.clone(), value.clone());
                drop(remote);
                self.send_to_viewers(CanvasEvent::CardData { id, value });
            }
            "artifact-upserted" => {
                let Ok(view) = serde_json::from_str::<ArtifactView>(data) else {
                    return;
                };
                if self
                    .artifacts
                    .read()
                    .await
                    .records
                    .contains_key(&view.artifact.id)
                {
                    return;
                }
                let view = remote_artifact(view, label);
                self.remote
                    .write()
                    .await
                    .artifacts
                    .insert(view.artifact.id.clone(), view.clone());
                self.send_to_viewers(CanvasEvent::ArtifactUpserted(Box::new(view)));
            }
            "artifact-removed" => {
                let Some(id) = parse("artifact-removed").as_ref().and_then(id_of) else {
                    return;
                };
                if self.remote.write().await.artifacts.remove(&id).is_some() {
                    self.send_to_viewers(CanvasEvent::ArtifactRemoved(id));
                }
            }
            "artifact-data" => {
                let Some(v) = parse("artifact-data") else {
                    return;
                };
                let (Some(id), Some(value)) = (id_of(&v), v.get("value").cloned()) else {
                    return;
                };
                let mut remote = self.remote.write().await;
                let Some(view) = remote.artifacts.get_mut(&id) else {
                    return;
                };
                view.data = Some(value.clone());
                view.data_at = Some(chrono::Utc::now().to_rfc3339());
                drop(remote);
                self.send_to_viewers(CanvasEvent::ArtifactData { id, value });
            }
            _ => {}
        }
    }
}

fn agent(read_timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(read_timeout)
        .build()
}

/// One connection to the upstream: opens its event stream, reads its
/// snapshot, then applies events until the stream ends or errors. The stream
/// opens before the snapshot, so no change between the two is missed.
/// Returns the reason the link ended. Blocking; `handle` runs the async
/// state changes.
fn run_link(state: &AppState, handle: &tokio::runtime::Handle, base: &str, label: &str) -> String {
    let events = match agent(STREAM_IDLE)
        .get(&format!("{base}/api/events"))
        .set("accept", "text/event-stream")
        .call()
    {
        Ok(response) => response,
        Err(e) => return format!("events: {e}"),
    };
    let snapshot: StateResponse = match agent(Duration::from_secs(30))
        .get(&format!("{base}/api/state"))
        .call()
        .map_err(|e| e.to_string())
        .and_then(|r| r.into_string().map_err(|e| e.to_string()))
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(snapshot) => snapshot,
        Err(e) => return format!("state: {e}"),
    };
    let counts = (
        snapshot.sessions.len(),
        snapshot.cards.len(),
        snapshot.artifacts.len(),
    );
    handle.block_on(state.load_remote_snapshot(snapshot, label));
    canvas_core::log::info(
        "upstream connected",
        &[
            ("upstream", &base),
            ("sessions", &counts.0),
            ("cards", &counts.1),
            ("artifacts", &counts.2),
        ],
    );

    let mut name = String::new();
    let mut data = String::new();
    for line in BufReader::new(events.into_reader()).lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => return format!("event stream: {e}"),
        };
        if line.is_empty() {
            if !name.is_empty() {
                handle.block_on(state.apply_remote_event(&name, &data, label));
            }
            name.clear();
            data.clear();
        } else if let Some(v) = line.strip_prefix("event:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    "event stream closed".to_string()
}

/// Starts the link when `sharing.toml` names an upstream: connects, merges,
/// and on a lost link drops the upstream's items, shows it offline and
/// reconnects after 2s, doubling to 30s.
pub fn spawn(state: AppState) {
    let Some(upstream) = state.sharing.upstream.clone() else {
        return;
    };
    let base = upstream.trim_end_matches('/').to_string();
    let label = host_label(&base);
    tokio::spawn(async move {
        state.remote.write().await.status = Some(UpstreamStatus {
            url: base.clone(),
            online: false,
            error: None,
        });
        let mut wait = RETRY_FIRST;
        loop {
            let handle = tokio::runtime::Handle::current();
            let (link_state, link_base, link_label) = (state.clone(), base.clone(), label.clone());
            let started = std::time::Instant::now();
            let reason = tokio::task::spawn_blocking(move || {
                run_link(&link_state, &handle, &link_base, &link_label)
            })
            .await
            .unwrap_or_else(|e| format!("link task: {e}"));
            // A link that held for a while starts its backoff over.
            if started.elapsed() > RETRY_MAX {
                wait = RETRY_FIRST;
            }
            canvas_core::log::warn(
                "upstream lost",
                &[
                    ("upstream", &base),
                    ("error", &reason),
                    ("retry_secs", &wait.as_secs()),
                ],
            );
            state.drop_remote(reason).await;
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(RETRY_MAX);
        }
    });
}

/// Fetches `path` from the upstream, forwarding the caller's `Range`, and
/// answers with what it answered: status, body and the headers a browser needs
/// (type, ranges, the artifact CSP and CORS). Used for the files of a card or
/// artifact the upstream holds.
///
/// With `artifact` set, the response carries this daemon's own artifact CSP,
/// CORS and caching headers for that id, never the upstream's, so a page the
/// upstream served loosely is still confined here.
pub async fn proxy_get(
    state: &AppState,
    path: &str,
    request_headers: &HeaderMap,
    artifact: Option<&str>,
) -> Response {
    let Some(status) = state.remote.read().await.status.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let url = format!("{}{path}", status.url);
    let range = request_headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let fetched = tokio::task::spawn_blocking(move || {
        let mut request = agent(Duration::from_secs(30)).get(&url);
        if let Some(range) = &range {
            request = request.set("range", range);
        }
        let response = match request.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(e.to_string()),
        };
        let code = response.status();
        let headers: Vec<(HeaderName, HeaderValue)> = [
            header::CONTENT_TYPE,
            header::CONTENT_RANGE,
            header::ACCEPT_RANGES,
            header::CONTENT_SECURITY_POLICY,
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
        ]
        .into_iter()
        .filter_map(|name| {
            let value = response.header(name.as_str())?;
            Some((name, HeaderValue::from_str(value).ok()?))
        })
        .collect();
        let mut body = Vec::new();
        response
            .into_reader()
            .take(PROXY_MAX_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if body.len() as u64 > PROXY_MAX_BYTES {
            return Err(format!("the file is over {} MB", PROXY_MAX_BYTES >> 20));
        }
        Ok((code, headers, body))
    })
    .await;
    match fetched {
        Ok(Ok((code, headers, body))) => {
            let mut response = (
                StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
                body,
            )
                .into_response();
            for (name, value) in headers {
                response.headers_mut().insert(name, value);
            }
            if let Some(id) = artifact {
                let own = [
                    (
                        header::CONTENT_SECURITY_POLICY,
                        crate::artifacts::content_security_policy(
                            id,
                            request_headers
                                .get(header::HOST)
                                .and_then(|h| h.to_str().ok()),
                        ),
                    ),
                    (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".to_string()),
                    (header::CACHE_CONTROL, "no-store".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ];
                for (name, value) in own {
                    if let Ok(value) = HeaderValue::from_str(&value) {
                        response.headers_mut().insert(name, value);
                    }
                }
            }
            response
        }
        Ok(Err(e)) => {
            canvas_core::log::warn("upstream fetch failed", &[("path", &path), ("error", &e)]);
            (StatusCode::BAD_GATEWAY, format!("upstream: {e}")).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("upstream: {e}")).into_response(),
    }
}

/// `s` percent-encoded for a URL path, keeping `/`.
pub fn encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_label_takes_the_host_of_an_upstream_url() {
        assert_eq!(host_label("https://hub.example/"), "hub.example");
        assert_eq!(host_label("http://100.64.0.1:7000"), "100.64.0.1");
        assert_eq!(host_label("http://[fd7a::1]:7000/x"), "fd7a::1");
        assert_eq!(host_label("hub"), "hub");
    }

    #[test]
    fn a_remote_card_names_no_absolute_path() {
        let card = Card {
            id: "c".into(),
            session_id: "s".into(),
            at: "2026-01-01T00:00:00Z".into(),
            updated_at: None,
            html: String::new(),
            images: vec!["/hub/media/a-b.mp4".into(), "/hub/media/c.png".into()],
            targets: vec![],
        };
        let card = remote_card(card);
        assert_eq!(card.images, vec!["upstream:a-b.mp4", "upstream:c.png"]);
    }

    #[test]
    fn encode_path_keeps_slashes_and_escapes_the_rest() {
        assert_eq!(encode_path("a b/ü.js"), "a%20b/%C3%BC.js");
    }
}
