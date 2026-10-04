//! `canvas artifact` against the real built binary as a throwaway daemon on
//! its own socket: the lifecycle, serving files, path confinement, and
//! surviving a restart.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Daemon {
    child: std::process::Child,
    data_dir: PathBuf,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "canvas-artifact-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn start_daemon(data_dir: &Path) -> Daemon {
    let socket = data_dir.join("canvasd.sock");
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_DATA_DIR", data_dir)
        .env("CANVAS_SOCKET", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        if Instant::now() > deadline {
            panic!("daemon on {} never came up", socket.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon {
        child,
        data_dir: data_dir.to_path_buf(),
        socket,
    }
}

fn run(daemon: &Daemon, args: &[&str]) -> std::process::Output {
    Command::new(canvas_bin())
        .args(args)
        .env("CANVAS_SOCKET", &daemon.socket)
        .env("CANVAS_DATA_DIR", &daemon.data_dir)
        .output()
        .expect("failed to run canvas")
}

fn json(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "canvas failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON value")
}

/// A raw GET with the path sent exactly as written, `..` included.
fn get(daemon: &Daemon, path: &str) -> canvas_core::unix_http::Response {
    canvas_core::unix_http::request(
        &daemon.socket,
        "GET",
        path,
        &[],
        &[],
        Some(Duration::from_secs(5)),
    )
    .expect("canvasd answered")
}

#[test]
fn new_put_show_list_delete_round_trip() {
    let daemon = start_daemon(&temp_dir("life"));
    let created = json(&run(
        &daemon,
        &["artifact", "new", "--title", "Phone prototype"],
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("art-"), "id {id}");
    let folder = PathBuf::from(created["path"].as_str().unwrap());
    assert_eq!(folder, daemon.data_dir.join("artifacts").join(&id));
    assert!(folder.is_dir());
    assert_eq!(created["title"], "Phone prototype");
    assert_eq!(created["kind"], "owned");

    let src = temp_dir("src");
    std::fs::write(
        src.join("page.html"),
        r#"<meta name="canvas-size" content="390x844"><p>hi</p>"#,
    )
    .unwrap();
    let put = json(&run(
        &daemon,
        &[
            "artifact",
            "put",
            &id,
            src.join("page.html").to_str().unwrap(),
        ],
    ));
    assert_eq!(put["entry"], "page.html");
    assert_eq!(
        put["size"],
        serde_json::json!({"width": 390, "height": 844})
    );
    assert!(put["updatedAt"].as_str().unwrap() >= created["updatedAt"].as_str().unwrap());

    let shown = json(&run(&daemon, &["artifact", "show", &id]));
    assert_eq!(shown["entry"], "page.html");
    let listed = json(&run(&daemon, &["artifact", "list"]));
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], id.as_str());

    let deleted = json(&run(&daemon, &["artifact", "delete", &id]));
    assert_eq!(deleted["deleted"], id.as_str());
    assert!(!folder.exists(), "delete removes the folder");
    let after = run(&daemon, &["artifact", "show", &id]);
    assert_eq!(after.status.code(), Some(1));
    assert!(after.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&after.stderr).trim(),
        "canvasd returned HTTP 404 (no artifact with that id)"
    );
    assert_eq!(
        json(&run(&daemon, &["artifact", "list"])),
        serde_json::json!([])
    );
}

#[test]
fn a_two_file_artifact_serves_both_files_with_its_csp() {
    let daemon = start_daemon(&temp_dir("serve"));
    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let src = temp_dir("app");
    std::fs::write(
        src.join("index.html"),
        r#"<script src="./app.js"></script>"#,
    )
    .unwrap();
    std::fs::write(src.join("app.js"), "document.title = 'ok';").unwrap();
    json(&run(
        &daemon,
        &["artifact", "put", &id, src.to_str().unwrap()],
    ));

    let page = get(&daemon, &format!("/artifacts/{id}/"));
    assert_eq!(page.status, 200);
    assert_eq!(page.body, br#"<script src="./app.js"></script>"#);
    assert_eq!(
        page.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    let csp = page
        .headers
        .iter()
        .find(|(k, _)| k == "content-security-policy")
        .map(|(_, v)| v.as_str())
        .expect("a CSP header");
    assert!(csp.contains("connect-src 'none'"), "{csp}");
    assert!(csp.contains("sandbox allow-scripts"), "{csp}");

    let script = get(&daemon, &format!("/artifacts/{id}/app.js"));
    assert_eq!(script.status, 200);
    assert_eq!(script.body, b"document.title = 'ok';");
}

#[test]
fn file_requests_cannot_leave_the_folder() {
    let daemon = start_daemon(&temp_dir("confine"));
    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let folder = daemon.data_dir.join("artifacts").join(&id);
    std::fs::write(daemon.data_dir.join("secret.txt"), "secret").unwrap();
    std::fs::write(folder.join("index.html"), "<p>in</p>").unwrap();
    std::os::unix::fs::symlink(daemon.data_dir.join("secret.txt"), folder.join("link.txt"))
        .unwrap();
    std::os::unix::fs::symlink(&daemon.data_dir, folder.join("up")).unwrap();

    for path in [
        format!("/artifacts/{id}/../../secret.txt"),
        format!("/artifacts/{id}/%2e%2e/%2e%2e/secret.txt"),
        format!("/artifacts/{id}/link.txt"),
        format!("/artifacts/{id}/up/secret.txt"),
        "/artifacts/art-nope/index.html".to_string(),
    ] {
        let response = get(&daemon, &path);
        assert_eq!(response.status, 404, "{path}");
        assert!(!response.body.windows(6).any(|w| w == b"secret"), "{path}");
    }
    assert_eq!(
        get(&daemon, &format!("/artifacts/{id}/index.html")).status,
        200
    );
}

#[test]
fn an_artifact_survives_a_daemon_restart() {
    let dir = temp_dir("restart");
    let daemon = start_daemon(&dir);
    let created = json(&run(&daemon, &["artifact", "new", "--title", "Kept"]));
    let id = created["id"].as_str().unwrap().to_string();
    std::fs::write(
        dir.join("artifacts").join(&id).join("index.html"),
        "<p>kept</p>",
    )
    .unwrap();
    drop(daemon);

    let daemon = start_daemon(&dir);
    let shown = json(&run(&daemon, &["artifact", "show", &id]));
    assert_eq!(shown["title"], "Kept");
    assert_eq!(shown["createdAt"], created["createdAt"]);
    assert_eq!(
        get(&daemon, &format!("/artifacts/{id}/")).body,
        b"<p>kept</p>"
    );
}

#[test]
fn put_refuses_a_missing_source_and_bad_usage() {
    let daemon = start_daemon(&temp_dir("bad"));
    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let missing = run(&daemon, &["artifact", "put", &id, "/no/such/file.html"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&missing.stderr).trim(),
        "canvasd returned HTTP 400 (no file or folder at /no/such/file.html)"
    );
    assert_eq!(
        run(&daemon, &["artifact", "new", "--title"]).status.code(),
        Some(2)
    );
    assert_eq!(run(&daemon, &["artifact"]).status.code(), Some(2));
}

#[test]
fn put_refuses_the_artifacts_own_folder_and_keeps_its_files() {
    let daemon = start_daemon(&temp_dir("self"));
    let created = json(&run(&daemon, &["artifact", "new"]));
    let id = created["id"].as_str().unwrap().to_string();
    let folder = PathBuf::from(created["path"].as_str().unwrap());
    std::fs::write(folder.join("index.html"), "<p>kept</p>").unwrap();
    for source in [
        folder.clone(),
        folder.join("index.html"),
        daemon.data_dir.join("artifacts"),
    ] {
        let output = run(&daemon, &["artifact", "put", &id, source.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{}", source.display());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("HTTP 400"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        std::fs::read(folder.join("index.html")).unwrap(),
        b"<p>kept</p>"
    );
}

#[test]
fn put_skips_a_link_to_nothing_in_the_source() {
    let daemon = start_daemon(&temp_dir("dangling"));
    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let src = temp_dir("dangling-src");
    std::fs::write(src.join("index.html"), "<p>ok</p>").unwrap();
    std::os::unix::fs::symlink(src.join("gone"), src.join("broken")).unwrap();
    let put = json(&run(
        &daemon,
        &["artifact", "put", &id, src.to_str().unwrap()],
    ));
    assert_eq!(put["entry"], "index.html");
    let folder = daemon.data_dir.join("artifacts").join(&id);
    assert!(folder.join("index.html").is_file());
    assert!(std::fs::symlink_metadata(folder.join("broken")).is_err());
}

/// A viewer as canvasd counts one: an open `/api/events` stream. Returns once
/// the response headers arrive, so the stream is subscribed.
fn connect_viewer(daemon: &Daemon) -> std::os::unix::net::UnixStream {
    use std::io::{Read, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(&daemon.socket).unwrap();
    stream
        .write_all(b"GET /api/events HTTP/1.1\r\nHost: canvas\r\n\r\n")
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = [0u8; 256];
    let n = stream.read(&mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).contains("200 OK"));
    stream
}

fn read_until(stream: &mut std::os::unix::net::UnixStream, needle: &str) -> String {
    use std::io::Read;
    let mut seen = String::new();
    let mut buf = [0u8; 1024];
    while !seen.contains(needle) {
        let n = stream.read(&mut buf).expect("event within 5s");
        assert!(n > 0, "stream closed before {needle}: {seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    seen
}

#[test]
fn focus_on_an_artifact_reaches_the_viewer_as_artifact_focus() {
    let daemon = start_daemon(&temp_dir("focus"));
    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut viewer = connect_viewer(&daemon);
    let output = run(&daemon, &["focus", &id]);
    assert_eq!(json(&output), serde_json::json!({"viewers": 1}));
    let seen = read_until(&mut viewer, "event: artifact-focus");
    assert!(seen.contains(&format!("{{\"id\":\"{id}\"}}")), "{seen}");
}

#[test]
fn focus_on_an_artifact_fails_with_no_viewer_or_an_unknown_id() {
    let daemon = start_daemon(&temp_dir("nofocus"));
    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let none = run(&daemon, &["focus", &id]);
    assert_eq!(none.status.code(), Some(1));
    assert!(none.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&none.stderr).trim(),
        "no Canvas viewer is open to show the artifact"
    );
    let unknown = run(&daemon, &["focus", "art-0000000000"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&unknown.stderr).trim(),
        "canvasd returned HTTP 404 (no artifact with that id)"
    );
}
