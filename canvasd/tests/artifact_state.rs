//! A page's saved state through canvasd's routes: round trip, key rule, caps,
//! confinement to `canvas-data/`, `put` leaving it alone, and a write firing
//! no artifact event.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use canvasd::build_router;
use canvasd::state::{AppState, CanvasEvent};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canvasd-state-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: &str) -> (u16, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn make(app: &axum::Router, body: Value) -> (String, PathBuf) {
    let (status, text) = call(app, "POST", "/api/artifacts", &body.to_string()).await;
    assert_eq!(status, 200, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    (
        v["id"].as_str().unwrap().to_string(),
        PathBuf::from(v["path"].as_str().unwrap()),
    )
}

async fn daemon() -> (AppState, axum::Router) {
    let state = AppState::open(&temp_dir()).await;
    let app = build_router(state.clone());
    (state, app)
}

#[tokio::test]
async fn a_value_round_trips_for_an_owned_and_a_linked_artifact() {
    let (_state, app) = daemon().await;
    let linked_dir = temp_dir();
    std::fs::write(linked_dir.join("index.html"), "<p>x</p>").unwrap();
    let (owned, owned_path) = make(&app, json!({})).await;
    let (linked, _) = make(&app, json!({"link": linked_dir})).await;

    for (id, folder) in [(&owned, &owned_path), (&linked, &linked_dir)] {
        let uri = format!("/api/artifacts/{id}/state/score");
        assert_eq!(call(&app, "PUT", &uri, r#"{"n":[1,2]}"#).await.0, 204);
        assert_eq!(
            call(
                &app,
                "PUT",
                &format!("/api/artifacts/{id}/state/name"),
                "\"ada\""
            )
            .await
            .0,
            204
        );
        let (status, body) = call(&app, "GET", &uri, "").await;
        assert_eq!((status, body.as_str()), (200, r#"{"n":[1,2]}"#));
        let (_, all) = call(&app, "GET", &format!("/api/artifacts/{id}/state"), "").await;
        assert_eq!(
            serde_json::from_str::<Value>(&all).unwrap(),
            json!({"name": "ada", "score": {"n": [1, 2]}})
        );
        assert!(folder.join("canvas-data/score.json").is_file());
        assert_eq!(
            call(&app, "GET", &format!("/api/artifacts/{id}/state/none"), "")
                .await
                .0,
            404
        );
    }
    // A value in one artifact is not in the other.
    let other = format!("/api/artifacts/{owned}/state/score");
    assert_eq!(call(&app, "PUT", &other, "7").await.0, 204);
    let (_, text) = call(
        &app,
        "GET",
        &format!("/api/artifacts/{linked}/state/score"),
        "",
    )
    .await;
    assert_eq!(text, r#"{"n":[1,2]}"#);
}

#[tokio::test]
async fn a_key_that_is_not_a_name_is_refused_and_nothing_is_written() {
    let (_state, app) = daemon().await;
    let (id, folder) = make(&app, json!({})).await;
    for key in ["Up", "a.b", "%2e%2e", "a%2fb", "-x", "x%00"] {
        let (status, _) = call(
            &app,
            "PUT",
            &format!("/api/artifacts/{id}/state/{key}"),
            "1",
        )
        .await;
        assert!(status == 400 || status == 404, "{key}: {status}");
    }
    let long = "a".repeat(65);
    let (status, text) = call(
        &app,
        "PUT",
        &format!("/api/artifacts/{id}/state/{long}"),
        "1",
    )
    .await;
    assert_eq!(status, 400, "{text}");
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/api/artifacts/{id}/state/ok"),
            "{nope"
        )
        .await
        .0,
        400
    );
    assert!(!folder.join("canvas-data").exists());
    assert_eq!(
        call(&app, "PUT", "/api/artifacts/art-0000000000/state/a", "1")
            .await
            .0,
        404
    );
}

#[tokio::test]
async fn one_key_and_one_artifact_have_a_size_cap() {
    let (_state, app) = daemon().await;
    let (id, folder) = make(&app, json!({})).await;
    let too_big = format!("\"{}\"", "x".repeat(256 * 1024));
    let (status, text) = call(
        &app,
        "PUT",
        &format!("/api/artifacts/{id}/state/big"),
        &too_big,
    )
    .await;
    assert_eq!(status, 413, "{text}");
    assert!(!folder.join("canvas-data/big.json").exists());

    let fits = format!("\"{}\"", "x".repeat(256 * 1024 - 2));
    for i in 0..20 {
        let uri = format!("/api/artifacts/{id}/state/k{i}");
        assert_eq!(call(&app, "PUT", &uri, &fits).await.0, 204, "k{i}");
    }
    let (status, text) = call(
        &app,
        "PUT",
        &format!("/api/artifacts/{id}/state/k20"),
        &fits,
    )
    .await;
    assert_eq!(status, 413, "{text}");
    assert!(text.contains("at most"), "{text}");
    assert!(!folder.join("canvas-data/k20.json").exists());
    // Overwriting a key replaces its size rather than adding to it.
    assert_eq!(
        call(&app, "PUT", &format!("/api/artifacts/{id}/state/k0"), &fits)
            .await
            .0,
        204
    );
}

#[tokio::test]
async fn a_linked_folder_is_written_only_inside_canvas_data() {
    let (_state, app) = daemon().await;
    let repo = temp_dir();
    std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
    std::fs::write(repo.join("index.html"), "<p>x</p>").unwrap();
    let (id, _) = make(&app, json!({"link": repo})).await;
    for key in ["a", "b-2", "c_3"] {
        assert_eq!(
            call(
                &app,
                "PUT",
                &format!("/api/artifacts/{id}/state/{key}"),
                "1"
            )
            .await
            .0,
            204
        );
    }
    let mut top: Vec<String> = std::fs::read_dir(&repo)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, [".git", "canvas-data", "index.html"]);
    assert_eq!(
        std::fs::read_dir(repo.join(".git/hooks")).unwrap().count(),
        0
    );
    let mut names: Vec<String> = std::fs::read_dir(repo.join("canvas-data"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.json", "b-2.json", "c_3.json"]);

    // A linked HTML file keeps its state beside it.
    let page_dir = temp_dir();
    let page = page_dir.join("quiz.html");
    std::fs::write(&page, "<p>q</p>").unwrap();
    let (file_id, _) = make(&app, json!({"link": page})).await;
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/api/artifacts/{file_id}/state/a"),
            "2"
        )
        .await
        .0,
        204
    );
    assert!(page_dir.join("canvas-data/a.json").is_file());
}

#[tokio::test]
async fn a_symlinked_canvas_data_or_key_file_is_refused() {
    let (_state, app) = daemon().await;
    let repo = temp_dir();
    let outside = temp_dir();
    std::fs::write(repo.join("index.html"), "<p>x</p>").unwrap();
    std::os::unix::fs::symlink(&outside, repo.join("canvas-data")).unwrap();
    let (id, _) = make(&app, json!({"link": repo})).await;
    let uri = format!("/api/artifacts/{id}/state/a");
    let (status, text) = call(&app, "PUT", &uri, "1").await;
    assert_eq!(status, 403, "{text}");
    assert!(text.contains("symlink"), "{text}");
    assert_eq!(
        call(&app, "DELETE", &format!("/api/artifacts/{id}/state"), "")
            .await
            .0,
        403
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);

    std::fs::remove_file(repo.join("canvas-data")).unwrap();
    std::fs::create_dir(repo.join("canvas-data")).unwrap();
    let victim = outside.join("victim");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&victim, repo.join("canvas-data/a.json")).unwrap();
    assert_eq!(call(&app, "PUT", &uri, "1").await.0, 403);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
}

#[tokio::test]
async fn put_leaves_state_alone_and_never_imports_it() {
    let (_state, app) = daemon().await;
    let (id, folder) = make(&app, json!({})).await;
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/api/artifacts/{id}/state/keep"),
            "\"mine\""
        )
        .await
        .0,
        204
    );

    let source = temp_dir();
    std::fs::write(source.join("index.html"), "<p>v2</p>").unwrap();
    std::fs::create_dir_all(source.join("canvas-data")).unwrap();
    std::fs::write(source.join("canvas-data/keep.json"), "\"theirs\"").unwrap();
    std::fs::write(source.join("canvas-data/extra.json"), "1").unwrap();
    let (status, text) = call(
        &app,
        "POST",
        &format!("/api/artifacts/{id}/put"),
        &json!({"source": source}).to_string(),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    assert!(folder.join("index.html").is_file());
    let (_, all) = call(&app, "GET", &format!("/api/artifacts/{id}/state"), "").await;
    assert_eq!(
        serde_json::from_str::<Value>(&all).unwrap(),
        json!({"keep": "mine"})
    );
}

#[tokio::test]
async fn clear_deletes_every_key_and_delete_removes_an_owned_artifacts_state() {
    let (_state, app) = daemon().await;
    let (id, folder) = make(&app, json!({})).await;
    for key in ["a", "b"] {
        call(
            &app,
            "PUT",
            &format!("/api/artifacts/{id}/state/{key}"),
            "1",
        )
        .await;
    }
    let (status, text) = call(&app, "DELETE", &format!("/api/artifacts/{id}/state"), "").await;
    assert_eq!((status, text.as_str()), (200, r#"{"cleared":2}"#));
    assert!(!folder.join("canvas-data").exists());
    let (_, all) = call(&app, "GET", &format!("/api/artifacts/{id}/state"), "").await;
    assert_eq!(all, "{}");
    call(&app, "PUT", &format!("/api/artifacts/{id}/state/a"), "1").await;
    call(&app, "DELETE", &format!("/api/artifacts/{id}"), "").await;
    assert!(!folder.exists());
}

fn only_events_for(rx: &mut tokio::sync::broadcast::Receiver<CanvasEvent>, id: &str) -> usize {
    let mut n = 0;
    while let Ok(event) = rx.try_recv() {
        if let CanvasEvent::ArtifactUpserted(view) = event {
            if view.artifact.id == id {
                n += 1;
            }
        }
    }
    n
}

#[tokio::test]
async fn a_state_write_fires_no_artifact_event_owned_or_linked() {
    let state = AppState::open(&temp_dir()).await;
    canvasd::watcher::spawn_artifact_watcher(state.clone());
    let app = build_router(state.clone());
    let linked_dir = temp_dir();
    std::fs::write(linked_dir.join("index.html"), "<p>x</p>").unwrap();
    let (owned, _) = make(&app, json!({})).await;
    let (linked, _) = make(&app, json!({"link": linked_dir})).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut rx = state.events.subscribe();

    for id in [&owned, &linked] {
        for i in 0..3 {
            let uri = format!("/api/artifacts/{id}/state/k{i}");
            assert_eq!(call(&app, "PUT", &uri, &i.to_string()).await.0, 204);
        }
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(only_events_for(&mut rx, &owned), 0, "owned reloaded");
    assert_eq!(only_events_for(&mut rx, &linked), 0, "linked reloaded");

    // The watcher still sees a real write in the same folder.
    std::fs::write(linked_dir.join("app.js"), "1").unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(only_events_for(&mut rx, &linked), 1);
    let _: &Path = &linked_dir;
}
