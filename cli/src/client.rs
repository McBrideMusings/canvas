//! Thin, timeout-bounded client for canvasd. Every call returns an error on
//! any failure (connection refused, timeout, non-2xx) — callers are expected
//! to swallow it, since a hook must never slow or break a Claude session.

use std::time::Duration;

use canvas_core::unix_http::{self, Response};
use canvas_core::{
    AssignGlobalProfileRequest, AssignRepoProfileRequest, Card, EffectiveProfile, PostRequest,
    ProfilesState, SetProfileTextRequest, UpdateCardRequest,
};

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
#[derive(Debug)]
enum Failure {
    Status(u16),
    Transport(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Status(code) => write!(f, "canvasd returned HTTP {code}"),
            Failure::Transport(e) => write!(f, "could not reach canvasd: {e}"),
        }
    }
}

impl std::error::Error for Failure {}

fn call(method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<Response, Failure> {
    let response = call_any_status(method, path, body)?;
    if (200..300).contains(&response.status) {
        Ok(response)
    } else {
        Err(Failure::Status(response.status))
    }
}

fn call_any_status(
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<Response, Failure> {
    let socket = canvas_core::paths::socket_path()
        .ok_or_else(|| Failure::Transport("no HOME to place the socket".to_string()))?;
    let payload = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let headers: &[(&str, &str)] = if body.is_some() {
        &[("Content-Type", "application/json")]
    } else {
        &[]
    };
    let started = std::time::Instant::now();
    let result = unix_http::request(&socket, method, path, headers, &payload, Some(timeout()));
    log_call(method, path, started.elapsed().as_millis(), &result);
    result.map_err(|e| Failure::Transport(e.to_string()))
}

/// One line per request: its status and duration, the daemon's error text for
/// a 4xx or 5xx, or why canvasd was unreachable.
fn log_call(method: &str, path: &str, ms: u128, result: &std::io::Result<Response>) {
    use canvas_core::log;
    let request = format!("{method} {path}");
    match result {
        Ok(response) if response.status < 400 => {
            log::info(&request, &[("status", &response.status), ("ms", &ms)])
        }
        Ok(response) => {
            let text = log::error_text(&response.body);
            let mut fields: Vec<(&str, &dyn std::fmt::Display)> =
                vec![("status", &response.status), ("ms", &ms)];
            if let Some(text) = &text {
                fields.push(("error", text));
            }
            log::warn(&request, &fields)
        }
        Err(e) => log::warn(
            "canvasd unreachable",
            &[("request", &request), ("ms", &ms), ("error", e)],
        ),
    }
}

/// Serialize `body` and POST it to `path`. A body that fails to serialize
/// still gets sent (as `null`) rather than panicking or short-circuiting the
/// request. The raw result is returned unchanged so each caller keeps its own
/// handling: the hook calls just propagate it, while `post_explicit` maps it
/// to its one-line error messages.
fn post_json<T: serde::Serialize>(path: &str, body: T) -> Result<Response, Failure> {
    call(
        "POST",
        path,
        Some(&serde_json::to_value(body).unwrap_or_default()),
    )
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
    let result = post_json("/api/posts", body);
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
    let result = call(
        "PUT",
        &format!("/api/cards/{card_id}"),
        Some(&serde_json::to_value(body).unwrap_or_default()),
    );
    handle_card_response(result)
}

/// Fetches the text of the profile in effect for `kind` at `cwd`. `None` on
/// any failure (canvasd unreachable, timeout, bad response) or when nothing
/// is assigned — every caller falls back to its own compiled-in default in
/// both cases, same as every other client.rs call in a hook's path.
pub fn fetch_profile_text(kind: &str, cwd: &str) -> Option<String> {
    let response = call(
        "GET",
        &format!("/api/profiles/{kind}/effective?cwd={}", percent_encode(cwd)),
        None,
    )
    .ok()?;
    into_json::<EffectiveProfile>(response).ok()?.text
}

/// One `canvas profile` request. Unlike `call`, a non-2xx answer keeps the
/// daemon's body in the error (the assign routes explain a 400 with
/// `no profile named "x"`), since the caller is a person or agent who needs
/// the reason on one stderr line.
fn profile_call(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<Response, String> {
    let response = call_any_status(method, path, body.as_ref()).map_err(|e| e.to_string())?;
    if (200..300).contains(&response.status) {
        return Ok(response);
    }
    let detail = String::from_utf8_lossy(&response.body);
    let detail = detail.trim();
    if detail.is_empty() {
        Err(Failure::Status(response.status).to_string())
    } else {
        Err(format!("{}: {detail}", Failure::Status(response.status)))
    }
}

/// `GET /api/profiles/:kind`: every profile, the global assignment, the
/// per-repo assignments and the built-in default.
pub fn get_profiles(kind: &str) -> Result<ProfilesState, String> {
    into_json(profile_call("GET", &format!("/api/profiles/{kind}"), None)?)
}

/// `GET /api/profiles/:kind/effective`, for `repo` when given, else for the
/// GitHub repo the daemon resolves from `cwd`.
pub fn get_effective_profile(
    kind: &str,
    repo: Option<&str>,
    cwd: &str,
) -> Result<EffectiveProfile, String> {
    let query = match repo {
        Some(repo) => format!("repo={}", percent_encode(repo)),
        None => format!("cwd={}", percent_encode(cwd)),
    };
    into_json(profile_call(
        "GET",
        &format!("/api/profiles/{kind}/effective?{query}"),
        None,
    )?)
}

/// `PUT /api/profiles/:kind/definitions`: `text: None` deletes the profile.
pub fn set_profile_text(kind: &str, name: &str, text: Option<String>) -> Result<(), String> {
    let body = SetProfileTextRequest {
        name: name.to_string(),
        text,
    };
    profile_call(
        "PUT",
        &format!("/api/profiles/{kind}/definitions"),
        Some(serde_json::to_value(body).unwrap_or_default()),
    )
    .map(|_| ())
}

/// `PUT /api/profiles/:kind/global` (`repo: None`) or `…/repos`.
pub fn assign_profile(kind: &str, repo: Option<&str>, profile: Option<&str>) -> Result<(), String> {
    let profile = profile.map(str::to_string);
    let (path, body) = match repo {
        Some(repo) => (
            format!("/api/profiles/{kind}/repos"),
            serde_json::to_value(AssignRepoProfileRequest {
                repo: repo.to_string(),
                profile,
            }),
        ),
        None => (
            format!("/api/profiles/{kind}/global"),
            serde_json::to_value(AssignGlobalProfileRequest { profile }),
        ),
    };
    profile_call("PUT", &path, Some(body.unwrap_or_default())).map(|_| ())
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

/// The card holding `slot` in the repo `cwd` belongs to; `None` when no card
/// does.
pub fn find_pinned(cwd: &str, slot: &str) -> Result<Option<Card>, String> {
    let path = format!(
        "/api/pins?cwd={}&slot={}",
        percent_encode(cwd),
        percent_encode(slot)
    );
    match call("GET", &path, None) {
        Ok(response) => into_json(response).map(Some),
        Err(Failure::Status(404)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// `canvas unpin`: releases a card's slot. `Ok(false)` when `card_id` names no
/// pinned card.
pub fn unpin_card(card_id: &str) -> Result<bool, String> {
    match call("DELETE", &format!("/api/cards/{card_id}/pin"), None) {
        Ok(_) => Ok(true),
        Err(Failure::Status(404)) => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}

/// `canvas card`: one card as the daemon holds it, for an agent handed a
/// card id (from a pasted post link) to read the HTML the viewer renders.
pub fn get_card(card_id: &str) -> Result<Card, String> {
    handle_card_response(call("GET", &format!("/api/cards/{card_id}"), None))
}

fn handle_card_response(result: Result<Response, Failure>) -> Result<Card, String> {
    match result {
        Ok(response) => into_json(response),
        Err(Failure::Status(404)) => {
            Err("canvasd returned HTTP 404 (no card with that id)".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Percent-encodes everything but unreserved characters, for a query value.
fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
