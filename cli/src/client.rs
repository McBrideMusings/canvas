//! Thin, timeout-bounded client for canvasd. Every call returns an error on
//! any failure (connection refused, timeout, non-2xx) — callers are expected
//! to swallow it, since a hook must never slow or break a Claude session.

use std::time::Duration;

use canvas_core::{Card, PostRequest, UpsertSessionRequest};

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

    match result {
        Ok(response) => response
            .into_json::<Card>()
            .map_err(|e| format!("canvasd returned malformed JSON: {e}")),
        Err(ureq::Error::Status(code, _)) => Err(format!("canvasd returned HTTP {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("could not reach canvasd: {t}")),
    }
}
