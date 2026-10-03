//! `canvas card <id>` prints a card the way canvasd holds it, so an agent
//! handed a pasted post link can read what the viewer renders. Spawns the
//! real built binary as a throwaway daemon on its own socket.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Daemon {
    child: std::process::Child,
    socket: std::path::PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn spawn_daemon() -> Daemon {
    let data_dir = std::env::temp_dir().join(format!(
        "canvas-card-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();
    let socket = data_dir.join("canvasd.sock");
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_DATA_DIR", &data_dir)
        .env("CANVAS_SOCKET", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let daemon = Daemon { child, socket };
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&daemon.socket).is_err() {
        if Instant::now() > deadline {
            panic!("daemon on {} never came up", daemon.socket.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn run(daemon: &Daemon, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut child = Command::new(canvas_bin())
        .args(args)
        .env("CANVAS_SOCKET", &daemon.socket)
        .env("CANVAS_DATA_DIR", daemon.socket.parent().unwrap())
        .env("CLAUDE_CODE_SESSION_ID", "card-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run canvas");
    {
        use std::io::Write;
        let mut pipe = child.stdin.take().unwrap();
        if let Some(bytes) = stdin {
            pipe.write_all(bytes).unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

#[test]
fn card_prints_the_posted_html_and_ids() {
    let daemon = spawn_daemon();
    let posted = run(
        &daemon,
        &["post", "-", "--format", "html"],
        Some(b"<p id=\"x\">hello</p>"),
    );
    assert!(posted.status.success(), "{:?}", posted);
    let posted: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    let card_id = posted["card_id"].as_str().unwrap();

    let output = run(&daemon, &["card", card_id], None);
    assert!(output.status.success(), "{:?}", output);
    let card: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(card["id"], card_id);
    assert_eq!(card["sessionId"], "card-test");
    assert!(card["html"]
        .as_str()
        .unwrap()
        .contains("<p id=\"x\">hello</p>"));
}

#[test]
fn card_fails_loudly_for_an_unknown_id() {
    let daemon = spawn_daemon();
    let output = run(&daemon, &["card", "no-such-card"], None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("404"));
}

fn post_card(daemon: &Daemon) -> String {
    let posted = run(
        daemon,
        &["post", "-", "--format", "html"],
        Some(b"<p>x</p>"),
    );
    assert!(posted.status.success(), "{:?}", posted);
    let posted: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    posted["card_id"].as_str().unwrap().to_string()
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

#[test]
fn focus_reports_the_viewers_it_reached() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    let _viewer = connect_viewer(&daemon);
    let output = run(&daemon, &["focus", &card_id], None);
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value, serde_json::json!({"viewers": 1}));
}

#[test]
fn focus_with_no_viewer_fails_with_one_line() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    let output = run(&daemon, &["focus", &card_id], None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("no Canvas viewer"), "{stderr}");
}

#[test]
fn focus_fails_for_an_unknown_id() {
    let daemon = spawn_daemon();
    let _viewer = connect_viewer(&daemon);
    let output = run(&daemon, &["focus", "no-such-card"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("404"));
}

#[test]
fn theme_reports_the_viewers_it_reached() {
    let daemon = spawn_daemon();
    let _viewer = connect_viewer(&daemon);
    let output = run(&daemon, &["theme", "dark"], None);
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value, serde_json::json!({"viewers": 1}));
}

#[test]
fn theme_with_no_viewer_fails_with_one_line() {
    let daemon = spawn_daemon();
    let output = run(&daemon, &["theme", "light"], None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("no Canvas viewer"), "{stderr}");
}

#[test]
fn theme_refuses_anything_but_light_or_dark() {
    let daemon = spawn_daemon();
    let output = run(&daemon, &["theme", "sepia"], None);
    assert_eq!(output.status.code(), Some(2), "{:?}", output);
}

#[test]
fn bare_theme_prints_what_the_viewer_reported() {
    use std::io::{Read, Write};
    let daemon = spawn_daemon();
    let output = run(&daemon, &["theme"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reported"));

    let viewer = connect_viewer(&daemon);
    let body = r#"{"theme":"dark"}"#;
    let mut stream = std::os::unix::net::UnixStream::connect(&daemon.socket).unwrap();
    write!(
        stream,
        "PUT /api/theme HTTP/1.1\r\nHost: canvas\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.contains("204"), "{reply}");

    let output = run(&daemon, &["theme"], None);
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value, serde_json::json!({"theme": "dark"}));

    drop(viewer);
    std::thread::sleep(Duration::from_millis(200));
    let output = run(&daemon, &["theme"], None);
    assert!(!output.status.success(), "{:?}", output);
}

#[test]
fn post_focus_creates_the_card_and_focuses_it() {
    let daemon = spawn_daemon();
    let _viewer = connect_viewer(&daemon);
    let posted = run(
        &daemon,
        &["post", "-", "--format", "html", "--focus"],
        Some(b"<p>x</p>"),
    );
    assert!(posted.status.success(), "{:?}", posted);
    let posted: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    assert_eq!(posted["viewers"], 1);
    let card = run(
        &daemon,
        &["card", posted["card_id"].as_str().unwrap()],
        None,
    );
    assert!(card.status.success(), "{:?}", card);
}

#[test]
fn post_update_focus_focuses_the_updated_card() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    let _viewer = connect_viewer(&daemon);
    let updated = run(
        &daemon,
        &[
            "post", "--update", &card_id, "--focus", "-", "--format", "html",
        ],
        Some(b"<p>y</p>"),
    );
    assert!(updated.status.success(), "{:?}", updated);
    let updated: serde_json::Value = serde_json::from_slice(&updated.stdout).unwrap();
    assert_eq!(updated["card_id"], card_id.as_str());
    assert_eq!(updated["viewers"], 1);
}

#[test]
fn post_focus_with_no_viewer_still_posts_and_warns() {
    let daemon = spawn_daemon();
    let posted = run(
        &daemon,
        &["post", "-", "--format", "html", "--focus"],
        Some(b"<p>x</p>"),
    );
    assert!(posted.status.success(), "{:?}", posted);
    let value: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    assert_eq!(value["viewers"], 0);
    assert!(String::from_utf8_lossy(&posted.stderr).contains("no Canvas viewer"));
}

/// Plays a viewer for one `canvas snapshot`: waits for the `card-snapshot`
/// event on `viewer` and answers its request with `content_type` and `body`.
fn answer_snapshot(
    daemon: &Daemon,
    mut viewer: std::os::unix::net::UnixStream,
    content_type: &'static str,
    body: Vec<u8>,
) -> std::thread::JoinHandle<()> {
    let socket = daemon.socket.clone();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut seen = String::new();
        let request = loop {
            let mut buf = [0u8; 1024];
            let n = viewer.read(&mut buf).unwrap();
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if let Some(at) = seen.find("\"request\":\"") {
                let rest = &seen[at + 11..];
                if let Some(end) = rest.find('"') {
                    break rest[..end].to_string();
                }
            }
        };
        let mut answer = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        let head = format!(
            "POST /api/snapshots/{request} HTTP/1.1\r\nHost: canvas\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        answer.write_all(head.as_bytes()).unwrap();
        answer.write_all(&body).unwrap();
        let mut response = String::new();
        answer.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");
    })
}

#[test]
fn snapshot_writes_the_png_the_viewer_captured() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&846u32.to_be_bytes());
    png.extend_from_slice(&120u32.to_be_bytes());
    let viewer = answer_snapshot(&daemon, connect_viewer(&daemon), "image/png", png.clone());
    let out = daemon.socket.parent().unwrap().join("shot.png");

    let output = run(
        &daemon,
        &["snapshot", &card_id, out.to_str().unwrap()],
        None,
    );
    viewer.join().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"path": out.to_str().unwrap(), "width": 846, "height": 120, "clipped": false})
    );
    assert_eq!(std::fs::read(&out).unwrap(), png);
}

#[test]
fn snapshot_fails_with_the_viewers_reason() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    let reason = br#"{"error":"the card is hidden by the search"}"#.to_vec();
    let viewer = answer_snapshot(&daemon, connect_viewer(&daemon), "application/json", reason);
    let out = daemon.socket.parent().unwrap().join("shot.png");

    let output = run(
        &daemon,
        &["snapshot", &card_id, out.to_str().unwrap()],
        None,
    );
    viewer.join().unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "the card is hidden by the search\n"
    );
    assert!(!out.exists());
}

#[test]
fn snapshot_with_no_viewer_fails_with_one_line() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    let out = daemon.socket.parent().unwrap().join("shot.png");
    let output = run(
        &daemon,
        &["snapshot", &card_id, out.to_str().unwrap()],
        None,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("no Canvas viewer"), "{stderr}");
    assert!(!out.exists());
}
