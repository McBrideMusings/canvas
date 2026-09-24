//! Thin, timeout-bounded client for canvasd. Every call returns an error on
//! any failure (connection refused, timeout, non-2xx) — callers are expected
//! to swallow it, since a hook must never slow or break a Claude session.

use std::time::Duration;

use canvas_core::{TurnRequest, UpsertSessionRequest};

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
