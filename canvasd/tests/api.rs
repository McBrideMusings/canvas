use axum::body::Body;
use axum::http::{Request, StatusCode};
use canvas_core::{Card, Session, StateResponse};
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

/// The `Host` every real client sends: the CLI, hooks and app all default to
/// `http://127.0.0.1:8229`. `oneshot` never opens a socket, so nothing else
/// sets it.
const HOST: &str = "127.0.0.1:8229";

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", HOST)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    get_with_host(uri, HOST)
}

fn get_with_host(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

/// `host` simulates the `Host` header a real connection would carry (the
/// `oneshot` test harness never opens a real socket, so nothing sets it for
/// us). Every real request to canvasd carries one at whichever of
/// `127.0.0.1:<port>` or `localhost:<port>` the client connected to.
fn delete(uri: &str, origin: Option<&str>, host: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("host", host);
    if let Some(origin) = origin {
        builder = builder.header("origin", origin);
    }
    builder.body(Body::empty()).unwrap()
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
            json!({"session_id": "s1", "cwd": "/Users/me/Projects/canvas"}),
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

/// A throwaway git checkout whose `origin` is `remote`, plus a linked
/// worktree of it.
fn git_checkout_with_origin(remote: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("canvasd-repo-{}", uuid::Uuid::new_v4()));
    let main = root.join("main");
    let linked = root.join("linked");
    std::fs::create_dir_all(&main).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", remote]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);
    git(&["worktree", "add", "-q", linked.to_str().unwrap()]);
    (main, linked)
}

#[tokio::test]
async fn sessions_record_the_github_repo_of_their_cwd() {
    let app = app();
    let (main, linked) = git_checkout_with_origin("git@github.com:octo/hello.git");

    // SessionStart in the main checkout.
    let response = app
        .clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": "r1", "cwd": main}),
        ))
        .await
        .unwrap();
    let session: Session = json_body(response).await;
    assert_eq!(session.repo.as_deref(), Some("octo/hello"));

    // A post for a session the daemon never saw, in a linked worktree.
    app.clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "r2", "cwd": linked, "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();

    // Outside any git checkout.
    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": "r3", "cwd": "/"}),
        ))
        .await
        .unwrap();

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    let repo = |id: &str| {
        state
            .sessions
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .repo
            .clone()
    };
    assert_eq!(repo("r2").as_deref(), Some("octo/hello"));
    assert_eq!(repo("r3"), None);

    std::fs::remove_dir_all(main.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn turns_endpoint_is_gone() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/turns",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "links": [], "paths": [], "images": []}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn each_post_creates_its_own_card() {
    let app = app();

    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": "s1", "cwd": "/tmp/proj"}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "html": "<p>hello</p>"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let card: Card = json_body(response).await;
    assert_eq!(card.html, "<p>hello</p>");
    assert!(card.images.is_empty());
    assert!(card.targets.is_empty());

    // A second post from the same session makes a second, distinct card.
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "html": "<p>again</p>"}),
        ))
        .await
        .unwrap();
    let card2: Card = json_body(response).await;
    assert_ne!(card2.id, card.id);
    assert_eq!(card2.html, "<p>again</p>");

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 2);
    let ids: Vec<&str> = state.cards.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&card.id.as_str()));
    assert!(ids.contains(&card2.id.as_str()));
}

#[tokio::test]
async fn post_for_unknown_session_creates_it_named_from_cwd() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s9", "cwd": "/tmp/unregistered-proj", "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let card: Card = json_body(response).await;
    assert_eq!(card.session_id, "s9");
    assert_eq!(card.html, "<p>x</p>");

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    let session = state.sessions.iter().find(|s| s.id == "s9").unwrap();
    assert_eq!(session.cwd, "/tmp/unregistered-proj");
    assert_eq!(session.name, "unregistered-proj");
}

#[tokio::test]
async fn ring_evicts_oldest_at_501() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": "s1", "cwd": "/tmp/proj"}),
        ))
        .await
        .unwrap();

    for i in 0..501 {
        app.clone()
            .oneshot(post(
                "/api/posts",
                json!({"session_id": "s1", "cwd": "/tmp/proj", "html": format!("<p>{i}</p>")}),
            ))
            .await
            .unwrap();
    }

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 500);
    // Newest first: the very first card (html "<p>0</p>") should have been evicted.
    assert!(state.cards.iter().all(|c| c.html != "<p>0</p>"));
    assert_eq!(state.cards[0].html, "<p>500</p>");
}

/// Posts one card carrying `images` and returns it.
async fn card_with_images(app: &axum::Router, images: Vec<String>) -> Card {
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "html": "<p>x</p>", "images": images}),
        ))
        .await
        .unwrap();
    json_body(response).await
}

#[tokio::test]
async fn card_image_serves_the_cards_images_and_rejects_others() {
    let app = app();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let card = card_with_images(
        &app,
        vec![
            format!("{manifest_dir}/../app/src-tauri/icons/tray.png"),
            format!("{manifest_dir}/Cargo.toml"),
            // Resolves to a real png from the test process's cwd (the canvasd
            // package root) — a relative path is rejected outright.
            "../app/src-tauri/icons/tray.png".to_string(),
        ],
    )
    .await;

    let status = |uri: String| {
        let app = app.clone();
        async move { app.oneshot(get(&uri)).await.unwrap().status() }
    };
    let id = &card.id;
    assert_eq!(
        status(format!("/api/cards/{id}/images/0")).await,
        StatusCode::OK
    );
    assert_eq!(
        status(format!("/api/cards/{id}/images/1")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(format!("/api/cards/{id}/images/2")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(format!("/api/cards/{id}/images/3")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status("/api/cards/unknown/images/0".to_string()).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn image_placeholder_is_resolved_to_the_cards_image_route_and_serves() {
    let app = app();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let png = format!("{manifest_dir}/../app/src-tauri/icons/tray.png");

    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({
                "session_id": "s1",
                "cwd": "/tmp/proj",
                "html": r#"<img src="canvas-image:0">"#,
                "images": [png],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let card: Card = json_body(response).await;
    let expected_src = format!("/api/cards/{}/images/0", card.id);
    assert!(card.html.contains(&expected_src), "{}", card.html);
    assert!(!card.html.contains("canvas-image:0"));

    let response = app.clone().oneshot(get(&expected_src)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!bytes.is_empty());
}

/// Prose that merely mentions the placeholder's literal text — not inside a
/// quoted attribute value — must survive untouched: the replacement is
/// scoped to `="canvas-image:<n>"`/`='canvas-image:<n>'`, not a blind
/// substring match anywhere in the html.
#[tokio::test]
async fn image_placeholder_text_outside_an_attribute_value_is_left_alone() {
    let app = app();
    let html = "<p>the placeholder looks like canvas-image:0 in prose</p>";

    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "html": html}),
        ))
        .await
        .unwrap();
    let card: Card = json_body(response).await;
    assert_eq!(card.html, html);
}

#[tokio::test]
async fn file_path_endpoint_is_gone() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let response = app()
        .oneshot(get(&format!(
            "/api/file?path={manifest_dir}/../app/src-tauri/icons/tray.png"
        )))
        .await
        .unwrap();
    assert_ne!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn reupserting_a_session_preserves_started_and_ended_at() {
    let app = app();

    let response = app
        .clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": "s1", "cwd": "/tmp/proj"}),
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
            json!({"session_id": "s1", "cwd": "/tmp/proj"}),
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

async fn seed_card(app: &axum::Router, session_id: &str, cwd: &str) -> Card {
    app.clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": session_id, "cwd": cwd}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": session_id, "cwd": cwd, "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();
    json_body(response).await
}

#[tokio::test]
async fn delete_card_removes_it_from_state() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    let response = app
        .clone()
        .oneshot(delete(
            &format!("/api/cards/{}", card.id),
            None,
            "127.0.0.1:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.cards.iter().all(|c| c.id != card.id));
}

#[tokio::test]
async fn delete_unknown_card_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(delete("/api/cards/nope", None, "127.0.0.1:8242"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_session_removes_session_and_its_cards_but_not_others() {
    let app = app();
    let card1 = seed_card(&app, "s1", "/tmp/proj").await;
    let card2 = seed_card(&app, "s2", "/tmp/proj2").await;

    let response = app
        .clone()
        .oneshot(delete("/api/sessions/s1", None, "127.0.0.1:8242"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.iter().all(|s| s.id != "s1"));
    assert!(state.cards.iter().all(|c| c.id != card1.id));
    assert!(state.sessions.iter().any(|s| s.id == "s2"));
    assert!(state.cards.iter().any(|c| c.id == card2.id));
}

#[tokio::test]
async fn delete_session_rejects_cross_origin_and_removes_nothing() {
    let app = app();
    seed_card(&app, "s1", "/tmp/proj").await;

    let response = app
        .clone()
        .oneshot(delete(
            "/api/sessions/s1",
            Some("https://evil.example"),
            "127.0.0.1:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.iter().any(|s| s.id == "s1"));

    let response = app
        .clone()
        .oneshot(delete(
            "/api/sessions/s1",
            Some("http://127.0.0.1:8242"),
            "127.0.0.1:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn delete_unknown_session_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(delete("/api/sessions/nope", None, "127.0.0.1:8242"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_card_rejects_cross_origin_and_removes_nothing() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    let response = app
        .clone()
        .oneshot(delete(
            &format!("/api/cards/{}", card.id),
            Some("https://evil.example"),
            "127.0.0.1:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.cards.iter().any(|c| c.id == card.id));
}

#[tokio::test]
async fn delete_card_rejects_https_origin_even_with_matching_host_and_port() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    let response = app
        .clone()
        .oneshot(delete(
            &format!("/api/cards/{}", card.id),
            Some("https://127.0.0.1:8242"),
            "127.0.0.1:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.cards.iter().any(|c| c.id == card.id));
}

#[tokio::test]
async fn delete_card_accepts_same_origin_127_and_localhost() {
    let app = app();
    let card1 = seed_card(&app, "s1", "/tmp/proj").await;
    let card2 = seed_card(&app, "s2", "/tmp/proj2").await;

    let response = app
        .clone()
        .oneshot(delete(
            &format!("/api/cards/{}", card1.id),
            Some("http://127.0.0.1:8242"),
            "127.0.0.1:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(delete(
            &format!("/api/cards/{}", card2.id),
            Some("http://localhost:8242"),
            "localhost:8242",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.cards.is_empty());
}

const REBOUND: &str = "rebind.example.com:8229";

#[tokio::test]
async fn rebound_host_is_refused_on_every_route() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    for uri in [
        "/".to_string(),
        "/app.js".to_string(),
        "/api/state".to_string(),
        "/api/events".to_string(),
        format!("/api/cards/{}/images/0", card.id),
    ] {
        let response = app
            .clone()
            .oneshot(get_with_host(&uri, REBOUND))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn rebound_host_cannot_delete_even_with_matching_origin() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    let response = app
        .clone()
        .oneshot(delete(
            &format!("/api/cards/{}", card.id),
            Some("http://rebind.example.com:8229"),
            REBOUND,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.cards.iter().any(|c| c.id == card.id));
}

#[tokio::test]
async fn host_with_userinfo_is_refused() {
    let response = app()
        .oneshot(get_with_host("/api/state", "evil@127.0.0.1:8229"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
}

#[tokio::test]
async fn missing_host_is_refused() {
    let request = Request::builder()
        .uri("/api/state")
        .body(Body::empty())
        .unwrap();
    let response = app().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
}

#[tokio::test]
async fn loopback_hosts_are_served_at_any_port() {
    for host in [
        "127.0.0.1:8229",
        "localhost:8229",
        "localhost:9001",
        "localhost",
    ] {
        let response = app()
            .oneshot(get_with_host("/api/state", host))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
    }
}
