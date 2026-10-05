use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use canvasd::build_router;
use canvasd::state::{AppState, CanvasEvent};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canvasd-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn send(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// A daemon with the watcher running and one fresh artifact, given time for
/// the OS watcher to start reporting.
async fn watched_artifact() -> (AppState, axum::Router, String, PathBuf) {
    let state = AppState::open(&temp_dir()).await;
    canvasd::watcher::spawn_artifact_watcher(state.clone());
    let app = build_router(state.clone());
    let created = send(&app, "POST", "/api/artifacts", Some(json!({}))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let folder = PathBuf::from(created["path"].as_str().unwrap());
    tokio::time::sleep(Duration::from_millis(500)).await;
    (state, app, id, folder)
}

/// The `artifact-upserted` events for `id` published within `window`.
async fn upserts_for(
    rx: &mut tokio::sync::broadcast::Receiver<CanvasEvent>,
    id: &str,
    window: Duration,
) -> Vec<String> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if let CanvasEvent::ArtifactUpserted(view) = event {
            if view.artifact.id == id {
                seen.push(view.artifact.updated_at);
            }
        }
    }
    seen
}

#[tokio::test]
async fn a_burst_of_writes_reloads_the_artifact_once() {
    let (state, app, id, folder) = watched_artifact().await;
    let before = send(&app, "GET", &format!("/api/artifacts/{id}"), None).await;
    let mut rx = state.events.subscribe();

    // Spread across 90ms, so the OS reports them as separate events.
    std::fs::write(folder.join("index.html"), "<h1>one</h1>").unwrap();
    tokio::time::sleep(Duration::from_millis(45)).await;
    std::fs::write(folder.join("app.js"), "1").unwrap();
    tokio::time::sleep(Duration::from_millis(45)).await;
    std::fs::create_dir_all(folder.join("css")).unwrap();
    std::fs::write(folder.join("css/site.css"), "h1{}").unwrap();

    let seen = upserts_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(seen.len(), 1, "one reload per burst, got {seen:?}");
    let after = send(&app, "GET", &format!("/api/artifacts/{id}"), None).await;
    assert_eq!(after["updatedAt"], json!(seen[0]));
    assert_ne!(after["updatedAt"], before["updatedAt"]);
    assert_eq!(after["entry"], "index.html");
}

#[tokio::test]
async fn artifacts_on_disk_are_watched_after_a_restart() {
    let dir = temp_dir();
    let created = {
        let app = build_router(AppState::open(&dir).await);
        send(&app, "POST", "/api/artifacts", Some(json!({}))).await
    };
    let id = created["id"].as_str().unwrap();
    let folder = PathBuf::from(created["path"].as_str().unwrap());
    let state = AppState::open(&dir).await;
    canvasd::watcher::spawn_artifact_watcher(state.clone());
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut rx = state.events.subscribe();

    std::fs::write(folder.join("index.html"), "<h1>after restart</h1>").unwrap();

    let seen = upserts_for(&mut rx, id, Duration::from_secs(2)).await;
    assert_eq!(seen.len(), 1, "got {seen:?}");
}

#[tokio::test]
async fn a_held_artifact_reloads_only_after_its_hold_ends() {
    let (state, _app, id, folder) = watched_artifact().await;
    let mut rx = state.events.subscribe();
    let hold = state.watcher.hold(&id);

    std::fs::write(folder.join("index.html"), "<h1>slow put</h1>").unwrap();

    let held = upserts_for(&mut rx, &id, Duration::from_millis(2500)).await;
    assert!(
        held.is_empty(),
        "nothing while held (past MAX_WAIT), got {held:?}"
    );
    drop(hold);
    let seen = upserts_for(&mut rx, &id, Duration::from_secs(1)).await;
    assert_eq!(seen.len(), 1, "got {seen:?}");
}

#[tokio::test]
async fn a_put_reloads_the_artifact_once() {
    let (state, app, id, _folder) = watched_artifact().await;
    let source = temp_dir();
    std::fs::write(source.join("index.html"), "<h1>put</h1>").unwrap();
    std::fs::write(source.join("app.js"), "1").unwrap();
    let mut rx = state.events.subscribe();

    send(
        &app,
        "POST",
        &format!("/api/artifacts/{id}/put"),
        Some(json!({ "source": source })),
    )
    .await;

    let seen = upserts_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(seen.len(), 1, "the put's own event only, got {seen:?}");
}

/// A daemon with the watcher running and one artifact linked to `link`.
async fn linked_artifact(link: &std::path::Path) -> (AppState, axum::Router, String) {
    let state = AppState::open(&temp_dir()).await;
    canvasd::watcher::spawn_artifact_watcher(state.clone());
    let app = build_router(state.clone());
    let created = send(
        &app,
        "POST",
        "/api/artifacts",
        Some(json!({ "link": link })),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(500)).await;
    (state, app, id)
}

/// The views published for `id` within `window`.
async fn views_for(
    rx: &mut tokio::sync::broadcast::Receiver<CanvasEvent>,
    id: &str,
    window: Duration,
) -> Vec<canvas_core::ArtifactView> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if let CanvasEvent::ArtifactUpserted(view) = event {
            if view.artifact.id == id {
                seen.push(view);
            }
        }
    }
    seen
}

#[tokio::test]
async fn saving_into_a_linked_folder_reloads_it() {
    let web = temp_dir();
    std::fs::write(web.join("index.html"), "<h1>one</h1>").unwrap();
    let (state, _app, id) = linked_artifact(&web).await;
    let mut rx = state.events.subscribe();

    std::fs::write(web.join("index.html"), "<h1>two</h1>").unwrap();

    let seen = upserts_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(seen.len(), 1, "got {seen:?}");
}

#[tokio::test]
async fn an_atomic_save_of_a_linked_file_reloads_it_and_its_siblings_do_not() {
    let dir = temp_dir();
    let page = dir.join("report.html");
    std::fs::write(&page, "<h1>one</h1>").unwrap();
    let (state, _app, id) = linked_artifact(&page).await;
    let mut rx = state.events.subscribe();

    std::fs::write(dir.join("notes.txt"), "beside it").unwrap();
    let quiet = upserts_for(&mut rx, &id, Duration::from_secs(1)).await;
    assert!(quiet.is_empty(), "a sibling write, got {quiet:?}");

    // How editors save: write a temporary file, rename it over the page.
    std::fs::write(dir.join(".report.html.tmp"), "<h1>two</h1>").unwrap();
    std::fs::rename(dir.join(".report.html.tmp"), &page).unwrap();
    let seen = upserts_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(seen.len(), 1, "got {seen:?}");

    // The rename replaced the inode the watch began on; a later save still lands.
    std::fs::write(&page, "<h1>three</h1>").unwrap();
    let again = upserts_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(again.len(), 1, "got {again:?}");
}

#[tokio::test]
async fn a_linked_folder_moving_away_publishes_source_missing_and_back_restores_it() {
    let parent = temp_dir();
    let web = parent.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<h1>here</h1>").unwrap();
    let (state, _app, id) = linked_artifact(&web).await;
    let mut rx = state.events.subscribe();

    std::fs::rename(&web, parent.join("web-moved")).unwrap();
    let gone = views_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(gone.len(), 1, "got {} views", gone.len());
    assert!(gone[0].source_missing);

    std::fs::rename(parent.join("web-moved"), &web).unwrap();
    let back = views_for(&mut rx, &id, Duration::from_secs(2)).await;
    assert_eq!(back.len(), 1, "got {} views", back.len());
    assert!(!back[0].source_missing);
}

#[tokio::test]
async fn a_write_inside_nested_links_reloads_both() {
    let outer = temp_dir();
    let inner = outer.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let (state, app, outer_id) = linked_artifact(&outer).await;
    let inner_id = send(
        &app,
        "POST",
        "/api/artifacts",
        Some(json!({ "link": inner })),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let same_id = send(
        &app,
        "POST",
        "/api/artifacts",
        Some(json!({ "link": inner })),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Deleting one of two links to the same folder leaves the other watched.
    send(&app, "DELETE", &format!("/api/artifacts/{same_id}"), None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut rx = state.events.subscribe();

    std::fs::write(inner.join("index.html"), "<h1>nested</h1>").unwrap();

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if let CanvasEvent::ArtifactUpserted(view) = event {
            seen.push(view.artifact.id);
        }
    }
    seen.sort();
    let mut want = vec![outer_id, inner_id];
    want.sort();
    assert_eq!(seen, want);
}
