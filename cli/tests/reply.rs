//! Integration coverage for canvas-17z: a card's reply reaching `canvas
//! wait`/`canvas replies`. Spawns the real built binary as a throwaway
//! daemon on its own socket, posts a real card through it, and posts the
//! reply the way the viewer would — directly to `/api/cards/:id/reply` —
//! since the browser side (the click, the postMessage) was proven
//! separately and isn't something `cargo test` can drive.

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
    let data_dir = std::env::temp_dir().join(format!("canvas-reply-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
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
    loop {
        if std::os::unix::net::UnixStream::connect(&daemon.socket).is_ok() {
            break;
        }
        if Instant::now() > deadline {
            panic!("daemon on {} never came up", daemon.socket.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn post_card(daemon: &Daemon) -> String {
    let output = Command::new(canvas_bin())
        .args(["post", "-", "--format", "html"])
        .env("CANVAS_SOCKET", &daemon.socket)
        .env("CLAUDE_CODE_SESSION_ID", "reply-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"<button id=\"a\">A</button>")?;
            child.wait_with_output()
        })
        .expect("failed to run canvas post");
    assert!(output.status.success(), "{:?}", output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    parsed["card_id"].as_str().unwrap().to_string()
}

/// Posts a reply the way the viewer does — a plain POST to
/// `/api/cards/:id/reply` — standing in for the click and postMessage that
/// only a real browser can perform.
fn reply_as_viewer(daemon: &Daemon, card_id: &str, value: &str) {
    let body = serde_json::json!(value).to_string();
    let response = canvas_core::unix_http::request(
        &daemon.socket,
        "POST",
        &format!("/api/cards/{card_id}/reply"),
        &[("Content-Type", "application/json")],
        body.as_bytes(),
        Some(Duration::from_secs(2)),
    )
    .expect("viewer's reply POST failed");
    assert!((200..300).contains(&response.status), "status {}", response.status);
}

#[test]
fn wait_returns_the_reply_once_it_lands() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);
    reply_as_viewer(&daemon, &card_id, "A");

    let output = Command::new(canvas_bin())
        .args(["wait", &card_id, "--timeout", "5"])
        .env("CANVAS_SOCKET", &daemon.socket)
        .output()
        .expect("failed to run canvas wait");
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "\"A\"");
}

#[test]
fn wait_times_out_when_nothing_replies() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);

    let start = Instant::now();
    let output = Command::new(canvas_bin())
        .args(["wait", &card_id, "--timeout", "1"])
        .env("CANVAS_SOCKET", &daemon.socket)
        .output()
        .expect("failed to run canvas wait");
    let elapsed = start.elapsed();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(3));
}

#[test]
fn replies_is_non_blocking_and_checks_again_later() {
    let daemon = spawn_daemon();
    let card_id = post_card(&daemon);

    let output = Command::new(canvas_bin())
        .args(["replies", &card_id])
        .env("CANVAS_SOCKET", &daemon.socket)
        .output()
        .expect("failed to run canvas replies");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());

    reply_as_viewer(&daemon, &card_id, "B");

    let output = Command::new(canvas_bin())
        .args(["replies", &card_id])
        .env("CANVAS_SOCKET", &daemon.socket)
        .output()
        .expect("failed to run canvas replies");
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "\"B\"");
}

#[test]
fn a_second_daemon_refuses_a_socket_that_answers() {
    let daemon = spawn_daemon();

    let output = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_SOCKET", &daemon.socket)
        .output()
        .expect("failed to run a second canvas daemon");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already running"));
    // The first daemon still owns the socket.
    assert!(std::os::unix::net::UnixStream::connect(&daemon.socket).is_ok());
}
