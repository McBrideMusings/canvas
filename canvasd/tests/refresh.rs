use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
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

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
    pid: Option<u32>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(pid) = pid {
        req = req.header("x-canvas-pid", pid.to_string());
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A daemon with a data dir and one artifact created with `extras` (the
/// widget and refresh fields of `POST /api/artifacts`).
async fn artifact_with(extras: Value, pid: Option<u32>) -> (AppState, axum::Router, String) {
    let state = AppState::open(&temp_dir()).await;
    let app = build_router(state.clone());
    let (status, created) = send(&app, "POST", "/api/artifacts", extras, pid).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    (state, app, created["id"].as_str().unwrap().to_string())
}

fn refresh(command: &str) -> Value {
    json!({
        "refresh": {"command": command, "everySecs": 5},
        "cwd": std::env::temp_dir(),
    })
}

/// Ticks the scheduler until `done` holds for the artifact's view, up to 5s.
async fn tick_until(state: &AppState, id: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..50 {
        state.refresh_tick_with(|_| true).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let artifacts = state.artifacts.read().await;
        let view = serde_json::to_value(artifacts.view(&artifacts.records[id])).unwrap();
        if done(&view) {
            return view;
        }
    }
    panic!("refresh never reached the expected state");
}

#[tokio::test]
async fn a_refresh_printing_json_delivers_it_as_artifact_data() {
    let (state, _app, id) = artifact_with(refresh("echo '{\"n\":1}'"), None).await;
    let mut events = state.events.subscribe();
    tick_until(&state, &id, |v| v["data"] == json!({"n": 1})).await;
    let event = events.recv().await.unwrap();
    assert!(
        matches!(event, canvasd::state::CanvasEvent::ArtifactData { ref value, .. } if value == &json!({"n": 1})),
        "{event:?}"
    );
}

#[tokio::test]
async fn a_failing_refresh_records_the_error_and_keeps_the_last_data() {
    let (state, _app, id) = artifact_with(refresh("echo boom >&2; exit 1"), None).await;
    state
        .artifacts
        .write()
        .await
        .data
        .insert(id.clone(), json!({"last": "good"}));
    let view = tick_until(&state, &id, |v| v["refreshError"]["message"] == "boom").await;
    assert_eq!(view["data"], json!({"last": "good"}));
    // The first failure waits twice the interval.
    let at = chrono::DateTime::parse_from_rfc3339(view["refreshError"]["at"].as_str().unwrap());
    let retry =
        chrono::DateTime::parse_from_rfc3339(view["refreshError"]["retryAt"].as_str().unwrap());
    assert_eq!((retry.unwrap() - at.unwrap()).num_seconds(), 10);
}

#[tokio::test]
async fn a_success_clears_the_error() {
    let marker = temp_dir().join("ok");
    let command = format!(
        "if [ -e {0} ]; then echo 2; else touch {0}; exit 1; fi",
        marker.display()
    );
    let (state, _app, id) = artifact_with(refresh(&command), None).await;
    tick_until(&state, &id, |v| v["refreshError"].is_object()).await;
    // Backoff holds the next run 10s off; a new interval restarts the loop.
    state
        .artifacts
        .write()
        .await
        .records
        .get_mut(&id)
        .unwrap()
        .refresh
        .as_mut()
        .unwrap()
        .every_secs = 6;
    let view = tick_until(&state, &id, |v| v["data"] == json!(2)).await;
    assert!(view["refreshError"].is_null(), "{view}");
}

#[tokio::test]
async fn output_that_is_not_json_is_an_error() {
    let err = canvasd::refresh::run_command("echo hello", "/").await;
    assert_eq!(err, Err("output is not JSON".to_string()));
}

#[tokio::test]
async fn an_artifact_with_no_refresh_spawns_nothing() {
    let (state, _app, id) = artifact_with(json!({}), None).await;
    state.refresh_tick_with(|_| true).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!state.artifacts.read().await.data.contains_key(&id));
    assert_eq!(state.refreshing_count(), 0);
}

#[tokio::test]
async fn a_refresh_under_the_minimum_interval_or_without_a_cwd_is_refused() {
    let app = build_router(AppState::open(&temp_dir()).await);
    let short = json!({"refresh": {"command": "true", "everySecs": 1}, "cwd": "/"});
    let (status, _) = send(&app, "POST", "/api/artifacts", short, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let no_cwd = json!({"refresh": {"command": "true", "everySecs": 5}});
    let (status, _) = send(&app, "POST", "/api/artifacts", no_cwd, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_refresh_records_its_cwd_and_pid_and_stops_with_that_process() {
    let (state, _app, id) = artifact_with(refresh("echo 1"), Some(4242)).await;
    {
        let artifacts = state.artifacts.read().await;
        let record = &artifacts.records[&id];
        let refresh = record.refresh.as_ref().unwrap();
        assert_eq!(refresh.pid, Some(4242));
        assert_eq!(PathBuf::from(&refresh.cwd), std::env::temp_dir());
    }
    state.artifacts.write().await.refresh_errors.insert(
        id.clone(),
        canvas_core::RefreshError {
            message: "boom".to_string(),
            at: String::new(),
            retry_at: String::new(),
        },
    );
    state.refresh_tick_with(|pid| pid != 4242).await;
    assert_eq!(state.refreshing_count(), 0);
    // Nothing will retry it, so its error goes.
    assert!(!state.artifacts.read().await.refresh_errors.contains_key(&id));
    state.refresh_tick_with(|_| true).await;
    assert_eq!(state.refreshing_count(), 1);
}

#[tokio::test]
async fn put_replaces_the_widget_and_refresh_and_keeps_them_when_absent() {
    let dir = temp_dir();
    let app = build_router(AppState::open(&dir).await);
    let extras = json!({
        "widget_html": "<b>one</b>",
        "refresh": {"command": "echo 1", "everySecs": 5},
        "cwd": "/",
    });
    let (_, created) = send(&app, "POST", "/api/artifacts", extras, None).await;
    let id = created["id"].as_str().unwrap();
    let source = temp_dir().join("index.html");
    std::fs::write(&source, "<p>x</p>").unwrap();
    let uri = format!("/api/artifacts/{id}/put");
    let (status, view) = send(&app, "POST", &uri, json!({"source": source}), None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["widgetHtml"], "<b>one</b>");
    assert_eq!(view["refresh"]["command"], "echo 1");
    let body = json!({"source": source, "widget_html": "<b>two</b>"});
    let (_, view) = send(&app, "POST", &uri, body, None).await;
    assert_eq!(view["widgetHtml"], "<b>two</b>");
    let saved = std::fs::read_to_string(dir.join("artifacts.json")).unwrap();
    assert!(saved.contains("<b>two</b>"), "{saved}");
}

#[tokio::test]
async fn canvas_data_reaches_an_artifact_and_404s_for_an_unknown_one() {
    let (state, app, id) = artifact_with(json!({}), None).await;
    let uri = format!("/api/artifacts/{id}/data");
    let (status, body) = send(&app, "PUT", &uri, json!({"cache": 81}), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"viewers": 0}));
    assert_eq!(
        state.artifacts.read().await.data.get(&id),
        Some(&json!({"cache": 81}))
    );
    let (status, _) = send(&app, "PUT", "/api/artifacts/art-0/data", json!(1), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
