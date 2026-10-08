//! canvasd and the CLI write timestamped lines to `logs/` under the data dir,
//! and a log folder that can't be written changes nothing a command prints
//! or how it exits. Spawns the real built binary as a throwaway daemon on its
//! own socket.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Daemon {
    child: std::process::Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("canvas-logs-{name}-{}", std::process::id()));
    if dir.exists() {
        let _ = std::fs::set_permissions(dir.join("logs"), std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn spawn_daemon(dir: &Path) -> Daemon {
    let socket = dir.join("canvasd.sock");
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_DATA_DIR", dir)
        .env("CANVAS_SOCKET", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let daemon = Daemon { child };
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        if Instant::now() > deadline {
            panic!("daemon on {} never came up", socket.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn run(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(canvas_bin())
        .args(args)
        .env("CANVAS_DATA_DIR", dir)
        .env("CANVAS_SOCKET", dir.join("canvasd.sock"))
        .env("CLAUDE_CODE_SESSION_ID", "logs-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run canvas");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

/// `canvas hook prompt` with an empty transcript: it reads the reminder layers
/// and prints nothing.
fn run_hook_prompt(dir: &Path) -> Output {
    let transcript = dir.join("transcript.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let input = serde_json::json!({
        "session_id": "logs-test",
        "cwd": dir,
        "transcript_path": transcript,
    });
    run(dir, &["hook", "prompt"], input.to_string().as_bytes())
}

fn read_log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join("logs").join(name)).unwrap_or_default()
}

/// The daemon hands each line to a writer thread after answering, so a line
/// can land a moment after the client has exited; wait for it.
fn wait_for_line(dir: &Path, name: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = read_log(dir, name);
        if log.contains(needle) || Instant::now() > deadline {
            return log;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_post_writes_its_request_line_to_the_daemon_log() {
    let dir = test_dir("post");
    let _daemon = spawn_daemon(&dir);
    let posted = run(&dir, &["post", "-", "--format", "html"], b"<p>hi</p>");
    assert!(posted.status.success(), "{posted:?}");

    let log = wait_for_line(&dir, "daemon.log", "POST /api/posts");
    let line = log
        .lines()
        .find(|l| l.contains("POST /api/posts"))
        .unwrap_or_else(|| panic!("no POST /api/posts line in:\n{log}"));
    assert!(line.contains(" daemon INFO "), "{line}");
    assert!(line.contains("status=200"), "{line}");
    assert!(line.contains(" ms="), "{line}");
    assert!(read_log(&dir, "cli.log").contains("POST /api/posts status=200"));
}

#[test]
fn a_hook_with_canvasd_stopped_exits_0_and_logs_it_was_unreachable() {
    let dir = test_dir("unreachable");
    let input = serde_json::json!({"session_id": "logs-test", "cwd": dir});
    let output = run(&dir, &["hook", "session-end"], input.to_string().as_bytes());
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty() && output.stderr.is_empty());

    let log = read_log(&dir, "cli.log");
    assert!(
        log.lines()
            .any(|l| l.contains(" cli WARN canvasd unreachable ")
                && l.contains("/api/sessions/logs-test/end")),
        "{log}"
    );
    assert!(
        log.contains("hook failed event=session-end agent=claude-code"),
        "{log}"
    );
}

#[test]
fn logs_path_prints_an_absolute_folder_that_exists() {
    let dir = test_dir("path");
    let output = run(&dir, &["logs", "--path"], b"");
    assert!(output.status.success(), "{output:?}");
    let printed = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    assert!(printed.is_absolute());
    assert!(printed.is_dir());
    assert_eq!(printed, dir.join("logs"));
}

#[test]
fn an_unwritable_log_folder_changes_no_exit_code_or_output() {
    let writable = test_dir("writable");
    let locked = test_dir("locked");
    let logs = locked.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o500)).unwrap();

    for dir in [&writable, &locked] {
        let hook = run_hook_prompt(dir);
        assert_eq!(hook.status.code(), Some(0));
        assert!(hook.stdout.is_empty() && hook.stderr.is_empty(), "{hook:?}");
    }

    let _daemon = spawn_daemon(&locked);
    let posted = run(&locked, &["post", "-", "--format", "html"], b"<p>hi</p>");
    assert_eq!(posted.status.code(), Some(0), "{posted:?}");
    assert!(posted.stderr.is_empty(), "{posted:?}");
    let json: serde_json::Value = serde_json::from_slice(&posted.stdout).unwrap();
    assert!(json["card_id"].is_string());

    let path = run(&locked, &["logs", "--path"], b"");
    assert_eq!(path.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(path.stdout).unwrap().trim(),
        logs.to_str().unwrap()
    );

    assert_eq!(std::fs::read_dir(&logs).unwrap().count(), 0);
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o755)).unwrap();
}
