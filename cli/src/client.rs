//! Thin, timeout-bounded client for canvasd. Every call returns an error on
//! any failure (connection refused, timeout, non-2xx) — callers are expected
//! to swallow it, since a hook must never slow or break a Claude session.

use std::time::Duration;

use canvas_core::{Card, GuidanceState, PostRequest, UpdateCardRequest, UpsertSessionRequest};

const TIMEOUT: Duration = Duration::from_secs(1);

fn base_url() -> String {
    std::env::var("CANVAS_URL").unwrap_or_else(|_| "http://127.0.0.1:8229".to_string())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(TIMEOUT)
        .timeout(TIMEOUT)
        .build()
}

/// Serialize `body` and POST it to `path` under `base_url()`. A body that
/// fails to serialize still gets sent (as `null`) rather than panicking or
/// short-circuiting the request. The raw `ureq::Result` is returned unchanged
/// so each caller keeps its own handling: the hook calls just propagate it,
/// while `post_explicit` inspects it to build its one-line error messages.
fn post_json<T: serde::Serialize>(path: &str, body: T) -> Result<ureq::Response, ureq::Error> {
    agent()
        .post(&format!("{}{}", base_url(), path))
        .send_json(serde_json::to_value(body).unwrap_or_default())
}

pub fn upsert_session(session_id: &str, cwd: &str) -> Result<(), ureq::Error> {
    let body = UpsertSessionRequest {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
    };
    post_json("/api/sessions", body)?;
    Ok(())
}

pub fn end_session(session_id: &str) -> Result<(), ureq::Error> {
    agent()
        .post(&format!("{}/api/sessions/{}/end", base_url(), session_id))
        .send_json(serde_json::json!({}))?;
    Ok(())
}

/// Unlike the hook calls above, `canvas post` is meant to be seen by the
/// agent that ran it, so this returns a one-line, human-readable message on
/// every failure instead of the raw `ureq::Error`, and returns the card the
/// daemon created on success so the caller can report its id.
pub fn post_explicit(
    session_id: &str,
    cwd: &str,
    html: String,
    images: Vec<String>,
    targets: Vec<String>,
) -> Result<Card, String> {
    let body = PostRequest {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        html,
        images,
        targets,
    };
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
    let result = agent()
        .put(&format!("{}/api/cards/{}", base_url(), card_id))
        .send_json(serde_json::to_value(body).unwrap_or_default());
    handle_card_response(result)
}

/// Fetches the guidance override in effect for `cwd`. `None` on any failure
/// (canvasd unreachable, timeout, bad response) — every caller falls back to
/// its own compiled-in default in that case, same as every other client.rs
/// call in a hook's path.
pub fn fetch_guidance(cwd: &str) -> Option<GuidanceState> {
    let response = agent()
        .get(&format!("{}/api/guidance", base_url()))
        .query("cwd", cwd)
        .call()
        .ok()?;
    response.into_json::<GuidanceState>().ok()
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
    let result = agent()
        .get(&format!("{}/api/cards/{}/reply", base_url(), card_id))
        .call();
    match result {
        Ok(response) => response
            .into_json::<serde_json::Value>()
            .map(Some)
            .map_err(|e| format!("canvasd returned malformed JSON: {e}")),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => Err(format!("canvasd returned HTTP {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("could not reach canvasd: {t}")),
    }
}

fn handle_card_response(result: Result<ureq::Response, ureq::Error>) -> Result<Card, String> {
    match result {
        Ok(response) => response
            .into_json::<Card>()
            .map_err(|e| format!("canvasd returned malformed JSON: {e}")),
        Err(ureq::Error::Status(404, _)) => {
            Err("canvasd returned HTTP 404 (no card with that id)".to_string())
        }
        Err(ureq::Error::Status(code, _)) => Err(format!("canvasd returned HTTP {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("could not reach canvasd: {t}")),
    }
}
