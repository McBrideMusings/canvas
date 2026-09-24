use axum::body::Body;
use axum::http::{Request, StatusCode};
use canvas_core::{Session, StateResponse, TurnCard};
use canvasd::build_router;
use canvasd::state::AppState;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

fn app() -> axum::Router {
    build_router(AppState::new())
}

async fn json_body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn root_serves_viewer() {
    let response = app().oneshot(get("/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("<title>Canvas</title>"));
}

#[tokio::test]
async fn upsert_and_end_session() {
    let app = app();

    let response = app
        .clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/Users/me/Projects/canvas", "claudePid": 111}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session: Session = json_body(response).await;
    assert_eq!(session.name, "canvas");
    assert!(session.ended_at.is_none());

    let response = app
        .clone()
        .oneshot(post("/api/sessions/s1/end", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    let s = state.sessions.iter().find(|s| s.id == "s1").unwrap();
    assert!(s.ended_at.is_some());
}

#[tokio::test]
async fn empty_turn_with_no_open_card_creates_nothing() {
    let app = app();

    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/tmp/proj", "claudePid": 1}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(post(
            "/api/turns",
            json!({"sessionId": "s1", "links": [], "paths": [], "images": []}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 0);
}

#[tokio::test]
async fn turn_with_content_and_no_open_card_creates_closed_card() {
    let app = app();

    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/tmp/proj", "claudePid": 1}),
        ))
        .await
        .unwrap();

    app.clone()
        .oneshot(post(
            "/api/turns",
            json!({"sessionId": "s1", "links": ["https://example.com"], "paths": [], "images": []}),
        ))
        .await
        .unwrap();

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 1);
    assert!(!state.cards[0].open);
    assert_eq!(state.cards[0].links, vec!["https://example.com"]);
}

#[tokio::test]
async fn explicit_post_opens_then_next_turn_closes_it() {
    let app = app();

    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/tmp/proj", "claudePid": 42}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"claudePid": 42, "html": "<p>hello</p>"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let card: TurnCard = json_body(response).await;
    assert!(card.open);
    assert_eq!(card.html, vec!["<p>hello</p>"]);

    // A second post extends the same open card.
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"claudePid": 42, "html": "<p>again</p>"}),
        ))
        .await
        .unwrap();
    let card2: TurnCard = json_body(response).await;
    assert_eq!(card2.id, card.id);
    assert_eq!(card2.html, vec!["<p>hello</p>", "<p>again</p>"]);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 1);
    assert!(state.cards[0].open);

    // The next turn closes it.
    app.clone()
        .oneshot(post(
            "/api/turns",
            json!({"sessionId": "s1", "links": ["/tmp/foo"], "paths": [], "images": []}),
        ))
        .await
        .unwrap();

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 1);
    assert!(!state.cards[0].open);
    assert_eq!(state.cards[0].html, vec!["<p>hello</p>", "<p>again</p>"]);
}

#[tokio::test]
async fn post_with_unknown_pid_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"claudePid": 9999, "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ring_evicts_oldest_at_501() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/tmp/proj", "claudePid": 1}),
        ))
        .await
        .unwrap();

    for i in 0..501 {
        app.clone()
            .oneshot(post(
                "/api/turns",
                json!({"sessionId": "s1", "links": [format!("https://example.com/{i}")], "paths": [], "images": []}),
            ))
            .await
            .unwrap();
    }

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 500);
    // Newest first: the very first card (link .../0) should have been evicted.
    assert!(state
        .cards
        .iter()
        .all(|c| c.links[0] != "https://example.com/0"));
    assert_eq!(state.cards[0].links[0], "https://example.com/500");
}

#[tokio::test]
async fn file_endpoint_serves_allowed_images_and_rejects_others() {
    let app = app();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let image_path = format!("{manifest_dir}/../docs/spikes/sideshow-reference/populated.png");

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/file?path={}",
            urlencoding_encode(&image_path)
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/file?path={}",
            urlencoding_encode(&format!("{manifest_dir}/Cargo.toml"))
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn file_endpoint_rejects_relative_paths() {
    let app = app();
    // This relative path *does* resolve to a real png from the test process's
    // cwd (the canvasd package root) — the point is that a relative path is
    // rejected outright, not that the file happens to be missing.
    let response = app
        .clone()
        .oneshot(get(
            "/api/file?path=../docs/spikes/sideshow-reference/populated.png",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reupserting_a_session_preserves_started_and_ended_at() {
    let app = app();

    let response = app
        .clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/tmp/proj", "claudePid": 1}),
        ))
        .await
        .unwrap();
    let first: Session = json_body(response).await;

    let response = app
        .clone()
        .oneshot(post("/api/sessions/s1/end", json!({})))
        .await
        .unwrap();
    let ended: Session = json_body(response).await;
    assert!(ended.ended_at.is_some());

    // A retried/re-fired registration for the same id must not reset
    // startedAt or resurrect an already-ended session.
    let response = app
        .clone()
        .oneshot(post(
            "/api/sessions",
            json!({"sessionId": "s1", "cwd": "/tmp/proj", "claudePid": 1}),
        ))
        .await
        .unwrap();
    let reupserted: Session = json_body(response).await;
    assert_eq!(reupserted.started_at, first.started_at);
    assert_eq!(reupserted.ended_at, ended.ended_at);
}

#[tokio::test]
async fn open_rejects_relative_paths() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post("/api/open", json!({"path": "relative/path.txt"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn events_endpoint_is_sse() {
    let app = app();
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
}

fn urlencoding_encode(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.~/".contains(c) {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}
