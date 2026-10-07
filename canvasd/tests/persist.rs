use std::path::{Path, PathBuf};

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
    v["sessions"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|s| s["id"].to_string());
    v
}

/// Drop the app, then start a new one on the same directory.
async fn restart(state: AppState, dir: &Path) -> axum::Router {
    state.flush_store();
    drop(state);
    build_router(AppState::open(dir).await)
}

fn line_count(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("stream.jsonl"))
        .unwrap()
        .lines()
        .count()
}

#[tokio::test]
async fn restart_rebuilds_the_same_state() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    send(
        &app,
        "POST",
        "/api/posts",
        Some(json!({"session_id": "s1", "cwd": "/a/one", "agent": "claude-code", "html": "<p>x</p>"})),
    )
    .await;
    send(
        &app,
        "POST",
        "/api/posts",
        Some(json!({"session_id": "s1", "cwd": "/a/one", "agent": "claude-code", "html": "<p>y</p>"})),
    )
    .await;
    send(
        &app,
        "POST",
        "/api/posts",
        Some(json!({"session_id": "s2", "cwd": "/a/two", "agent": "claude-code", "html": "<p>z</p>"})),
    )
    .await;
    send(&app, "POST", "/api/sessions/s2/end", None).await;
    let before = state_of(&app).await;
    assert_eq!(before["cards"].as_array().unwrap().len(), 3);

    let app = restart(state, &dir).await;
    assert_eq!(state_of(&app).await, before);
    // Compaction: 2 sessions + 3 cards, down from one line per change.
    assert_eq!(line_count(&dir), 5);
}

#[tokio::test]
async fn an_updated_card_stays_on_top_with_both_times_after_restart() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    let mut ids = Vec::new();
    for html in ["<p>a</p>", "<p>b</p>"] {
        let bytes = send(
            &app,
            "POST",
            "/api/posts",
            Some(
                json!({"session_id": "s1", "cwd": "/a/one", "agent": "claude-code", "html": html}),
            ),
        )
        .await;
        let card: Value = serde_json::from_slice(&bytes).unwrap();
        ids.push(card["id"].as_str().unwrap().to_string());
    }
    let (a, b) = (&ids[0], &ids[1]);
    send(
        &app,
        "PUT",
        &format!("/api/cards/{a}"),
        Some(json!({"html": "<p>a2</p>"})),
    )
    .await;
    let before = state_of(&app).await;

    let app = restart(state, &dir).await;
    let after = state_of(&app).await;
    assert_eq!(after, before);
    let cards = after["cards"].as_array().unwrap();
    assert_eq!(cards[0]["id"], a.as_str());
    assert_eq!(cards[1]["id"], b.as_str());
    assert!(cards[0]["updatedAt"].as_str().unwrap() > cards[0]["at"].as_str().unwrap());
    assert!(cards[1].get("updatedAt").is_none());
}

#[tokio::test]
async fn pushed_card_data_is_not_written_to_the_stream() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    let posted = send(
        &app,
        "POST",
        "/api/posts",
        Some(json!({"session_id": "s1", "cwd": "/a", "agent": "claude-code", "html": "<p>x</p>"})),
    )
    .await;
    let id = serde_json::from_slice::<Value>(&posted).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let lines_before = {
        state.flush_store();
        line_count(&dir)
    };
    send(
        &app,
        "PUT",
        &format!("/api/cards/{id}/data"),
        Some(json!({"n": 1})),
    )
    .await;
    state.flush_store();
    assert_eq!(line_count(&dir), lines_before);

    let app = restart(state, &dir).await;
    let bytes = send(&app, "GET", &format!("/api/cards/{id}/data"), None).await;
    assert!(bytes.is_empty(), "data survived a restart: {bytes:?}");
}

#[tokio::test]
async fn deletions_stay_deleted_after_restart() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    for s in ["keep", "gone"] {
        send(
            &app,
            "POST",
            "/api/posts",
            Some(json!({"session_id": s, "cwd": "/a", "agent": "claude-code", "html": "<p>x</p>"})),
        )
        .await;
    }
    send(
        &app,
        "POST",
        "/api/posts",
        Some(json!({"session_id": "third", "cwd": "/a", "agent": "claude-code", "html": "h"})),
    )
    .await;
    let all: StateResponse =
        serde_json::from_slice(&send(&app, "GET", "/api/state", None).await).unwrap();
    let third_card = all
        .cards
        .iter()
        .find(|c| c.session_id == "third")
        .unwrap()
        .id
        .clone();
    send(&app, "DELETE", &format!("/api/cards/{third_card}"), None).await;
    send(&app, "DELETE", "/api/sessions/gone", None).await;
    let before = state_of(&app).await;

    let app = restart(state, &dir).await;
    let after = state_of(&app).await;
    // "third" survives until restart, then goes: it has no card left.
    assert_eq!(after["cards"], before["cards"]);
    let ids: Vec<&str> = after["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["keep"]);
    assert_eq!(after["cards"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn cleared_cards_stay_cleared_after_restart() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    for s in ["cleared", "other"] {
        send(
            &app,
            "POST",
            "/api/posts",
            Some(json!({"session_id": s, "cwd": "/a", "agent": "claude-code", "html": "<p>x</p>"})),
        )
        .await;
        send(
            &app,
            "POST",
            "/api/posts",
            Some(json!({"session_id": s, "cwd": "/a", "agent": "claude-code", "html": "<p>y</p>"})),
        )
        .await;
    }
    send(&app, "DELETE", "/api/sessions/cleared/cards", None).await;
    let before = state_of(&app).await;
    assert_eq!(before["cards"].as_array().unwrap().len(), 2);

    let app = restart(state, &dir).await;
    let after = state_of(&app).await;
    // "cleared" survives until restart, then goes: it has no card left.
    assert_eq!(after["cards"], before["cards"]);
    let ids: Vec<&str> = after["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["other"]);
}

#[tokio::test]
async fn entries_older_than_24_hours_are_dropped() {
    let dir = temp_dir();
    let old = (chrono::Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
    let new = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let session = |id: &str, at: &str| json!({"op": "session_upserted", "data": {"id": id, "cwd": "/a", "name": "a", "startedAt": at}});
    let card = |id: &str, sid: &str, at: &str| json!({"op": "card_upserted", "data": {"id": id, "sessionId": sid, "at": at, "html": "<p>x</p>", "images": [], "targets": []}});
    let lines = [
        session("old", &old),
        card("c-old", "old", &old),
        session("live", &old), // started long ago, still has a recent card
        card("c-live", "live", &new),
        session("empty", &new), // recent, but never posted
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("stream.jsonl"), body).unwrap();

    let app = build_router(AppState::open(&dir).await);
    let state = state_of(&app).await;
    let sessions: Vec<&str> = state["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(sessions, ["live"]);
    // Written before sessions carried an agent; every such session was Claude Code.
    assert_eq!(state["sessions"][0]["agent"], "claude-code");
    assert_eq!(state["cards"][0]["id"], "c-live");
    assert_eq!(line_count(&dir), 2);
}

#[tokio::test]
async fn a_stored_card_that_would_freeze_the_viewer_is_dropped_on_reload() {
    let dir = temp_dir();
    let at = chrono::Utc::now().to_rfc3339();
    let session = |id: &str| json!({"op": "session_upserted", "data": {"id": id, "cwd": "/a", "name": "a", "startedAt": at}});
    let card = |id: &str, sid: &str, html: &str| json!({"op": "card_upserted", "data": {"id": id, "sessionId": sid, "at": at, "html": html, "images": [], "targets": []}});
    let freezing = format!("{}<table><tr><td><svg></table>", "<div>".repeat(506));
    let lines = [
        session("frozen"),
        card("c-frozen", "frozen", &freezing),
        session("live"),
        card("c-live", "live", "<p>x</p>"),
        card("c-live-frozen", "live", &freezing),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("stream.jsonl"), body).unwrap();

    let app = build_router(AppState::open(&dir).await);
    let state = state_of(&app).await;
    let cards: Vec<&str> = state["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(cards, ["c-live"]);
    // A session left with no card goes too.
    assert_eq!(state["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(line_count(&dir), 2);
}

#[tokio::test]
async fn torn_last_line_is_skipped() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    send(
        &app,
        "POST",
        "/api/posts",
        Some(json!({"session_id": "s1", "cwd": "/a", "agent": "claude-code", "html": "<p>x</p>"})),
    )
    .await;
    let before = state_of(&app).await;
    state.flush_store();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("stream.jsonl"))
        .unwrap();
    std::io::Write::write_all(
        &mut file,
        br#"{"op":"card_upserted","data":{"id":"torn","sess"#,
    )
    .unwrap();
    drop(state);

    let app = build_router(AppState::open(&dir).await);
    assert_eq!(state_of(&app).await, before);
}

/// One request carrying the provenance headers `canvas artifact` sends.
async fn send_as(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-canvas-session", "s-prov")
        .header("x-canvas-agent", "codex")
        .header("x-canvas-pid", "4242");
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn artifact_log_names_the_session_and_survives_delete_and_restart() {
    let dir = temp_dir();
    let state = AppState::open(&dir).await;
    let app = build_router(state.clone());
    let created = send_as(&app, "POST", "/api/artifacts", Some(json!({}))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let page = dir.join("page.html");
    std::fs::write(&page, "<h1>x</h1>").unwrap();
    let source = page.to_str().unwrap();
    send_as(
        &app,
        "POST",
        &format!("/api/artifacts/{id}/put"),
        Some(json!({ "source": source })),
    )
    .await;
    send_as(&app, "DELETE", &format!("/api/artifacts/{id}"), None).await;

    let app = restart(state, &dir).await;
    let bytes = send(&app, "GET", &format!("/api/artifacts/{id}/log"), None).await;
    let log: Value = serde_json::from_slice(&bytes).unwrap();
    let lines = log.as_array().unwrap();
    let actions: Vec<&str> = lines
        .iter()
        .map(|l| l["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["create", "put", "delete"]);
    for line in lines {
        assert_eq!(line["sessionId"], "s-prov");
        assert_eq!(line["agent"], "codex");
        assert_eq!(line["pid"], 4242);
    }

    let unknown = send(&app, "GET", "/api/artifacts/art-0000000000/log", None).await;
    assert_eq!(
        String::from_utf8_lossy(&unknown),
        "no artifact with that id"
    );
}

#[tokio::test]
async fn script_errors_list_per_artifact_and_keep_the_newest_50() {
    let dir = temp_dir();
    let app = build_router(AppState::open(&dir).await);
    let a = send_as(&app, "POST", "/api/artifacts", Some(json!({}))).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = send_as(&app, "POST", "/api/artifacts", Some(json!({}))).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    for n in 1..=55 {
        send(
            &app,
            "POST",
            &format!("/api/artifacts/{a}/errors"),
            Some(json!({ "kind": "error", "message": format!("boom {n}"), "line": n })),
        )
        .await;
    }
    send(
        &app,
        "POST",
        &format!("/api/artifacts/{b}/errors"),
        Some(json!({ "kind": "rejection", "message": "Error: nope" })),
    )
    .await;

    let shown: Value =
        serde_json::from_slice(&send(&app, "GET", &format!("/api/artifacts/{a}"), None).await)
            .unwrap();
    let errors = shown["scriptErrors"].as_array().unwrap();
    assert_eq!(errors.len(), 50);
    assert_eq!(errors[0]["message"], "boom 6");
    assert_eq!(errors[49]["message"], "boom 55");
    assert_eq!(errors[49]["line"], 55);
    assert_eq!(errors[49]["kind"], "error");
    assert!(errors[49]["at"].is_string());

    let other: Value =
        serde_json::from_slice(&send(&app, "GET", &format!("/api/artifacts/{b}"), None).await)
            .unwrap();
    assert_eq!(
        other["scriptErrors"],
        json!([{ "at": other["scriptErrors"][0]["at"], "kind": "rejection", "message": "Error: nope" }])
    );

    let list: Value =
        serde_json::from_slice(&send(&app, "GET", "/api/artifacts", None).await).unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a.get("scriptErrors").is_none()));

    let unknown = send(
        &app,
        "POST",
        "/api/artifacts/art-0000000000/errors",
        Some(json!({ "kind": "error", "message": "x" })),
    )
    .await;
    assert_eq!(
        String::from_utf8_lossy(&unknown),
        "no artifact with that id"
    );
}

#[tokio::test]
async fn artifact_snapshot_asks_the_viewer_and_answers_its_png() {
    use futures::StreamExt;

    let dir = temp_dir();
    let app = build_router(AppState::open(&dir).await);
    let id = send_as(&app, "POST", "/api/artifacts", Some(json!({}))).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = |id: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/artifacts/{id}/snapshot"))
            .body(Body::empty())
            .unwrap()
    };

    let unknown = app
        .clone()
        .oneshot(snapshot("art-0000000000"))
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    let nobody = app.clone().oneshot(snapshot(&id)).await.unwrap();
    assert_eq!(nobody.status(), 409);
    let body = nobody.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        &body[..],
        b"no Canvas viewer is open to capture the artifact"
    );

    let events = app
        .clone()
        .oneshot(Request::get("/api/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let mut events = events.into_body().into_data_stream();
    let waiting = tokio::spawn(app.clone().oneshot(snapshot(&id)));
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
        .await
        .expect("no event within 2s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(text.contains("event: artifact-snapshot"), "{text}");
    let data: Value =
        serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
    assert_eq!(data["id"], id.as_str());
    let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
    let answer = Request::post(format!(
        "/api/snapshots/{}",
        data["request"].as_str().unwrap()
    ))
    .header("content-type", "image/png")
    .body(Body::from(png.clone()))
    .unwrap();
    assert_eq!(app.clone().oneshot(answer).await.unwrap().status(), 204);

    let response = waiting.await.unwrap().unwrap();
    assert_eq!(response.status(), 200);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.to_vec(), png);
}

#[tokio::test]
async fn artifact_html_is_served_with_the_error_relay_and_other_files_untouched() {
    let dir = temp_dir();
    let app = build_router(AppState::open(&dir).await);
    let id = send_as(&app, "POST", "/api/artifacts", Some(json!({}))).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("index.html"),
        "<!doctype html><html><head><script src=app.js></script></head></html>",
    )
    .unwrap();
    std::fs::write(src.join("app.js"), "throw new Error('x')").unwrap();
    send_as(
        &app,
        "POST",
        &format!("/api/artifacts/{id}/put"),
        Some(json!({ "source": src.to_str().unwrap() })),
    )
    .await;

    let page =
        String::from_utf8(send(&app, "GET", &format!("/artifacts/{id}/"), None).await).unwrap();
    let relay_at = page.find("canvas-artifact-error").unwrap();
    assert!(page.starts_with("<!doctype html><html><head><script>"));
    assert!(relay_at < page.find("app.js").unwrap());
    let script = send(&app, "GET", &format!("/artifacts/{id}/app.js"), None).await;
    assert_eq!(String::from_utf8_lossy(&script), "throw new Error('x')");
}

#[tokio::test]
async fn a_linked_page_that_would_freeze_the_viewer_is_refused_and_left_as_is() {
    let dir = temp_dir();
    let app = build_router(AppState::open(&dir).await);
    let page = |k: usize| {
        format!(
            "<!doctype html><html><head><title>t</title></head><body>{}\
             <table><tr><td><svg></table></body></html>",
            "<div>".repeat(k)
        )
    };
    let file = dir.join("deep.html");
    std::fs::write(&file, page(506)).unwrap();
    let id = send_as(
        &app,
        "POST",
        "/api/artifacts",
        Some(json!({ "link": file })),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let get = |uri: String| {
        let app = app.clone();
        async move {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let body = res.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8(body.to_vec()).unwrap())
        }
    };
    let (status, body) = get(format!("/artifacts/{id}/")).await;
    assert_eq!(status, 422);
    let at = page(506).find("</table>").unwrap();
    assert_eq!(
        body,
        format!(
            "Canvas won't show this page: its </table> at byte {at} closes a table cell \
             nested past WebKit's 512-element limit, which freezes Canvas.app; nest the \
             table less deeply.\n"
        )
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), page(506));

    // One `<div>` fewer fits the page's frame, which has no wrapper.
    std::fs::write(&file, page(505)).unwrap();
    let (status, body) = get(format!("/artifacts/{id}/")).await;
    assert_eq!(status, 200);
    assert!(body.contains("canvas-artifact-error"));
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn an_artifact_page_link_opens_through_open_and_is_listed() {
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir();
    let log = dir.join("opened.txt");
    let stub = dir.join("open-stub.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho \"$1\" >> '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("CANVAS_OPEN_BIN", &stub);

    let app = build_router(AppState::open(&dir).await);
    let id = send_as(&app, "POST", "/api/artifacts", Some(json!({}))).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let status = |uri: String, url: &str| {
        let app = app.clone();
        let body = json!({ "url": url }).to_string();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
            .as_u16()
        }
    };
    let route = format!("/api/artifacts/{id}/open");
    assert_eq!(
        status(route.clone(), "https://www.rainforestpay.com/").await,
        204
    );
    assert_eq!(status(route.clone(), "file:///etc/passwd").await, 400);
    assert_eq!(status(route.clone(), "/etc/passwd").await, 400);
    assert_eq!(status(route.clone(), "javascript:alert(1)").await, 400);
    assert_eq!(status(route.clone(), "https://a.test/\nb").await, 400);
    assert_eq!(
        status(
            route.clone(),
            &format!("https://a.test/{}", "x".repeat(2048))
        )
        .await,
        400
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        status(
            "/api/artifacts/art-0000000000/open".into(),
            "https://a.test/"
        )
        .await,
        404
    );

    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "https://www.rainforestpay.com/\n"
    );
    let shown: Value =
        serde_json::from_slice(&send(&app, "GET", &format!("/api/artifacts/{id}"), None).await)
            .unwrap();
    let opened = shown["openedLinks"].as_array().unwrap();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["url"], "https://www.rainforestpay.com/");
}
