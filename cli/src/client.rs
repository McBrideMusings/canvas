//! Thin, timeout-bounded client for canvasd. Every call returns an error on
//! any failure (connection refused, timeout, non-2xx) — callers are expected
//! to swallow it, since a hook must never slow or break a Claude session.

use std::time::Duration;

use canvas_core::unix_http::{self, percent_encode, Response};
use canvas_core::{Card, PostRequest, Session, UpdateCardRequest};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

/// The per-request timeout: 1s, or `CANVAS_CLIENT_TIMEOUT_MS` when set, so a
/// test can pick a value that doesn't depend on how busy the machine is.
fn timeout() -> Duration {
    std::env::var("CANVAS_CLIENT_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// Why a call to canvasd failed: it answered with a non-2xx status, or the
/// request never got a complete answer (no socket, refused, timed out).
/// `Transport` holds the finished sentence [`unix_http::failure_text`] wrote.
#[derive(Debug)]
enum Failure {
    Status(u16),
    Transport(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Status(code) => write!(f, "canvasd returned HTTP {code}"),
            Failure::Transport(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Failure {}

fn call(method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<Response, Failure> {
    let response = call_any_status(method, path, body, timeout())?;
    if (200..300).contains(&response.status) {
        Ok(response)
    } else {
        Err(Failure::Status(response.status))
    }
}

/// One request to canvasd, whatever status it answers. `timeout` bounds each
/// socket read and write: `timeout()` for everything but the one call that
/// waits on a viewer.
fn call_any_status(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    timeout: Duration,
) -> Result<Response, Failure> {
    call_with_headers(method, path, body, &[], timeout)
}

/// [`call_any_status`] with `extra` headers sent alongside.
fn call_with_headers(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    extra: &[(&str, &str)],
    timeout: Duration,
) -> Result<Response, Failure> {
    let payload = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let mut headers: Vec<(&str, &str)> = extra.to_vec();
    if body.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    unix_http::call(method, path, &headers, &payload, timeout)
        .map_err(|e| Failure::Transport(unix_http::failure_text(&e)))
}

fn into_json<T: serde::de::DeserializeOwned>(response: Response) -> Result<T, String> {
    serde_json::from_slice(&response.body)
        .map_err(|e| format!("canvasd returned malformed JSON: {e}"))
}

pub fn end_session(session_id: &str) -> Result<(), String> {
    call(
        "POST",
        &format!("/api/sessions/{session_id}/end"),
        Some(&serde_json::json!({})),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Unlike the hook calls above, `canvas post` is meant to be seen by the
/// agent that ran it, so this returns a one-line, human-readable message on
/// every failure instead of the raw `Failure`, and returns the card the
/// daemon created on success so the caller can report its id.
pub fn post_explicit(body: PostRequest) -> Result<Card, String> {
    let result = call_any_status(
        "POST",
        "/api/posts",
        Some(&serde_json::to_value(body).unwrap_or_default()),
        timeout(),
    );
    handle_card_response(result)
}

/// Replaces an existing card's content in place, keyed by the card id a
/// prior `canvas post` reported. Same one-line error handling as
/// `post_explicit` — `--update` is run by an agent that needs to know
/// whether it worked.
pub fn update_card(
    card_id: &str,
    html: String,
    images: Vec<String>,
    targets: Vec<String>,
) -> Result<Card, String> {
    let body = UpdateCardRequest {
        html,
        images,
        targets,
    };
    let result = call_any_status(
        "PUT",
        &format!("/api/cards/{card_id}"),
        Some(&serde_json::to_value(body).unwrap_or_default()),
        timeout(),
    );
    handle_card_response(result)
}

/// `canvas wait`: polls `GET /api/cards/:id/reply` every 250ms until a reply
/// lands or `timeout` elapses. Blocks the calling agent, so it's for "ask a
/// question in a card, then wait for the click" — `get_reply` below is the
/// non-blocking single check for "did they answer yet".
pub fn wait_for_reply(card_id: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match get_reply(card_id)? {
            Some(value) => return Ok(value),
            None => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!("no reply for {card_id} within {timeout:?}"));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
}

/// `canvas replies`: one non-blocking check of `GET /api/cards/:id/reply`.
/// `Ok(None)` means the card hasn't been answered yet — not an error, since
/// checking back later without blocking is the whole point of this call.
pub fn get_reply(card_id: &str) -> Result<Option<serde_json::Value>, String> {
    match call("GET", &format!("/api/cards/{card_id}/reply"), None) {
        Ok(response) => into_json(response).map(Some),
        Err(Failure::Status(404)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// `canvas data`: pushes `value` into the named card's running script.
pub fn push_data(card_id: &str, value: &serde_json::Value) -> Result<(), String> {
    match call("PUT", &format!("/api/cards/{card_id}/data"), Some(value)) {
        Ok(_) => Ok(()),
        Err(Failure::Status(404)) => {
            Err("canvasd returned HTTP 404 (no card with that id)".to_string())
        }
        Err(Failure::Status(413)) => {
            Err("the value is over 256KB, the most a card takes".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `canvas focus`: asks every open viewer to bring the card into view and
/// returns how many viewers the request reached.
pub fn focus_card(card_id: &str) -> Result<u64, String> {
    #[derive(serde::Deserialize)]
    struct Focused {
        viewers: u64,
    }
    let result = call(
        "POST",
        &format!("/api/cards/{card_id}/focus"),
        Some(&serde_json::json!({})),
    );
    match result {
        Ok(response) => {
            let viewers = into_json::<Focused>(response)?.viewers;
            canvas_core::log::info("focus", &[("card", &card_id), ("viewers", &viewers)]);
            Ok(viewers)
        }
        Err(Failure::Status(404)) => {
            Err("canvasd returned HTTP 404 (no card with that id)".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Asks every open viewer to switch to `theme`; returns how many it reached.
/// canvasd refuses anything but `light` or `dark`.
pub fn set_theme(theme: &str) -> Result<u64, String> {
    #[derive(serde::Deserialize)]
    struct Set {
        viewers: u64,
    }
    let response = call(
        "POST",
        "/api/theme",
        Some(&serde_json::json!({ "theme": theme })),
    )
    .map_err(|e| e.to_string())?;
    let viewers = into_json::<Set>(response)?.viewers;
    canvas_core::log::info("theme set", &[("theme", &theme), ("viewers", &viewers)]);
    Ok(viewers)
}

/// The theme a viewer last reported showing, `None` before any has.
pub fn viewer_theme() -> Result<Option<String>, String> {
    #[derive(serde::Deserialize)]
    struct Reported {
        theme: Option<String>,
    }
    let response = call("GET", "/api/theme", None).map_err(|e| e.to_string())?;
    Ok(into_json::<Reported>(response)?.theme)
}

/// canvasd waits up to 20s for a viewer to capture a card; this leaves it
/// room to answer that it gave up.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(25);

/// What a viewer captured of a card or artifact pane for `canvas snapshot`.
pub struct Snapshot {
    pub png: Vec<u8>,
    /// The card is taller than the window, so the PNG stops at its edge.
    pub clipped: bool,
}

/// `canvas snapshot`: the PNG an open viewer captured of the card, or of the
/// artifact's pane for an `art-` id. An error is canvasd's one-line reason: no
/// viewer open, the viewer's own reason it could not capture it, or that no
/// viewer answered in time.
pub fn snapshot(id: &str) -> Result<Snapshot, String> {
    let (route, noun) = if id.starts_with(canvasd::artifacts::ID_PREFIX) {
        ("artifacts", "artifact")
    } else {
        ("cards", "card")
    };
    let response = call_any_status(
        "POST",
        &format!("/api/{route}/{}/snapshot", percent_encode(id)),
        Some(&serde_json::json!({})),
        SNAPSHOT_TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    match response.status {
        200 => Ok(Snapshot {
            clipped: response.header("x-canvas-clipped") == Some("true"),
            png: response.body,
        }),
        404 => Err(format!(
            "canvasd returned HTTP 404 (no {noun} with that id)"
        )),
        status => Err(canvas_core::log::error_text(&response.body)
            .unwrap_or_else(|| format!("canvasd returned HTTP {status}"))),
    }
}

/// `canvas card`: one card as the daemon holds it, for an agent handed a
/// card id (from a pasted post link) to read the HTML the viewer renders.
pub fn get_card(card_id: &str) -> Result<Card, String> {
    handle_card_response(call_any_status(
        "GET",
        &format!("/api/cards/{card_id}"),
        None,
        timeout(),
    ))
}

/// Every session and card canvasd holds, cards oldest first.
#[derive(Debug, serde::Deserialize)]
pub struct Stream {
    pub sessions: Vec<Session>,
    pub cards: Vec<Card>,
}

/// Reading up to 500 cards' HTML, more than a hook-sized call carries.
const STATE_TIMEOUT: Duration = Duration::from_secs(10);

/// `canvas export --all`: the daemon's whole stream from `/api/state`.
pub fn get_stream() -> Result<Stream, String> {
    let response =
        call_any_status("GET", "/api/state", None, STATE_TIMEOUT).map_err(|e| e.to_string())?;
    match response.status {
        200..=299 => into_json(response),
        status => Err(Failure::Status(status).to_string()),
    }
}

fn handle_card_response(result: Result<Response, Failure>) -> Result<Card, String> {
    let response = result.map_err(|e| e.to_string())?;
    match response.status {
        200..=299 => into_json(response),
        404 => Err("canvasd returned HTTP 404 (no card with that id)".to_string()),
        // canvasd's own error text, when it sent one, is the whole line.
        status => Err(canvas_core::log::error_text(&response.body)
            .unwrap_or_else(|| Failure::Status(status).to_string())),
    }
}

/// `canvas focus art-…`: how many viewers the artifact-focus event reached.
pub fn focus_artifact(id: &str) -> Result<u64, String> {
    let value = artifact_call(
        "POST",
        &format!("/api/artifacts/{}/focus", percent_encode(id)),
        Some(&serde_json::json!({})),
    )?;
    let viewers = value["viewers"]
        .as_u64()
        .ok_or_else(|| "canvasd returned malformed JSON: no viewers count".to_string())?;
    canvas_core::log::info("focus", &[("artifact", &id), ("viewers", &viewers)]);
    Ok(viewers)
}

/// One `/api/artifacts` call, answering canvasd's JSON. A non-2xx status
/// becomes one line carrying canvasd's own error text, so `canvas artifact`
/// says why (an unknown id, a source path that isn't there). The session,
/// agent and pid of the coding agent this shell runs under, when there is
/// one, go along as headers for canvasd's provenance log.
pub fn artifact_call(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let actor = crate::agent::from_env()
        .map(|(adapter, session)| (session, adapter, adapter.pid().map(|p| p.to_string())));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some((session, adapter, pid)) = &actor {
        headers.push(("X-Canvas-Session", session));
        headers.push(("X-Canvas-Agent", adapter.name()));
        if let Some(pid) = pid {
            headers.push(("X-Canvas-Pid", pid));
        }
    }
    let response =
        call_with_headers(method, path, body, &headers, timeout()).map_err(|e| e.to_string())?;
    if (200..300).contains(&response.status) {
        return into_json(response);
    }
    let text = String::from_utf8_lossy(&response.body).trim().to_string();
    Err(if text.is_empty() {
        format!("canvasd returned HTTP {}", response.status)
    } else {
        format!("canvasd returned HTTP {} ({text})", response.status)
    })
}
