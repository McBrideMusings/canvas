//! `canvas export <id>` writes the card as one standalone page: images as
//! `data:` URIs, links resolved from the card's targets, its `canvas data`
//! value baked in. Spawns the real built binary as a throwaway daemon on its
//! own socket.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Daemon {
    child: std::process::Child,
    socket: PathBuf,
}

impl Daemon {
    fn dir(&self) -> &Path {
        self.socket.parent().unwrap()
    }
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
        "canvas-export-test-{}-{}",
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
        .current_dir(daemon.dir())
        .env("CANVAS_SOCKET", &daemon.socket)
        .env("CANVAS_DATA_DIR", daemon.dir())
        .env("CLAUDE_CODE_SESSION_ID", "export-test")
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

fn post(daemon: &Daemon, html: &str) -> String {
    let posted = run(
        daemon,
        &["post", "-", "--format", "html"],
        Some(html.as_bytes()),
    );
    assert!(posted.status.success(), "{:?}", posted);
    let posted: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    posted["card_id"].as_str().unwrap().to_string()
}

/// Runs `canvas export <id> -o out.html`; returns the printed JSON and the page.
fn export(daemon: &Daemon, card_id: &str) -> (serde_json::Value, String) {
    let out = daemon.dir().join("out.html");
    let output = run(
        daemon,
        &["export", card_id, "-o", out.to_str().unwrap()],
        None,
    );
    assert!(output.status.success(), "{:?}", output);
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["path"], out.to_str().unwrap());
    assert_eq!(printed["cards"], 1);
    (printed, std::fs::read_to_string(&out).unwrap())
}

#[test]
fn export_inlines_the_image() {
    let daemon = spawn_daemon();
    let image = daemon.dir().join("dot.png");
    std::fs::write(&image, b"abc").unwrap();
    let card_id = post(
        &daemon,
        &format!("<p><img src=\"{}\"></p>", image.display()),
    );

    let (printed, page) = export(&daemon, &card_id);
    assert_eq!(printed["warnings"], serde_json::json!([]));
    assert!(
        page.contains("<img src=\"data:image/png;base64,YWJj\">"),
        "{page}"
    );
    assert!(!page.contains("/api/"));
    assert!(!page.contains(&format!("src=\"{}", daemon.dir().display())));
}

#[test]
fn export_warns_for_a_deleted_image_and_shows_a_placeholder() {
    let daemon = spawn_daemon();
    let image = daemon.dir().join("gone.png");
    std::fs::write(&image, b"abc").unwrap();
    let card_id = post(&daemon, &format!("<img src=\"{}\">", image.display()));
    std::fs::remove_file(&image).unwrap();

    let (printed, page) = export(&daemon, &card_id);
    let warnings = printed["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{printed}");
    assert_eq!(warnings[0]["kind"], "missing-image");
    assert_eq!(warnings[0]["target"], image.to_str().unwrap());
    assert!(page.contains("image missing: gone.png"), "{page}");
}

#[test]
fn export_delivers_the_latest_data_value() {
    let daemon = spawn_daemon();
    let card_id = post(
        &daemon,
        "<p id=n></p><script>addEventListener('message',e=>{if(e.data.type==='canvas-data')n.textContent=e.data.value.n})</script>",
    );
    let pushed = run(&daemon, &["data", &card_id, "-"], Some(br#"{"n":42}"#));
    assert!(pushed.status.success(), "{:?}", pushed);

    let (_, page) = export(&daemon, &card_id);
    assert!(page.contains(r#"var v={"n":42}"#), "{page}");
    assert!(page.contains("type:'canvas-data',value:v"));
}

#[test]
fn export_resolves_web_and_local_links() {
    let daemon = spawn_daemon();
    let file = daemon.dir().join("notes.md");
    std::fs::write(&file, b"x").unwrap();
    let card_id = post(
        &daemon,
        &format!(
            "<a href=\"https://example.com/a\">web</a> <a href=\"{}\">notes</a>",
            file.display()
        ),
    );

    let (_, page) = export(&daemon, &card_id);
    assert!(
        page.contains(
            r#"<a href="https://example.com/a" target="_blank" rel="noopener">web</a> notes"#
        ),
        "{page}"
    );
    assert!(!page.contains("#canvas-open-"));
}

#[test]
fn export_fails_with_one_line_for_an_unknown_id() {
    let daemon = spawn_daemon();
    let output = run(&daemon, &["export", "no-such-card"], None);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("404"), "{stderr}");
    assert!(!daemon.dir().join("no-such-card.html").exists());
}

#[test]
fn export_fails_with_one_line_when_the_file_cannot_be_written() {
    let daemon = spawn_daemon();
    let card_id = post(&daemon, "<p>x</p>");
    let out = daemon.dir().join("no-such-dir").join("out.html");
    let output = run(
        &daemon,
        &["export", &card_id, "-o", out.to_str().unwrap()],
        None,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("cannot write"), "{stderr}");
}

#[test]
fn export_writes_card_id_html_by_default() {
    let daemon = spawn_daemon();
    let card_id = post(&daemon, "<h1>Title</h1>");
    let output = run(&daemon, &["export", &card_id], None);
    assert!(output.status.success(), "{:?}", output);
    let page = std::fs::read_to_string(daemon.dir().join(format!("{card_id}.html"))).unwrap();
    assert!(page.contains("<title>Title</title>"));
}
