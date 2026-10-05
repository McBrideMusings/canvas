use axum::body::Body;
use axum::http::{Request, StatusCode};
use canvas_core::{
    Agent, Card, EffectiveProfile, ProfileMode, ProfilesState, Session, StateResponse,
};
use canvasd::build_router;
use canvasd::state::{AppState, CanvasEvent};
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

fn put(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
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

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
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
async fn a_post_creates_the_session_and_ending_it_sets_ended_at() {
    let app = app();

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.is_empty());

    seed_card(&app, "s1", "/Users/me/Projects/canvas").await;
    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    let session = state.sessions.iter().find(|s| s.id == "s1").unwrap();
    assert_eq!(session.name, "canvas");
    assert_eq!(session.agent, Agent::ClaudeCode);
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
async fn ending_a_session_that_never_posted_is_404_and_creates_nothing() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post("/api/sessions/never/end", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.is_empty());
}

#[tokio::test]
async fn the_sessions_registration_route_is_gone() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/sessions",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.is_empty());
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

    // A first post from the main checkout.
    app.clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "r1", "cwd": main, "agent": "claude-code", "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();

    // A first post from a linked worktree.
    app.clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "r2", "cwd": linked, "agent": "claude-code", "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();

    // Outside any git checkout.
    app.clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "r3", "cwd": "/", "agent": "claude-code", "html": "<p>x</p>"}),
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
    assert_eq!(repo("r1").as_deref(), Some("octo/hello"));
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

    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>hello</p>"}),
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
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>again</p>"}),
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
            json!({"session_id": "s9", "cwd": "/tmp/unregistered-proj", "agent": "claude-code", "html": "<p>x</p>"}),
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
    assert_eq!(session.agent, Agent::ClaudeCode);
}

#[tokio::test]
async fn a_post_without_an_agent_is_rejected() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "html": "<p>x</p>"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.is_empty());
}

#[tokio::test]
async fn update_card_replaces_content_in_place_and_keeps_its_id_and_session() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>v1</p>"}),
        ))
        .await
        .unwrap();
    let card: Card = json_body(response).await;

    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/cards/{}", card.id),
            json!({"html": "<p>v2</p>", "targets": ["/tmp/x"]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let updated: Card = json_body(response).await;
    assert_eq!(updated.id, card.id);
    assert_eq!(updated.session_id, card.session_id);
    assert_eq!(updated.html, "<p>v2</p>");
    assert_eq!(updated.targets, vec!["/tmp/x".to_string()]);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 1);
    assert_eq!(state.cards[0].html, "<p>v2</p>");
}

#[tokio::test]
async fn update_card_keeps_at_sets_updated_at_and_moves_it_to_the_top() {
    let app = app();
    let a = seed_card(&app, "s1", "/tmp/proj").await;
    let b = seed_card(&app, "s1", "/tmp/proj").await;
    assert_eq!(a.updated_at, None);

    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/cards/{}", a.id),
            json!({"html": "<p>v2</p>"}),
        ))
        .await
        .unwrap();
    let updated: Card = json_body(response).await;
    assert_eq!(updated.at, a.at);
    let updated_at = updated.updated_at.clone().expect("updatedAt set");
    assert!(
        updated_at > a.at,
        "{updated_at} should be later than {}",
        a.at
    );

    let state: StateResponse =
        json_body(app.clone().oneshot(get("/api/state")).await.unwrap()).await;
    let ids: Vec<&str> = state.cards.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [a.id.as_str(), b.id.as_str()]);
}

#[tokio::test]
async fn update_unknown_card_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(put(
            "/api/cards/does-not-exist",
            json!({"html": "<p>x</p>"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ring_evicts_oldest_at_501() {
    let app = app();
    for i in 0..501 {
        app.clone()
            .oneshot(post(
                "/api/posts",
                json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": format!("<p>{i}</p>")}),
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

#[tokio::test]
async fn updating_a_card_moves_it_to_the_front_so_it_survives_eviction() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>status</p>"}),
        ))
        .await
        .unwrap();
    let status_card: Card = json_body(response).await;

    // 500 other cards land after it — enough to reach the ring's cap.
    for i in 0..499 {
        app.clone()
            .oneshot(post(
                "/api/posts",
                json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": format!("<p>{i}</p>")}),
            ))
            .await
            .unwrap();
    }
    // Touch the status card — it's still the oldest in the ring by position.
    app.clone()
        .oneshot(put(
            &format!("/api/cards/{}", status_card.id),
            json!({"html": "<p>status v2</p>"}),
        ))
        .await
        .unwrap();
    // One more post would evict it by position alone, if update hadn't moved it.
    app.clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>499</p>"}),
        ))
        .await
        .unwrap();

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert_eq!(state.cards.len(), 500);
    let survived = state.cards.iter().find(|c| c.id == status_card.id);
    assert!(
        survived.is_some(),
        "updated card was evicted despite being touched last"
    );
    assert_eq!(survived.unwrap().html, "<p>status v2</p>");
}

/// Posts one card carrying `images` and returns it.
async fn card_with_images(app: &axum::Router, images: Vec<String>) -> Card {
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>x</p>", "images": images}),
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
async fn export_inlines_images_warns_for_missing_ones_and_bakes_in_data() {
    let app = app();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let tray = format!("{manifest_dir}/../app/src-tauri/icons/tray.png");
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({
                "session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code",
                "html": "<img src=\"canvas-image:0\"><img src=\"canvas-image:1\">",
                "images": [tray, "/nonexistent/gone.png"],
            }),
        ))
        .await
        .unwrap();
    let card: Card = json_body(response).await;
    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/cards/{}/data", card.id),
            json!({"n": 7}),
        ))
        .await
        .unwrap();
    assert!(response.status().is_success());

    let response = app
        .clone()
        .oneshot(get(&format!("/api/cards/{}/export", card.id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let export: canvas_core::ExportResult = json_body(response).await;
    let png = canvas_core::base64(&std::fs::read(&tray).unwrap());
    assert!(export
        .html
        .contains(&format!("<img src=\"data:image/png;base64,{png}\">")));
    assert!(export.html.contains("image missing: gone.png"));
    assert!(!export.html.contains("/api/cards/"));
    assert!(export.html.contains(r#"var v={"n":7}"#));
    assert_eq!(
        export.warnings,
        vec![canvas_core::ExportWarning {
            kind: canvas_core::ExportWarningKind::MissingImage,
            target: "/nonexistent/gone.png".into(),
            reason: "No such file or directory (os error 2)".into(),
        }]
    );
}

#[tokio::test]
async fn export_unknown_card_is_404() {
    let response = app().oneshot(get("/api/cards/nope/export")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn card_video_serves_whole_and_by_byte_range() {
    let app = app();
    let dir = std::env::temp_dir().join(format!("canvasd-video-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("clip.webm");
    std::fs::write(&clip, b"0123456789").unwrap();
    let card = card_with_images(&app, vec![clip.to_string_lossy().to_string()]).await;
    let uri = format!("/api/cards/{}/images/0", card.id);

    let ranged = |range: &'static str| {
        let app = app.clone();
        let uri = uri.clone();
        async move {
            let request = Request::builder()
                .uri(&uri)
                .header("range", range)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let content_range = response
                .headers()
                .get("content-range")
                .map(|v| v.to_str().unwrap().to_string());
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, content_range, body)
        }
    };

    let whole = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(whole.status(), StatusCode::OK);
    assert_eq!(whole.headers()["content-type"], "video/webm");
    assert_eq!(whole.headers()["accept-ranges"], "bytes");

    let (status, content_range, body) = ranged("bytes=2-4").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(content_range.as_deref(), Some("bytes 2-4/10"));
    assert_eq!(&body[..], b"234");

    let (status, content_range, body) = ranged("bytes=7-").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(content_range.as_deref(), Some("bytes 7-9/10"));
    assert_eq!(&body[..], b"789");

    let (status, content_range, body) = ranged("bytes=-3").await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(content_range.as_deref(), Some("bytes 7-9/10"));
    assert_eq!(&body[..], b"789");

    let (status, _, body) = ranged("bytes=0-1,5-6").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"0123456789");

    let (status, content_range, _) = ranged("bytes=10-").await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(content_range.as_deref(), Some("bytes */10"));

    std::fs::remove_dir_all(&dir).unwrap();
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
                "agent": "claude-code",
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
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": html}),
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
async fn a_later_post_keeps_the_sessions_started_and_ended_at() {
    let app = app();

    seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    let first = state.sessions.into_iter().find(|s| s.id == "s1").unwrap();

    let response = app
        .clone()
        .oneshot(post("/api/sessions/s1/end", json!({})))
        .await
        .unwrap();
    let ended: Session = json_body(response).await;
    assert!(ended.ended_at.is_some());

    // A post after the session ended must not reset startedAt or resurrect it.
    seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    let later = state.sessions.into_iter().find(|s| s.id == "s1").unwrap();
    assert_eq!(later.started_at, first.started_at);
    assert_eq!(later.ended_at, ended.ended_at);
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
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": session_id, "cwd": cwd, "agent": "claude-code", "html": "<p>x</p>"}),
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
        .oneshot(delete(&format!("/api/cards/{}", card.id)))
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
        .oneshot(delete("/api/cards/nope"))
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
        .oneshot(delete("/api/sessions/s1"))
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
async fn delete_unknown_session_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(delete("/api/sessions/nope"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn clear_cards_removes_only_that_sessions_cards_and_keeps_it_registered() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let a1 = seed_card(&app, "s1", "/tmp/proj").await;
    let a2 = seed_card(&app, "s1", "/tmp/proj").await;
    let other = seed_card(&app, "s2", "/tmp/proj2").await;
    let mut events = state.events.subscribe();

    let response = app
        .clone()
        .oneshot(delete("/api/sessions/s1/cards"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app.clone().oneshot(get("/api/state")).await.unwrap();
    let state: StateResponse = json_body(response).await;
    assert!(state.sessions.iter().any(|s| s.id == "s1"));
    assert!(state
        .sessions
        .iter()
        .find(|s| s.id == "s1")
        .unwrap()
        .ended_at
        .is_none());
    assert!(state.cards.iter().all(|c| c.session_id != "s1"));
    assert!(state.cards.iter().any(|c| c.id == other.id));

    let mut removed = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            CanvasEvent::CardRemoved(id) => removed.push(id),
            other => panic!("unexpected event {other:?}"),
        }
    }
    removed.sort();
    let mut expected = vec![a1.id, a2.id];
    expected.sort();
    assert_eq!(removed, expected);
}

#[tokio::test]
async fn clear_cards_on_a_session_whose_cards_were_all_deleted_is_204_with_no_events() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    app.clone()
        .oneshot(delete(&format!("/api/cards/{}", card.id)))
        .await
        .unwrap();
    let mut events = state.events.subscribe();

    let response = app
        .clone()
        .oneshot(delete("/api/sessions/s1/cards"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn clear_cards_on_an_unknown_session_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(delete("/api/sessions/nope/cards"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

const KIND: &str = "posting-guidance";

#[tokio::test]
async fn profiles_default_to_no_assignment() {
    let response = app()
        .oneshot(get(&format!("/api/profiles/{KIND}")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let p: ProfilesState = json_body(response).await;
    assert!(p.profiles.is_empty());
    assert!(p.global.is_none());

    let response = app()
        .oneshot(get(&format!(
            "/api/profiles/{KIND}/effective?cwd=/tmp/proj"
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let e: EffectiveProfile = json_body(response).await;
    assert!(e.profiles.is_empty());
    assert!(e.text.is_none());
}

/// The compiled-in default is only meaningful for the one kind that ships
/// with one — an arbitrary kind string has no builtin to show read-only.
#[tokio::test]
async fn only_posting_guidance_reports_a_builtin_default() {
    let response = app()
        .oneshot(get(&format!("/api/profiles/{KIND}")))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert!(p.builtin.is_some_and(|t| !t.trim().is_empty()));

    let response = app()
        .oneshot(get("/api/profiles/some-other-kind"))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert!(p.builtin.is_none());
}

#[tokio::test]
async fn stop_triggers_profiles_must_parse_and_have_a_built_in_default() {
    let app = app();
    let response = app
        .clone()
        .oneshot(put(
            "/api/profiles/stop-triggers/definitions",
            json!({"name": "loud", "text": "image\nlinks many"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).starts_with("line 2:"));

    let response = app
        .clone()
        .oneshot(put(
            "/api/profiles/stop-triggers/definitions",
            json!({"name": "quiet", "text": "off"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(get("/api/profiles/stop-triggers"))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert!(p.builtin.unwrap().contains("image"));
    assert!(p.profiles.contains_key("quiet") && !p.profiles.contains_key("loud"));
}

#[tokio::test]
async fn global_profile_assignment_applies_to_every_repo() {
    let app = app();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "chatty", "text": "post more, always"}),
        ))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/global"),
            json!({"profile": "chatty"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/profiles/{KIND}/effective?cwd=/tmp/proj"
        )))
        .await
        .unwrap();
    let e: EffectiveProfile = json_body(response).await;
    assert_eq!(e.text.as_deref(), Some("post more, always"));
}

#[tokio::test]
async fn replace_mode_repo_assignment_wins_over_global_for_that_repo_only() {
    let app = app();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "default", "text": "global text"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "canvas", "text": "canvas-specific text"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/global"),
            json!({"profile": "default"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/repos"),
            json!({"repo": "acme/canvas", "profile": "canvas"}),
        ))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/mode"),
            json!({"mode": "replace"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/profiles/{KIND}/effective?repo=acme/canvas"
        )))
        .await
        .unwrap();
    let e: EffectiveProfile = json_body(response).await;
    assert_eq!(e.text.as_deref(), Some("canvas-specific text"));
    assert_eq!(e.profiles, vec!["canvas"]);

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/profiles/{KIND}/effective?repo=someone/other-repo"
        )))
        .await
        .unwrap();
    let e: EffectiveProfile = json_body(response).await;
    assert_eq!(e.text.as_deref(), Some("global text"));
}

/// The hook path must not carry every profile's text — only the effective one.
#[tokio::test]
async fn effective_response_omits_the_profile_set() {
    let app = app();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "default", "text": "global text"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "other", "text": "unrelated text"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/global"),
            json!({"profile": "default"}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(get(&format!("/api/profiles/{KIND}/effective")))
        .await
        .unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8(body.to_vec()).unwrap();
    assert!(raw.contains("global text"));
    assert!(!raw.contains("unrelated text"));
}

#[tokio::test]
async fn blank_text_deletes_a_profile_and_clears_its_assignments() {
    let app = app();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "chatty", "text": "something"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/global"),
            json!({"profile": "chatty"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/definitions"),
            json!({"name": "chatty", "text": "   "}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(get(&format!("/api/profiles/{KIND}")))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert!(p.profiles.is_empty());
    assert!(p.global.is_none());
}

#[tokio::test]
async fn different_kinds_keep_independent_profile_sets() {
    let app = app();
    app.clone()
        .oneshot(put(
            "/api/profiles/posting-guidance/definitions",
            json!({"name": "a", "text": "guidance text"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            "/api/profiles/some-other-kind/definitions",
            json!({"name": "a", "text": "unrelated text"}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(get("/api/profiles/posting-guidance"))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert_eq!(
        p.profiles.get("a").map(String::as_str),
        Some("guidance text")
    );

    let response = app
        .clone()
        .oneshot(get("/api/profiles/some-other-kind"))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert_eq!(
        p.profiles.get("a").map(String::as_str),
        Some("unrelated text")
    );
}

#[tokio::test]
async fn assigning_a_profile_name_that_does_not_exist_is_rejected() {
    let app = app();

    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/global"),
            json!({"profile": "nonexistent"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = app
        .clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/repos"),
            json!({"repo": "acme/canvas", "profile": "nonexistent"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = app
        .clone()
        .oneshot(get(&format!("/api/profiles/{KIND}")))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert!(p.global.is_none());
    assert!(p.repos.is_empty());
}

#[tokio::test]
async fn card_reply_round_trips() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    let response = app
        .clone()
        .oneshot(get(&format!("/api/cards/{}/reply", card.id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .clone()
        .oneshot(post(&format!("/api/cards/{}/reply", card.id), json!("A")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(get(&format!("/api/cards/{}/reply", card.id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!("A"));
}

#[tokio::test]
async fn card_reply_last_write_wins() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    for value in ["A", "B"] {
        let response = app
            .clone()
            .oneshot(post(&format!("/api/cards/{}/reply", card.id), json!(value)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let response = app
        .clone()
        .oneshot(get(&format!("/api/cards/{}/reply", card.id)))
        .await
        .unwrap();
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!("B"));
}

#[tokio::test]
async fn card_reply_on_unknown_card_is_404() {
    let app = app();

    let response = app
        .clone()
        .oneshot(post("/api/cards/does-not-exist/reply", json!("A")))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .clone()
        .oneshot(get("/api/cards/does-not-exist/reply"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn card_data_round_trips_and_the_last_write_wins() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let uri = format!("/api/cards/{}/data", card.id);

    let response = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    for value in [json!({"n": 1}), json!({"n": 2, "rows": [1, 2, 3]})] {
        let response = app.clone().oneshot(put(&uri, value)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let response = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!({"n": 2, "rows": [1, 2, 3]}));
}

#[tokio::test]
async fn card_data_reaches_viewers_as_a_card_data_event() {
    use futures::StreamExt;

    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut body = response.into_body().into_data_stream();

    app.clone()
        .oneshot(put(
            &format!("/api/cards/{}/data", card.id),
            json!({"n": 7}),
        ))
        .await
        .unwrap();

    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("event: card-data"), "{text}");
    assert!(
        text.contains(&card.id) && text.contains("\"n\":7"),
        "{text}"
    );
}

#[tokio::test]
async fn card_data_on_unknown_card_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(put("/api/cards/does-not-exist/data", json!({"n": 1})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn oversized_card_data_is_rejected() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let uri = format!("/api/cards/{}/data", card.id);

    let response = app
        .clone()
        .oneshot(put(&uri, json!("x".repeat(300 * 1024))))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let response = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_card_drops_its_data() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let uri = format!("/api/cards/{}/data", card.id);
    app.clone().oneshot(put(&uri, json!(1))).await.unwrap();

    app.clone()
        .oneshot(delete(&format!("/api/cards/{}", card.id)))
        .await
        .unwrap();
    let response = app.clone().oneshot(get(&uri)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn oversized_card_reply_is_rejected() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;

    let huge = "x".repeat(5000);
    let response = app
        .clone()
        .oneshot(post(&format!("/api/cards/{}/reply", card.id), json!(huge)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let response = app
        .clone()
        .oneshot(get(&format!("/api/cards/{}/reply", card.id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn card_reply_is_dropped_when_its_card_is_evicted_from_the_ring() {
    let state = AppState::new();
    let app = canvasd::build_router(state.clone());
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": "<p>0</p>"}),
        ))
        .await
        .unwrap();
    let first: Card = json_body(response).await;
    app.clone()
        .oneshot(post(&format!("/api/cards/{}/reply", first.id), json!("A")))
        .await
        .unwrap();
    assert_eq!(state.inner.read().await.replies.len(), 1);

    for i in 1..501 {
        app.clone()
            .oneshot(post(
                "/api/posts",
                json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": format!("<p>{i}</p>")}),
            ))
            .await
            .unwrap();
    }

    // The ring evicted the first card at 501; its reply shouldn't outlive it.
    assert!(state.inner.read().await.replies.is_empty());
}

/// Additive is the default: a repo's profile is appended to the global one.
#[tokio::test]
async fn additive_mode_joins_global_and_repo_text() {
    let app = app();
    for (name, text) in [("default", "global text"), ("canvas", "canvas text")] {
        app.clone()
            .oneshot(put(
                &format!("/api/profiles/{KIND}/definitions"),
                json!({"name": name, "text": text}),
            ))
            .await
            .unwrap();
    }
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/global"),
            json!({"profile": "default"}),
        ))
        .await
        .unwrap();
    app.clone()
        .oneshot(put(
            &format!("/api/profiles/{KIND}/repos"),
            json!({"repo": "o/canvas", "profile": "canvas"}),
        ))
        .await
        .unwrap();

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/profiles/{KIND}/effective?repo=o/canvas"
        )))
        .await
        .unwrap();
    let e: EffectiveProfile = json_body(response).await;
    assert_eq!(e.text.as_deref(), Some("global text\n\ncanvas text"));
    assert_eq!(e.profiles, vec!["default", "canvas"]);

    let response = app
        .clone()
        .oneshot(get(&format!("/api/profiles/{KIND}")))
        .await
        .unwrap();
    let p: ProfilesState = json_body(response).await;
    assert_eq!(p.mode, ProfileMode::Additive);
}

#[tokio::test]
async fn no_pins_route_exists_and_a_pin_in_a_post_is_ignored() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post(
            "/api/posts",
            json!({
                "session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code",
                "html": "<p>x</p>", "pin": {"slot": "status"},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let card: serde_json::Value = json_body(response).await;
    assert!(card.get("pin").is_none(), "{card}");
    let id = card["id"].as_str().unwrap();

    let pins = app
        .clone()
        .oneshot(get("/api/pins?cwd=/tmp/proj&slot=status"))
        .await
        .unwrap();
    assert_eq!(pins.status(), StatusCode::NOT_FOUND);
    let unpin = app
        .clone()
        .oneshot(delete(&format!("/api/cards/{id}/pin")))
        .await
        .unwrap();
    assert_eq!(unpin.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_dead_agent_process_ends_its_session() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    for (session, pid) in [("s1", Some(child.id())), ("s2", None)] {
        let response = app
            .clone()
            .oneshot(post(
                "/api/posts",
                json!({
                    "session_id": session, "cwd": std::env::temp_dir(), "agent": "claude-code",
                    "html": "<p>x</p>", "pid": pid,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    assert_eq!(
        state
            .sweep_dead_sessions(canvasd::state::process_alive)
            .await,
        0
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        state
            .sweep_dead_sessions(canvasd::state::process_alive)
            .await,
        1
    );

    let inner = state.inner.read().await;
    assert!(inner.sessions["s1"].ended_at.is_some());
    assert!(inner.sessions["s2"].ended_at.is_none());
    drop(inner);
    // Already ended: a second sweep finds nothing.
    assert_eq!(
        state
            .sweep_dead_sessions(canvasd::state::process_alive)
            .await,
        0
    );
}

#[tokio::test]
async fn focus_reaches_viewers_as_a_card_focus_event_naming_the_card() {
    use futures::StreamExt;

    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut body = response.into_body().into_data_stream();

    let response = app
        .clone()
        .oneshot(post(&format!("/api/cards/{}/focus", card.id), json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!({"viewers": 1}));

    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("event: card-focus"), "{text}");
    assert!(
        text.contains(&format!("{{\"id\":\"{}\"}}", card.id)),
        "{text}"
    );
}

#[tokio::test]
async fn focus_with_no_viewer_reports_zero() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app
        .clone()
        .oneshot(post(&format!("/api/cards/{}/focus", card.id), json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!({"viewers": 0}));
}

#[tokio::test]
async fn focus_on_unknown_card_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post("/api/cards/does-not-exist/focus", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn theme_set_reaches_viewers_as_a_theme_set_event() {
    use futures::StreamExt;

    let app = app();
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut body = response.into_body().into_data_stream();

    let response = app
        .clone()
        .oneshot(post("/api/theme", json!({"theme": "dark"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!({"viewers": 1}));

    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("event: theme-set"), "{text}");
    assert!(text.contains(r#"{"theme":"dark"}"#), "{text}");
}

#[tokio::test]
async fn theme_set_with_no_viewer_reports_zero() {
    let response = app()
        .oneshot(post("/api/theme", json!({"theme": "light"})))
        .await
        .unwrap();
    let value: serde_json::Value = json_body(response).await;
    assert_eq!(value, json!({"viewers": 0}));
}

#[tokio::test]
async fn theme_set_refuses_a_theme_that_is_not_light_or_dark() {
    let response = app()
        .oneshot(post("/api/theme", json!({"theme": "sepia"})))
        .await
        .unwrap();
    assert!(response.status().is_client_error(), "{}", response.status());
}

#[tokio::test]
async fn theme_reads_back_what_the_viewer_last_reported() {
    let app = app();
    let viewer = app.clone().oneshot(get("/api/events")).await.unwrap();
    let value: serde_json::Value =
        json_body(app.clone().oneshot(get("/api/theme")).await.unwrap()).await;
    assert_eq!(value, json!({"theme": null}));

    let response = app
        .clone()
        .oneshot(put("/api/theme", json!({"theme": "dark"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let value: serde_json::Value =
        json_body(app.clone().oneshot(get("/api/theme")).await.unwrap()).await;
    assert_eq!(value, json!({"theme": "dark"}));

    drop(viewer);
    let value: serde_json::Value =
        json_body(app.clone().oneshot(get("/api/theme")).await.unwrap()).await;
    assert_eq!(value, json!({"theme": null}));
}

#[tokio::test]
async fn artifact_pane_reads_back_what_the_viewer_last_reported() {
    let app = app();
    let viewer = app.clone().oneshot(get("/api/events")).await.unwrap();
    let value: serde_json::Value = json_body(
        app.clone()
            .oneshot(get("/api/artifact-pane"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value, json!({"pane": null}));

    let report = json!({
        "id": "art-0123456789",
        "width": 960,
        "height": 600,
        "full": false,
        "chosen": {"width": 960, "height": 600},
    });
    let response = app
        .clone()
        .oneshot(put("/api/artifact-pane", report.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let value: serde_json::Value = json_body(
        app.clone()
            .oneshot(get("/api/artifact-pane"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value, json!({"pane": report}));

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::delete("/api/artifact-pane")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let value: serde_json::Value = json_body(
        app.clone()
            .oneshot(get("/api/artifact-pane"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value, json!({"pane": null}));

    app.clone()
        .oneshot(put("/api/artifact-pane", report.clone()))
        .await
        .unwrap();
    drop(viewer);
    let value: serde_json::Value = json_body(
        app.clone()
            .oneshot(get("/api/artifact-pane"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value, json!({"pane": null}));
}

#[tokio::test]
async fn artifact_pane_refuses_an_empty_size_and_an_unknown_artifact() {
    let app = app();
    let zero = json!({"action": "resize", "width": 0, "height": 600});
    let response = app
        .clone()
        .oneshot(post("/api/artifacts/art-0123456789/pane", zero))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .clone()
        .oneshot(post(
            "/api/artifacts/art-0123456789/pane",
            json!({"action": "full"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .clone()
        .oneshot(post(
            "/api/artifacts/art-0123456789/pane",
            json!({"action": "spin"}),
        ))
        .await
        .unwrap();
    assert!(response.status().is_client_error(), "{}", response.status());
}

/// Plays the viewer's half of a snapshot: reads the `card-snapshot` event off
/// an open `/api/events` body and answers its request with `content_type` and
/// `body`. Returns the event's data.
async fn answer_snapshot(
    app: &axum::Router,
    events: &mut (impl futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
    content_type: &str,
    body: Vec<u8>,
) -> serde_json::Value {
    use futures::StreamExt;

    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("event: card-snapshot"), "{text}");
    let data = text
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("event has data");
    let data: serde_json::Value = serde_json::from_str(data).unwrap();
    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/snapshots/{}",
            data["request"].as_str().unwrap()
        ))
        .header("content-type", content_type)
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    data
}

#[tokio::test]
async fn snapshot_answers_the_png_a_viewer_sends_back() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut events = response.into_body().into_data_stream();

    let waiting = tokio::spawn(
        app.clone()
            .oneshot(post(&format!("/api/cards/{}/snapshot", card.id), json!({}))),
    );
    let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
    let data = answer_snapshot(&app, &mut events, "image/png", png.clone()).await;
    assert_eq!(data["id"], card.id.as_str());

    let response = waiting.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/png");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.to_vec(), png);
}

#[tokio::test]
async fn snapshot_relays_the_reason_a_viewer_could_not_capture() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut events = response.into_body().into_data_stream();

    let waiting = tokio::spawn(
        app.clone()
            .oneshot(post(&format!("/api/cards/{}/snapshot", card.id), json!({}))),
    );
    let reason = json!({"error": "the card is hidden by the search"});
    answer_snapshot(
        &app,
        &mut events,
        "application/json",
        reason.to_string().into_bytes(),
    )
    .await;

    let response = waiting.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"the card is hidden by the search");
}

#[tokio::test]
async fn snapshot_with_no_viewer_is_409() {
    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app
        .clone()
        .oneshot(post(&format!("/api/cards/{}/snapshot", card.id), json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn snapshot_of_unknown_card_is_404_and_a_stray_answer_is_404() {
    let app = app();
    let response = app
        .clone()
        .oneshot(post("/api/cards/does-not-exist/snapshot", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .clone()
        .oneshot(post(
            "/api/snapshots/no-such-request",
            json!({"error": "x"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn snapshot_passes_on_that_the_card_was_clipped() {
    use futures::StreamExt;

    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut events = response.into_body().into_data_stream();
    let waiting = tokio::spawn(
        app.clone()
            .oneshot(post(&format!("/api/cards/{}/snapshot", card.id), json!({}))),
    );
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    let data: serde_json::Value =
        serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
    let answer = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/snapshots/{}",
            data["request"].as_str().unwrap()
        ))
        .header("content-type", "image/png")
        .header("x-canvas-clipped", "true")
        .body(Body::from(b"\x89PNG".to_vec()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(answer).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );

    let response = waiting.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-canvas-clipped"], "true");
}

#[tokio::test]
async fn a_snapshot_request_the_cli_abandons_stops_waiting() {
    use futures::StreamExt;

    let app = app();
    let card = seed_card(&app, "s1", "/tmp/proj").await;
    let response = app.clone().oneshot(get("/api/events")).await.unwrap();
    let mut events = response.into_body().into_data_stream();
    let waiting = tokio::spawn(
        app.clone()
            .oneshot(post(&format!("/api/cards/{}/snapshot", card.id), json!({}))),
    );
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    let data: serde_json::Value =
        serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
    waiting.abort();
    let _ = waiting.await;

    let answer = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/snapshots/{}",
            data["request"].as_str().unwrap()
        ))
        .header("content-type", "image/png")
        .body(Body::from(b"\x89PNG".to_vec()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(answer).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}
