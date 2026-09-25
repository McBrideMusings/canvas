//! Integration coverage for `canvas guidance` and the guidance block
//! `canvas hook session-start` prints to stdout. Spawns the real built
//! binary as a throwaway daemon on a free port so the hook path is driven
//! end to end, never faked.

use std::io::Write;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const GUIDANCE: &str = include_str!("../../plugin/guidance.md");

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
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

fn spawn_daemon(port: u16, data_dir: &std::path::Path) -> Daemon {
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_PORT", port.to_string())
        .env("CANVAS_DATA_DIR", data_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let daemon = Daemon { child };

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        if Instant::now() > deadline {
            panic!("daemon on port {port} never came up");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn run_hook_session_start(canvas_url: &str) -> std::process::Output {
    let mut child = Command::new(canvas_bin())
        .args(["hook", "session-start"])
        .env("CANVAS_URL", canvas_url)
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
    let port = free_port();
    let data_dir = std::env::temp_dir().join(format!("canvas-guidance-test-{port}"));
    std::fs::create_dir_all(&data_dir).unwrap();
    let _daemon = spawn_daemon(port, &data_dir);

    let output = run_hook_session_start(&format!("http://127.0.0.1:{port}"));
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), GUIDANCE);
}

#[test]
fn session_start_prints_nothing_and_exits_0_when_the_daemon_is_down() {
    // An address in the TEST-NET-1 documentation range: routable but
    // guaranteed nothing answers, so the connection attempt actually times
    // out rather than failing fast with connection-refused.
    let start = Instant::now();
    let output = run_hook_session_start("http://192.0.2.1:8229");
    let elapsed = start.elapsed();

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        elapsed < Duration::from_secs(3),
        "hook took {elapsed:?}, expected it to respect the 1s client timeout"
    );
}
