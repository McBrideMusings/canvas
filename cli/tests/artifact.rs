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

/// An HTML page as the artifact holds it: canvasd serves it with its error
/// relay script first, which this cuts off.
fn page_of(body: &[u8]) -> &[u8] {
    let relay = b"<script>(() => { const send = ";
    assert!(body.starts_with(relay), "served without the error relay");
    let end = body.windows(9).position(|w| w == b"</script>").unwrap() + 9;
    &body[end..]
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
    assert_eq!(
        page_of(&page.body),
        br#"<script crossorigin src="./app.js"></script>"#
    );
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
fn the_canvas_stylesheet_is_served_and_an_artifact_may_link_it() {
    let daemon = start_daemon(&temp_dir("stylesheet"));
    let sheet = get(&daemon, "/canvas.css");
    assert_eq!(sheet.status, 200);
    assert_eq!(sheet.content_type.as_deref(), Some("text/css"));
    let css = String::from_utf8_lossy(&sheet.body);
    assert!(css.contains("--color-surface:"), "the tokens");
    assert!(css.contains("h1 {"), "the base styles");

    let id = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let folder = daemon.data_dir.join("artifacts").join(&id);
    std::fs::write(folder.join("index.html"), "<p>in</p>").unwrap();
    let page = get(&daemon, &format!("/artifacts/{id}/"));
    let csp = page
        .headers
        .iter()
        .find(|(k, _)| k == "content-security-policy")
        .map(|(_, v)| v.as_str())
        .expect("a CSP header");
    let style_src = csp
        .split(';')
        .find(|d| d.trim_start().starts_with("style-src"))
        .expect("a style-src directive");
    assert!(
        style_src.contains(" canvas://localhost/canvas.css "),
        "{csp}"
    );
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
        page_of(&get(&daemon, &format!("/artifacts/{id}/")).body),
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

fn stderr(output: &std::process::Output) -> String {
    assert_eq!(output.status.code(), Some(1), "expected failure");
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

#[test]
fn a_linked_folder_serves_goes_missing_and_relinks_with_its_id() {
    let daemon = start_daemon(&temp_dir("link"));
    let repo = temp_dir("repo");
    let web = repo.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<p>linked</p>").unwrap();
    std::fs::write(repo.join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(repo.join("secret.txt"), web.join("out.txt")).unwrap();

    let created = json(&run(
        &daemon,
        &[
            "artifact",
            "new",
            "--link",
            web.to_str().unwrap(),
            "--title",
            "Dash",
        ],
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["kind"], "linked");
    assert_eq!(created["link"], web.to_str().unwrap());
    assert_eq!(created["path"], web.to_str().unwrap());
    assert_eq!(created["entry"], "index.html");
    assert_eq!(created.get("sourceMissing"), None);
    assert!(!daemon.data_dir.join("artifacts").join(&id).exists());
    assert_eq!(
        page_of(&get(&daemon, &format!("/artifacts/{id}/")).body),
        b"<p>linked</p>"
    );
    for path in [
        format!("/artifacts/{id}/out.txt"),
        format!("/artifacts/{id}/../secret.txt"),
    ] {
        assert_eq!(get(&daemon, &path).status, 404, "{path}");
    }

    let moved = repo.join("web-moved");
    std::fs::rename(&web, &moved).unwrap();
    let missing = json(&run(&daemon, &["artifact", "show", &id]));
    assert_eq!(missing["sourceMissing"], true);
    assert_eq!(missing["path"], web.to_str().unwrap());
    let gone = get(&daemon, &format!("/artifacts/{id}/"));
    assert_eq!(gone.status, 404);
    assert_eq!(gone.body, b"the linked source is missing");

    let relinked = json(&run(
        &daemon,
        &["artifact", "relink", &id, moved.to_str().unwrap()],
    ));
    assert_eq!(relinked["id"], id.as_str());
    assert_eq!(relinked["createdAt"], created["createdAt"]);
    assert_eq!(relinked["link"], moved.to_str().unwrap());
    assert_eq!(relinked.get("sourceMissing"), None);
    assert_eq!(
        page_of(&get(&daemon, &format!("/artifacts/{id}/")).body),
        b"<p>linked</p>"
    );

    json(&run(&daemon, &["artifact", "delete", &id]));
    assert!(
        moved.join("index.html").is_file(),
        "delete keeps linked files"
    );
}

#[test]
fn a_linked_html_file_is_the_whole_artifact() {
    let daemon = start_daemon(&temp_dir("linkfile"));
    let dir = temp_dir("page");
    std::fs::write(
        dir.join("report.html"),
        r#"<meta name="canvas-size" content="400x300"><p>one</p>"#,
    )
    .unwrap();
    std::fs::write(dir.join("style.css"), "p{}").unwrap();
    let page = dir.join("report.html");
    let created = json(&run(
        &daemon,
        &["artifact", "new", "--link", page.to_str().unwrap()],
    ));
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["entry"], "report.html");
    assert_eq!(
        created["size"],
        serde_json::json!({"width": 400, "height": 300})
    );
    let served = get(&daemon, &format!("/artifacts/{id}/"));
    assert_eq!(served.status, 200);
    assert!(served.body.ends_with(b"<p>one</p>"));
    for rel in ["style.css", "report.html"] {
        assert_eq!(
            get(&daemon, &format!("/artifacts/{id}/{rel}")).status,
            404,
            "{rel}"
        );
    }
}

#[test]
fn link_and_relink_refuse_what_they_cannot_serve() {
    let daemon = start_daemon(&temp_dir("linkbad"));
    let dir = temp_dir("notes");
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    assert_eq!(
        stderr(&run(
            &daemon,
            &["artifact", "new", "--link", "/no/such/dir"]
        )),
        "canvasd returned HTTP 400 (no folder or HTML file at /no/such/dir)"
    );
    let txt = dir.join("notes.txt");
    assert_eq!(
        stderr(&run(
            &daemon,
            &["artifact", "new", "--link", txt.to_str().unwrap()]
        )),
        format!(
            "canvasd returned HTTP 400 ({} is not a folder or an .html file)",
            txt.display()
        )
    );
    assert!(json(&run(&daemon, &["artifact", "list"]))
        .as_array()
        .unwrap()
        .is_empty());

    let owned = json(&run(&daemon, &["artifact", "new"]))["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        stderr(&run(&daemon, &["artifact", "relink", &owned, dir.to_str().unwrap()])),
        format!("canvasd returned HTTP 400 ({owned} is an owned artifact; only a linked artifact can be relinked)")
    );
    let linked = json(&run(
        &daemon,
        &["artifact", "new", "--link", dir.to_str().unwrap()],
    ))["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        stderr(&run(
            &daemon,
            &["artifact", "put", &linked, txt.to_str().unwrap()]
        )),
        format!(
            "canvasd returned HTTP 400 ({linked} is linked to {}; save its files there instead)",
            dir.display()
        )
    );
    assert_eq!(
        stderr(&run(
            &daemon,
            &["artifact", "relink", &linked, "/no/such/dir"]
        )),
        "canvasd returned HTTP 400 (no folder or HTML file at /no/such/dir)"
    );
    assert_eq!(
        json(&run(&daemon, &["artifact", "show", &linked]))["link"],
        dir.to_str().unwrap()
    );
    assert_eq!(
        run(&daemon, &["artifact", "new", "--link"]).status.code(),
        Some(2)
    );
}
