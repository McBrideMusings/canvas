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
    assert!(p.builtin.unwrap().contains("long-block 15"));
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
