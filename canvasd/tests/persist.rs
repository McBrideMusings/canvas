use std::path::PathBuf;

use axum::body::Body;
use axum::http::Request;
use canvas_core::StateResponse;
use canvasd::build_router;
use canvasd::state::AppState;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canvasd-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn send(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Vec<u8> {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    res.into_body().collect().await.unwrap().to_bytes().to_vec()
}

async fn state_of(app: &axum::Router) -> Value {
    let bytes = send(app, "GET", "/api/state", None).await;
    let mut v: Value = serde_json::from_slice(&bytes).unwrap();
    v["sessions"].as_array_mut().unwrap().sort_by_key(|s| s["id"].to_string());
    v
}

/// Drop the app, then start a new one on the same directory.
async fn restart(state: AppState, dir: &PathBuf) -> axum::Router {
    state.flush_store();
    drop(state);
    build_router(AppState::open(dir))
}

fn line_count(dir: &PathBuf) -> usize {
    std::fs::read_to_string(dir.join("stream.jsonl")).unwrap().lines().count()
}

#[tokio::test]
async fn restart_rebuilds_the_same_state() {
    let dir = temp_dir();
    let state = AppState::open(&dir);
    let app = build_router(state.clone());
    send(&app, "POST", "/api/sessions", Some(json!({"session_id": "s1", "cwd": "/a/one"}))).await;
    send(&app, "POST", "/api/posts", Some(json!({"session_id": "s1", "cwd": "/a/one", "html": "<p>x</p>"}))).await;
    send(&app, "POST", "/api/posts", Some(json!({"session_id": "s1", "cwd": "/a/one", "html": "<p>y</p>"}))).await;
    send(&app, "POST", "/api/turns", Some(json!({"session_id": "s1", "cwd": "/a/one", "links": ["https://e.com"]}))).await;
    send(&app, "POST", "/api/turns", Some(json!({"session_id": "s2", "cwd": "/a/two", "paths": ["/tmp/f"]}))).await;
    send(&app, "POST", "/api/sessions/s2/end", None).await;
    let before = state_of(&app).await;
    assert_eq!(before["cards"].as_array().unwrap().len(), 2);

    let app = restart(state, &dir).await;
    assert_eq!(state_of(&app).await, before);
    // Compaction: 2 sessions + 2 cards, down from one line per change.
    assert_eq!(line_count(&dir), 4);
}

#[tokio::test]
async fn deletions_stay_deleted_after_restart() {
    let dir = temp_dir();
    let state = AppState::open(&dir);
    let app = build_router(state.clone());
    for s in ["keep", "gone"] {
        send(&app, "POST", "/api/turns", Some(json!({"session_id": s, "cwd": "/a", "links": ["https://e.com"]}))).await;
    }
    send(&app, "POST", "/api/posts", Some(json!({"session_id": "third", "cwd": "/a", "html": "h"}))).await;
    let all: StateResponse = serde_json::from_slice(&send(&app, "GET", "/api/state", None).await).unwrap();
    let third_card = all.cards.iter().find(|c| c.session_id == "third").unwrap().id.clone();
    send(&app, "DELETE", &format!("/api/cards/{third_card}"), None).await;
    send(&app, "DELETE", "/api/sessions/gone", None).await;
    let before = state_of(&app).await;

    let app = restart(state, &dir).await;
    let after = state_of(&app).await;
    assert_eq!(after, before);
    let ids: Vec<&str> = after["sessions"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["keep", "third"]);
    assert_eq!(after["cards"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn entries_older_than_24_hours_are_dropped() {
    let dir = temp_dir();
    let old = (chrono::Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
    let new = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let session = |id: &str, at: &str| json!({"op": "session_upserted", "data": {"id": id, "cwd": "/a", "name": "a", "startedAt": at}});
    let card = |id: &str, sid: &str, at: &str| json!({"op": "card_upserted", "data": {"id": id, "sessionId": sid, "at": at, "html": [], "links": ["https://e.com"], "paths": [], "images": [], "open": false}});
    let lines = [
        session("old", &old),
        card("c-old", "old", &old),
        session("live", &old), // started long ago, still has a recent card
        card("c-live", "live", &new),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("stream.jsonl"), body).unwrap();

    let app = build_router(AppState::open(&dir));
    let state = state_of(&app).await;
    let sessions: Vec<&str> = state["sessions"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(sessions, ["live"]);
    assert_eq!(state["cards"][0]["id"], "c-live");
    assert_eq!(line_count(&dir), 2);
}

#[tokio::test]
async fn torn_last_line_is_skipped() {
    let dir = temp_dir();
    let state = AppState::open(&dir);
    let app = build_router(state.clone());
    send(&app, "POST", "/api/turns", Some(json!({"session_id": "s1", "cwd": "/a", "links": ["https://e.com"]}))).await;
    let before = state_of(&app).await;
    state.flush_store();
    let mut file = std::fs::OpenOptions::new().append(true).open(dir.join("stream.jsonl")).unwrap();
    std::io::Write::write_all(&mut file, br#"{"op":"card_upserted","data":{"id":"torn","sess"#).unwrap();
    drop(state);

    let app = build_router(AppState::open(&dir));
    assert_eq!(state_of(&app).await, before);
}
