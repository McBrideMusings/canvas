use std::path::PathBuf;
use std::time::{Duration, Instant};

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

fn always(command: &str) -> Value {
    json!({
        "refresh": {"command": command, "everySecs": 5, "always": true},
        "cwd": std::env::temp_dir(),
    })
}

/// One viewer window reporting that it shows `ids`.
async fn watch(state: &AppState, ids: &[&str]) {
    state
        .record_watch(canvas_core::WatchReport {
            window: "w1".to_string(),
            visible: true,
            artifacts: ids.iter().map(|i| i.to_string()).collect(),
        })
        .await;
}

async fn view_of(state: &AppState, id: &str) -> Value {
    let artifacts = state.artifacts.read().await;
    serde_json::to_value(artifacts.view(&artifacts.records[id])).unwrap()
}

/// Ticks the scheduler, with a window watching the artifact, until `done`
/// holds for its view, up to 5s.
async fn tick_until(state: &AppState, id: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..50 {
        watch(state, &[id]).await;
        state.refresh_tick().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let view = view_of(state, id).await;
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
    // The run's start and end reach the viewers as upserts around the data.
    let mut kinds = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            canvasd::state::CanvasEvent::ArtifactData { value, .. } => {
                assert_eq!(value, json!({"n": 1}));
                kinds.push("data");
            }
            canvasd::state::CanvasEvent::ArtifactUpserted(view) => {
                kinds.push(if view.refreshing { "started" } else { "ended" });
            }
            _ => {}
        }
    }
    assert_eq!(kinds[..3], ["started", "data", "ended"], "{kinds:?}");
}

#[tokio::test]
async fn a_failing_refresh_records_the_error_and_keeps_the_last_data() {
    // `always` skips the first-watch hold-off a freshly seeded value gets.
    let (state, _app, id) = artifact_with(always("echo boom >&2; exit 1"), None).await;
    state
        .artifacts
        .write()
        .await
        .set_data(&id, Some(json!({"last": "good"})));
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
    assert_eq!(
        err,
        canvasd::refresh::Outcome::Failed("output is not JSON".to_string())
    );
}

#[tokio::test]
async fn an_artifact_with_no_refresh_spawns_nothing() {
    let (state, _app, id) = artifact_with(json!({}), None).await;
    watch(&state, &[&id]).await;
    state.refresh_tick().await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!state.artifacts.read().await.data.contains_key(&id));
    assert_eq!(state.artifacts.read().await.refresh_count(), 0);
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
async fn the_refresh_records_its_cwd_and_outlives_the_agent_that_set_it() {
    let marker = temp_dir().join("ran");
    let command = format!("touch {}; echo 1", marker.display());
    let (state, _app, id) = artifact_with(refresh(&command), Some(4242)).await;
    {
        let artifacts = state.artifacts.read().await;
        let saved = serde_json::to_value(&artifacts.records[&id]).unwrap();
        assert_eq!(saved["refresh"]["always"], false);
        assert!(saved["refresh"].get("pid").is_none(), "{saved}");
        let refresh = artifacts.records[&id].refresh.as_ref().unwrap();
        assert_eq!(PathBuf::from(&refresh.cwd), std::env::temp_dir());
    }
    // pid 4242 is no process; the refresh runs regardless.
    tick_until(&state, &id, |v| v["data"] == json!(1)).await;
    assert!(marker.exists());
}

/// Ticks at `now` and gives any run it started a moment to finish.
async fn tick_at(state: &AppState, now: Instant) {
    state.refresh_tick_at(now).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

fn runs(dir: &std::path::Path) -> usize {
    std::fs::read_to_string(dir.join("runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// A refresh that appends a line to `dir/runs` on every run.
fn counting(dir: &std::path::Path, always_on: bool) -> Value {
    let command = format!("echo x >> {}; echo 1", dir.join("runs").display());
    if always_on {
        always(&command)
    } else {
        refresh(&command)
    }
}

#[tokio::test]
async fn an_unwatched_refresh_never_runs_and_a_watched_one_runs_at_once() {
    let dir = temp_dir();
    let (state, _app, id) = artifact_with(counting(&dir, false), None).await;
    let start = Instant::now();
    // Five simulated minutes with no window showing it.
    for secs in (0..300).step_by(5) {
        tick_at(&state, start + Duration::from_secs(secs)).await;
    }
    assert_eq!(runs(&dir), 0);
    assert_eq!(view_of(&state, &id).await["watched"], false);
    assert!(view_of(&state, &id).await["lastRunAt"].is_null());

    watch(&state, &[&id]).await;
    assert_eq!(view_of(&state, &id).await["watched"], true);
    tick_at(&state, Instant::now()).await;
    assert_eq!(runs(&dir), 1);
    let view = view_of(&state, &id).await;
    assert_eq!(view["data"], json!(1));
    assert!(view["lastRunAt"].is_string(), "{view}");
    assert!(view["dataAt"].is_string(), "{view}");
}

#[tokio::test]
async fn a_lapsed_watch_stops_runs() {
    let dir = temp_dir();
    let (state, _app, id) = artifact_with(counting(&dir, false), None).await;
    watch(&state, &[&id]).await;
    tick_at(&state, Instant::now()).await;
    assert_eq!(runs(&dir), 1);
    // The report is 31s old: due, but nothing watches it any more.
    let later = Instant::now() + Duration::from_secs(31);
    tick_at(&state, later).await;
    tick_at(&state, later + Duration::from_secs(10)).await;
    assert_eq!(runs(&dir), 1);
    // A window that reports an empty list stops watching at once.
    watch(&state, &[]).await;
    assert_eq!(view_of(&state, &id).await["watched"], false);
}

#[tokio::test]
async fn an_always_refresh_runs_with_no_window_at_its_interval() {
    let dir = temp_dir();
    let (state, _app, _id) = artifact_with(counting(&dir, true), None).await;
    let start = Instant::now();
    tick_at(&state, start).await;
    assert_eq!(runs(&dir), 1);
    // Not due again until five seconds after that run ended.
    tick_at(&state, Instant::now() + Duration::from_secs(2)).await;
    assert_eq!(runs(&dir), 1);
    tick_at(&state, Instant::now() + Duration::from_secs(6)).await;
    assert_eq!(runs(&dir), 2);
}

#[tokio::test]
async fn empty_output_skips_and_keeps_the_last_value() {
    // `always` skips the first-watch hold-off a freshly seeded value gets.
    let (state, _app, id) = artifact_with(always("true"), None).await;
    state
        .artifacts
        .write()
        .await
        .set_data(&id, Some(json!({"last": "good"})));
    let view = tick_until(&state, &id, |v| v["lastRunAt"].is_string()).await;
    assert_eq!(view["data"], json!({"last": "good"}));
    assert!(view["refreshError"].is_null(), "{view}");
    assert_eq!(
        canvasd::refresh::run_command("printf '  \\n'", "/").await,
        canvasd::refresh::Outcome::Skip
    );
}

#[tokio::test]
async fn stopping_a_refresh_removes_it_and_its_error() {
    let (state, app, id) = artifact_with(refresh("exit 1"), None).await;
    tick_until(&state, &id, |v| v["refreshError"].is_object()).await;
    let uri = format!("/api/artifacts/{id}/refresh");
    let (status, view) = send(&app, "DELETE", &uri, json!(null), None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert!(view["refresh"].is_null(), "{view}");
    assert!(view["refreshError"].is_null(), "{view}");
    // Stopping one with nothing to stop answers the artifact as it is.
    let (status, view) = send(&app, "DELETE", &uri, json!(null), None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    state.refresh_tick().await;
    assert_eq!(state.artifacts.read().await.refresh_count(), 0);
}

#[tokio::test]
async fn a_run_in_flight_when_stopped_delivers_nothing() {
    let (state, app, id) = artifact_with(refresh("sleep 1; echo 5"), None).await;
    watch(&state, &[&id]).await;
    state.refresh_tick().await;
    assert_eq!(view_of(&state, &id).await["refreshing"], true);
    let uri = format!("/api/artifacts/{id}/refresh");
    let (status, view) = send(&app, "DELETE", &uri, json!(null), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(view["refreshing"].is_null(), "{view}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let view = view_of(&state, &id).await;
    assert!(view["data"].is_null(), "{view}");
    assert!(view["refreshError"].is_null(), "{view}");

    // A failing run stopped mid-flight leaves no error behind either.
    let (state, app, id) = artifact_with(refresh("sleep 1; exit 1"), None).await;
    watch(&state, &[&id]).await;
    state.refresh_tick().await;
    let uri = format!("/api/artifacts/{id}/refresh");
    send(&app, "DELETE", &uri, json!(null), None).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(view_of(&state, &id).await["refreshError"].is_null());
}

#[tokio::test]
async fn a_first_watch_runs_at_once_only_for_a_value_older_than_the_interval() {
    let dir = temp_dir();
    let (state, _app, id) = artifact_with(counting(&dir, false), None).await;
    // A value pushed just now is fresh: watching it waits for the interval.
    state.artifacts.write().await.set_data(&id, Some(json!(0)));
    watch(&state, &[&id]).await;
    tick_at(&state, Instant::now()).await;
    assert_eq!(runs(&dir), 0);
    // Once that value is five seconds old, the watched artifact runs.
    tick_at(&state, Instant::now() + Duration::from_secs(5)).await;
    assert_eq!(runs(&dir), 1);
}

#[tokio::test]
async fn the_watch_route_marks_an_artifact_watched() {
    let (state, app, id) = artifact_with(json!({}), None).await;
    let body = json!({"window": "w9", "visible": true, "artifacts": [id]});
    let (status, _) = send(&app, "PUT", "/api/artifact-watch", body, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(view_of(&state, &id).await["watched"], true);
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
async fn a_widget_that_freezes_webkit_is_refused_and_dropped_on_load() {
    let dir = temp_dir();
    let app = build_router(AppState::open(&dir).await);
    let freezing = format!("{}<table><tr><td><svg></table>", "<div>".repeat(505));
    let body = json!({"widget_html": freezing});
    let (status, _) = send(&app, "POST", "/api/artifacts", body, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let body = json!({"widget_html": "<b>one</b>"});
    let (_, created) = send(&app, "POST", "/api/artifacts", body, None).await;
    let id = created["id"].as_str().unwrap();
    let source = temp_dir().join("index.html");
    std::fs::write(&source, "<p>x</p>").unwrap();
    let uri = format!("/api/artifacts/{id}/put");
    let body = json!({"source": source, "widget_html": freezing});
    let (status, _) = send(&app, "POST", &uri, body, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let saved = std::fs::read_to_string(dir.join("artifacts.json")).unwrap();
    assert!(saved.contains("<b>one</b>"), "{saved}");

    // A widget stored before canvasd refused one.
    std::fs::write(
        dir.join("artifacts.json"),
        saved.replace("<b>one</b>", &freezing),
    )
    .unwrap();
    let state = AppState::open(&dir).await;
    let artifacts = state.artifacts.read().await;
    assert_eq!(artifacts.records[id].widget_html, None);
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
    assert_eq!(
        err,
        canvasd::refresh::Outcome::Failed("output is over 256 KB".to_string())
    );
}
