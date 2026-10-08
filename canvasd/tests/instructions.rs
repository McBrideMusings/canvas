use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use canvas_core::instructions::{self, Kind, Overrides};
use canvasd::build_router;
use canvasd::state::AppState;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct Dirs {
    base: PathBuf,
    data: PathBuf,
    repo: PathBuf,
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn dirs() -> Dirs {
    let base = std::env::temp_dir().join(format!("canvasd-instr-{}", uuid::Uuid::new_v4()));
    let data = base.join("data");
    let repo = base.join("repo");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    Dirs { base, data, repo }
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn json_of(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (status, bytes) = send(app, method, uri, body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_from(app: &axum::Router, session: &str, cwd: &Path) {
    let body =
        json!({"session_id": session, "cwd": cwd, "agent": "claude-code", "html": "<p>x</p>"});
    json_of(app, "POST", "/api/posts", Some(body)).await;
}

#[tokio::test]
async fn a_post_records_its_git_root_and_it_outlives_the_stream_and_a_restart() {
    let d = dirs();
    let state = AppState::open(&d.data).await;
    let app = build_router(state.clone());
    post_from(&app, "s1", &d.repo.join("src")).await;
    post_from(&app, "s2", &d.base).await;

    let projects = json_of(&app, "GET", "/api/projects", None).await;
    assert_eq!(projects.as_array().unwrap().len(), 1, "{projects}");
    assert_eq!(projects[0]["root"], json!(d.repo));
    assert_eq!(projects[0]["name"], "repo");
    assert_eq!(projects[0]["instructions"], false);
    assert!(projects[0]["lastSeen"].is_string());

    // The posts age out of the stream; the project stays.
    state.flush_store();
    drop(app);
    drop(state);
    std::fs::remove_file(d.data.join("stream.jsonl")).unwrap();
    let app = build_router(AppState::open(&d.data).await);
    let projects = json_of(&app, "GET", "/api/projects", None).await;
    assert_eq!(projects[0]["root"], json!(d.repo));
    assert_eq!(
        instructions::seen_projects(&d.data).unwrap()[0].root,
        d.repo
    );
}

#[tokio::test]
async fn an_unreadable_projects_file_is_never_overwritten_until_it_reads_again() {
    let d = dirs();
    let file = d.data.join(instructions::PROJECTS_FILE);
    let earlier = d.base.join("earlier");
    std::fs::create_dir_all(earlier.join(".git")).unwrap();
    let stored = json!([{"root": earlier, "name": "earlier", "lastSeen": "2026-10-01T00:00:00Z"}]);
    let corrupt = format!("{stored}\n<<<<<<< conflict");
    std::fs::write(&file, &corrupt).unwrap();
    assert!(instructions::seen_projects(&d.data)
        .unwrap_err()
        .contains("projects.json"));

    let app = build_router(AppState::open(&d.data).await);
    post_from(&app, "s1", &d.repo).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), corrupt);
    let projects = json_of(&app, "GET", "/api/projects", None).await;
    assert_eq!(projects[0]["root"], json!(d.repo));

    // Repaired by hand: the next record keeps both.
    std::fs::write(&file, stored.to_string()).unwrap();
    post_from(&app, "s2", &d.repo).await;
    let roots: Vec<_> = instructions::seen_projects(&d.data)
        .unwrap()
        .into_iter()
        .map(|p| p.root)
        .collect();
    assert_eq!(roots, vec![d.repo.clone(), earlier]);
}

#[tokio::test]
async fn a_project_write_creates_the_folder_and_the_file() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    post_from(&app, "s1", &d.repo).await;

    let body = json!({"root": d.repo, "text": "theirs\n"});
    let written = json_of(
        &app,
        "PUT",
        "/api/instructions/instructions/project",
        Some(body),
    )
    .await;
    assert_eq!(written["bytes"], 7);
    let file = d.repo.join(".canvas/instructions.md");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "theirs\n");
    let projects = json_of(&app, "GET", "/api/projects", None).await;
    assert_eq!(
        (
            projects[0]["instructions"].clone(),
            projects[0]["reminders"].clone()
        ),
        (json!(true), json!(false))
    );

    let body = json!({"root": d.repo, "text": "  \n"});
    json_of(
        &app,
        "PUT",
        "/api/instructions/instructions/project",
        Some(body),
    )
    .await;
    assert!(!file.exists(), "blank text deletes the file");
}

#[tokio::test]
async fn a_project_write_for_a_root_never_recorded_is_refused() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    let body = json!({"root": d.repo, "text": "theirs\n"});
    let (status, bytes) = send(
        &app,
        "PUT",
        "/api/instructions/instructions/project",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(String::from_utf8_lossy(&bytes).contains("not a project"));
    assert!(!d.repo.join(".canvas").exists());
}

#[tokio::test]
async fn a_symlinked_canvas_folder_or_file_is_refused() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    post_from(&app, "s1", &d.repo).await;
    let elsewhere = d.base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, d.repo.join(".canvas")).unwrap();

    let body = json!({"root": d.repo, "text": "theirs\n"});
    let (status, _) = send(
        &app,
        "PUT",
        "/api/instructions/instructions/project",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!elsewhere.join("instructions.md").exists());

    std::fs::remove_file(d.repo.join(".canvas")).unwrap();
    std::fs::create_dir(d.repo.join(".canvas")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.join("x.md"),
        d.repo.join(".canvas/instructions.md"),
    )
    .unwrap();
    let (status, _) = send(
        &app,
        "PUT",
        "/api/instructions/instructions/project",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!elsewhere.join("x.md").exists());
}

#[tokio::test]
async fn a_reminders_text_that_does_not_parse_is_refused_with_its_line() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    let body = json!({"text": "no image\nfile\nlinks many\n"});
    let (status, bytes) = send(
        &app,
        "PUT",
        "/api/instructions/reminders/person",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        String::from_utf8_lossy(&bytes).contains("line 3"),
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(!d.data.join("reminders.txt").exists());
}

#[tokio::test]
async fn compose_flags_an_unsaved_reminders_override_that_does_not_parse_with_the_put_wording() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    post_from(&app, "s1", &d.repo).await;
    let bad = "no image\nfile\nlinks many\n";
    let (_, put) = send(
        &app,
        "PUT",
        "/api/instructions/reminders/person",
        Some(json!({"text": bad})),
    )
    .await;
    let route = json_of(
        &app,
        "POST",
        "/api/instructions/reminders/compose",
        Some(json!({"root": d.repo, "person": bad, "project": "image\n"})),
    )
    .await;
    assert!(route["text"].as_str().unwrap().contains("links many"));
    let layers = route["layers"].as_array().unwrap();
    let person = layers.iter().find(|l| l["source"] == "person").unwrap();
    assert_eq!(person["error"], json!(String::from_utf8_lossy(&put)));
    assert!(person["error"].as_str().unwrap().starts_with("line 3:"));
    for layer in layers.iter().filter(|l| l["source"] != "person") {
        assert!(layer.get("error").is_none(), "{layer}");
    }
}

#[tokio::test]
async fn the_compose_route_matches_the_hook_and_takes_unsaved_overrides() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    post_from(&app, "s1", &d.repo).await;
    json_of(
        &app,
        "PUT",
        "/api/instructions/instructions/person",
        Some(json!({"text": "mine\n"})),
    )
    .await;
    json_of(
        &app,
        "PUT",
        "/api/instructions/instructions/project",
        Some(json!({"root": d.repo, "text": "theirs\n"})),
    )
    .await;

    // What `canvas hook session-start` prints for a session in the repo.
    let hook = instructions::compose(
        Kind::Instructions,
        Some(&d.data),
        Some(&d.repo.join("src")),
        &Overrides::default(),
    );
    let route = json_of(
        &app,
        "POST",
        "/api/instructions/instructions/compose",
        Some(json!({"root": d.repo})),
    )
    .await;
    assert_eq!(route["text"], json!(hook.text));
    assert!(hook.text.ends_with("\n\nmine\n\ntheirs\n"));
    let layers = route["layers"].as_array().unwrap();
    assert_eq!(layers[1]["source"], "person");
    assert_eq!(layers[2]["startLine"], json!(hook.layers[2].start_line));
    assert_eq!(layers[2]["lines"], 1);

    let route = json_of(
        &app,
        "POST",
        "/api/instructions/instructions/compose",
        Some(json!({"root": d.repo, "person": "unsaved"})),
    )
    .await;
    assert!(route["text"]
        .as_str()
        .unwrap()
        .ends_with("\n\nunsaved\n\ntheirs\n"));
    assert_eq!(
        std::fs::read_to_string(d.data.join("instructions.md")).unwrap(),
        "mine\n"
    );
}

#[tokio::test]
async fn include_off_drops_the_built_in_from_the_read() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    let flags = json_of(
        &app,
        "PUT",
        "/api/instructions/reminders/include",
        Some(json!({"include": false})),
    )
    .await;
    assert_eq!(flags, json!({"instructions": true, "reminders": false}));

    let read = json_of(&app, "GET", "/api/instructions/reminders", None).await;
    assert_eq!(read["include"], false);
    assert_eq!(read["layers"], json!([]));
    let read = json_of(&app, "GET", "/api/instructions/instructions", None).await;
    assert_eq!(read["include"], true);
    assert_eq!(read["layers"][0]["source"], "built-in");

    let (status, _) = send(&app, "GET", "/api/instructions/guidance", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_project_reminders_text_that_does_not_parse_is_refused_and_blank_person_text_deletes() {
    let d = dirs();
    let app = build_router(AppState::open(&d.data).await);
    post_from(&app, "s1", &d.repo).await;
    let body = json!({"root": d.repo, "text": "links many\n"});
    let (status, bytes) = send(
        &app,
        "PUT",
        "/api/instructions/reminders/project",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&bytes).starts_with("line 1:"));
    assert!(!d.repo.join(".canvas").exists());

    let put = |text: &str| json!({"text": text});
    json_of(
        &app,
        "PUT",
        "/api/instructions/reminders/person",
        Some(put("no image\n")),
    )
    .await;
    assert!(d.data.join("reminders.txt").exists());
    let written = json_of(
        &app,
        "PUT",
        "/api/instructions/reminders/person",
        Some(put("")),
    )
    .await;
    assert_eq!(written["bytes"], 0);
    assert!(!d.data.join("reminders.txt").exists());
}
