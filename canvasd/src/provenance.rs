//! The artifact provenance log: one JSON line per create, put, relink,
//! watched change and delete, in `artifact-log.jsonl` beside
//! `artifacts.json`, kept after the artifact is deleted. `canvas artifact log
//! <id>` reads it back; the viewer never shows it.
//!
//! Who acted comes from the `x-canvas-session`, `x-canvas-agent` and
//! `x-canvas-pid` headers the CLI sends from its agent's environment. A
//! request without them (a person at a shell) and a watched change log no
//! session.

use std::path::Path;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use canvas_core::Agent;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

pub const LOG_FILE: &str = "artifact-log.jsonl";

pub const SESSION_HEADER: &str = "x-canvas-session";
pub const AGENT_HEADER: &str = "x-canvas-agent";
pub const PID_HEADER: &str = "x-canvas-pid";

/// The session, agent and process behind a request, each when known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Actor {
    pub session_id: Option<String>,
    pub agent: Option<Agent>,
    pub pid: Option<u32>,
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for Actor {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let header = |name: &str| {
            parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Ok(Actor {
            session_id: header(SESSION_HEADER),
            agent: header(AGENT_HEADER)
                .and_then(|a| serde_json::from_value(serde_json::Value::String(a)).ok()),
            pid: header(PID_HEADER).and_then(|p| p.parse().ok()),
        })
    }
}

/// What happened to the artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Put,
    Relink,
    Change,
    Delete,
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub at: String,
    pub id: String,
    pub action: Action,
    pub session_id: Option<String>,
    pub agent: Option<Agent>,
    pub pid: Option<u32>,
}

/// Appends one line for `id` to the log in `dir`. A line that can't be
/// written is logged and dropped: the action itself already happened.
pub async fn append(dir: &Path, id: &str, action: Action, actor: &Actor) {
    let entry = Entry {
        at: chrono::Utc::now().to_rfc3339(),
        id: id.to_string(),
        action,
        session_id: actor.session_id.clone(),
        agent: actor.agent,
        pid: actor.pid,
    };
    let Ok(mut line) = serde_json::to_vec(&entry) else {
        return;
    };
    line.push(b'\n');
    let written = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(LOG_FILE))
            .await?;
        file.write_all(&line).await
    }
    .await;
    if let Err(e) = written {
        canvas_core::log::warn(
            "artifact log line not written",
            &[
                ("id", &id),
                ("action", &format!("{action:?}")),
                ("error", &e),
            ],
        );
    }
}

/// Every line for `id`, oldest first. A line that doesn't parse (a torn
/// write) is skipped.
pub async fn read(dir: &Path, id: &str) -> std::io::Result<Vec<Entry>> {
    let text = match tokio::fs::read_to_string(dir.join(LOG_FILE)).await {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .filter(|entry| entry.id == id)
        .collect())
}
