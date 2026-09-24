//! Thin, timeout-bounded client for canvasd. Every call returns an error on
//! any failure (connection refused, timeout, non-2xx) — callers are expected
//! to swallow it, since a hook must never slow or break a Claude session.

use std::time::Duration;

use canvas_core::{PostRequest, TurnRequest, UpsertSessionRequest};

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

pub fn upsert_session(session_id: &str, cwd: &str, claude_pid: u32) -> Result<(), ureq::Error> {
    let body = UpsertSessionRequest {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        claude_pid,
    };
    agent()
        .post(&format!("{}/api/sessions", base_url()))
        .send_json(serde_json::to_value(body).unwrap_or_default())?;
    Ok(())
}

pub fn end_session(session_id: &str) -> Result<(), ureq::Error> {
    agent()
        .post(&format!("{}/api/sessions/{}/end", base_url(), session_id))
        .send_json(serde_json::json!({}))?;
    Ok(())
}

pub fn post_turn(
    session_id: &str,
    links: Vec<String>,
    paths: Vec<String>,
    images: Vec<String>,
) -> Result<(), ureq::Error> {
    let body = TurnRequest {
        session_id: session_id.to_string(),
        links,
        paths,
        images,
    };
    agent()
        .post(&format!("{}/api/turns", base_url()))
        .send_json(serde_json::to_value(body).unwrap_or_default())?;
    Ok(())
}

/// Unlike the hook calls above, `canvas post` is meant to be seen by the
/// agent that ran it, so this returns a one-line, human-readable message on
/// every failure instead of the raw `ureq::Error` — including a specific
/// message for the 404 canvasd returns when no live session claims this pid.
pub fn post_explicit(claude_pid: u32, html: String) -> Result<(), String> {
    let body = PostRequest { claude_pid, html };
    let result = agent()
        .post(&format!("{}/api/posts", base_url()))
        .send_json(serde_json::to_value(body).unwrap_or_default());

    match result {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => Err(format!(
            "no live Canvas session is registered for claude pid {claude_pid}"
        )),
        Err(ureq::Error::Status(code, _)) => Err(format!("canvasd returned HTTP {code}")),
        Err(ureq::Error::Transport(t)) => Err(format!("could not reach canvasd: {t}")),
    }
}
