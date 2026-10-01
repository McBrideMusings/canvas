use axum::body::Body;
use axum::http::{Request, StatusCode};
use canvas_core::Card;
use canvasd::build_router;
use canvasd::state::AppState;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

async fn post_pin(app: &axum::Router, pin: serde_json::Value) -> Card {
    let body = json!({
        "session_id": "s", "cwd": std::env::temp_dir(), "agent": "claude-code",
        "html": "<p>x</p>", "pin": pin,
    });
    let request = Request::builder()
        .method("POST")
        .uri("/api/posts")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Ticks the scheduler until `done` holds for the card's state, up to 5s.
async fn tick_until(
    state: &AppState,
    id: &str,
    done: impl Fn(&Card, Option<&serde_json::Value>) -> bool,
) {
    for _ in 0..50 {
        state.refresh_tick().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let inner = state.inner.read().await;
        let card = inner.cards.iter().find(|c| c.id == id).unwrap();
        if done(card, inner.data.get(id)) {
            return;
        }
    }
    panic!("refresh never reached the expected state");
}

#[tokio::test]
async fn a_refresh_printing_json_delivers_it_as_card_data() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let card = post_pin(
        &app,
        json!({"slot": "t", "refresh": {"command": "echo '{\"n\":1}'", "everySecs": 5}}),
    )
    .await;
    tick_until(&state, &card.id, |_, data| data == Some(&json!({"n": 1}))).await;
}

#[tokio::test]
async fn a_failing_refresh_records_the_error_and_keeps_the_last_data() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let card = post_pin(
        &app,
        json!({"slot": "t", "refresh": {"command": "echo boom >&2; exit 1", "everySecs": 5}}),
    )
    .await;
    state
        .inner
        .write()
        .await
        .data
        .insert(card.id.clone(), json!({"last": "good"}));
    tick_until(&state, &card.id, |c, _| {
        c.pin.as_ref().and_then(|p| p.refresh_error.as_deref()) == Some("boom")
    })
    .await;
    let inner = state.inner.read().await;
    assert_eq!(inner.data.get(&card.id), Some(&json!({"last": "good"})));
}

#[tokio::test]
async fn output_that_is_not_json_is_an_error() {
    let err = canvasd::refresh::run_command("echo hello", "/").await;
    assert_eq!(err, Err("output is not JSON".to_string()));
}

#[tokio::test]
async fn a_pin_with_no_refresh_spawns_nothing() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let card = post_pin(&app, json!({"slot": "t"})).await;
    state.refresh_tick().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!state.inner.read().await.data.contains_key(&card.id));
    assert!(state.refreshing_count() == 0);
}

#[tokio::test]
async fn a_refresh_under_the_minimum_interval_is_refused() {
    let app = build_router(AppState::new());
    let body = json!({
        "session_id": "s", "cwd": "/", "agent": "claude-code", "html": "x",
        "pin": {"slot": "t", "refresh": {"command": "true", "everySecs": 1}},
    });
    let request = Request::builder()
        .method("POST")
        .uri("/api/posts")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_pin_that_ends_stops_its_loop() {
    let state = AppState::new();
    let app = build_router(state.clone());
    let card = post_pin(
        &app,
        json!({"slot": "t", "refresh": {"command": "echo 1", "everySecs": 5}}),
    )
    .await;
    tick_until(&state, &card.id, |_, data| data.is_some()).await;
    {
        let mut inner = state.inner.write().await;
        let mut unpinned = inner
            .cards
            .iter()
            .find(|c| c.id == card.id)
            .unwrap()
            .clone();
        unpinned.pin = None;
        inner.upsert_card(unpinned);
    }
    state.refresh_tick().await;
    assert!(state.refreshing_count() == 0);
}

#[test]
fn failures_double_the_wait_up_to_ten_minutes() {
    use canvasd::refresh::delay_secs;
    assert_eq!(delay_secs(5, 0), 5);
    assert_eq!(delay_secs(5, 1), 10);
    assert_eq!(delay_secs(5, 3), 40);
    assert_eq!(delay_secs(5, 20), 600);
    assert_eq!(delay_secs(900, 2), 900);
}

#[tokio::test]
async fn output_over_the_cap_is_an_error_not_buffered() {
    let err = canvasd::refresh::run_command("yes", "/").await;
    assert_eq!(err, Err("output is over 256 KB".to_string()));
}
