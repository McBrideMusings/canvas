//! Integration coverage for `canvas guidance` and the guidance block
//! `canvas hook session-start` prints to stdout. Spawns the real built
//! binary as a throwaway daemon on its own socket so the hook path is driven
//! end to end, never faked.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GUIDANCE: &str = include_str!("../../plugin/guidance.md");

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

fn spawn_daemon(socket: &std::path::Path, data_dir: &std::path::Path) -> Daemon {
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_DATA_DIR", data_dir)
        .env("CANVAS_SOCKET", socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let daemon = Daemon { child };

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            break;
        }
        if Instant::now() > deadline {
            panic!("daemon on {} never came up", socket.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("canvas-guidance-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_hook_session_start(socket: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(canvas_bin())
        .args(["hook", "session-start"])
        .env("CANVAS_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn canvas hook session-start");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"session_id":"guidance-test","cwd":"/tmp"}"#)
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn guidance_subcommand_prints_the_block_byte_for_byte() {
    let output = Command::new(canvas_bin())
        .arg("guidance")
        .output()
        .expect("failed to run canvas guidance");
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), GUIDANCE);
}

#[test]
fn session_start_prints_guidance_when_the_daemon_is_up() {
    let dir = test_dir("up");
    let socket = dir.join("canvasd.sock");
    let _daemon = spawn_daemon(&socket, &dir);

    let output = run_hook_session_start(&socket);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), GUIDANCE);
}

#[test]
fn session_start_still_prints_guidance_when_canvasd_never_answers() {
    // A socket that accepts the connection and holds it: registering the
    // session fails on the client's 1s timeout, but posting only needs a
    // running canvasd, not a registered session, so the guidance still
    // belongs in the transcript.
    let dir = test_dir("wedged");
    let socket = dir.join("canvasd.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

    let start = Instant::now();
    let output = run_hook_session_start(&socket);
    let elapsed = start.elapsed();

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), GUIDANCE);
    assert!(
        elapsed < Duration::from_secs(3),
        "hook took {elapsed:?}, expected it to respect the 1s client timeout"
    );
}

#[test]
fn session_start_prints_guidance_when_canvasd_is_not_running() {
    // No socket file at all — the failure mode of a canvasd restart
    // mid-deploy.
    let dir = test_dir("down");
    let output = run_hook_session_start(&dir.join("canvasd.sock"));

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), GUIDANCE);
}
