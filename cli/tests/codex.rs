//! A Codex session posts and gets instructions the way a Claude Code one does,
//! keyed by `CODEX_THREAD_ID`. Spawns the real built binary as a throwaway
//! daemon on its own socket.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const INSTRUCTIONS: &str = include_str!("../../plugin/instructions.md");

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
        "canvas-codex-test-{}-{}",
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
        assert!(Instant::now() < deadline, "daemon never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn run(daemon: &Daemon, args: &[&str], stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(canvas_bin())
        .args(args)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env("CODEX_THREAD_ID", "abc")
        .env("CANVAS_SOCKET", &daemon.socket)
        .env("CANVAS_DATA_DIR", daemon.socket.parent().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run canvas");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn post_with_only_codex_thread_id_creates_a_codex_session() {
    let daemon = spawn_daemon();
    let posted = run(
        &daemon,
        &["post", "-", "--format", "html"],
        b"<p>from codex</p>",
    );
    assert!(posted.status.success(), "{posted:?}");
    let posted: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    let card_id = posted["card_id"].as_str().unwrap();

    let card = run(&daemon, &["card", card_id], b"");
    let card: serde_json::Value = serde_json::from_slice(&card.stdout).unwrap();
    assert_eq!(card["sessionId"], "abc");

    let sessions = canvasd_sessions(&daemon);
    assert_eq!(sessions[0]["id"], "abc");
    assert_eq!(sessions[0]["agent"], "codex");
}

/// The session list as the viewer reads it.
fn canvasd_sessions(daemon: &Daemon) -> Vec<serde_json::Value> {
    use std::io::Read;
    let mut stream = std::os::unix::net::UnixStream::connect(&daemon.socket).unwrap();
    stream
        .write_all(b"GET /api/state HTTP/1.0\r\nHost: canvas\r\n\r\n")
        .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    let state: serde_json::Value = serde_json::from_str(body).unwrap();
    state["sessions"].as_array().unwrap().clone()
}

#[test]
fn session_start_with_codex_stdin_prints_the_instructions() {
    let daemon = spawn_daemon();
    let output = run(
        &daemon,
        &["hook", "session-start", "--agent", "codex"],
        br#"{"session_id":"abc","transcript_path":null,"cwd":"/tmp","hook_event_name":"SessionStart","model":"m","source":"startup"}"#,
    );
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), INSTRUCTIONS);
}
