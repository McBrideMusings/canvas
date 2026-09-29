//! Integration coverage for `canvas profile`. Spawns the real built binary
//! as a throwaway daemon on its own socket and drives each verb against it.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn canvas_bin() -> &'static str {
    env!("CARGO_BIN_EXE_canvas")
}

struct Daemon {
    child: std::process::Child,
    dir: std::path::PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn spawn_daemon() -> Daemon {
    let dir = std::env::temp_dir().join(format!(
        "canvas-profile-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(canvas_bin())
        .arg("daemon")
        .env("CANVAS_DATA_DIR", &dir)
        .env("CANVAS_SOCKET", dir.join("canvasd.sock"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn canvas daemon");
    let daemon = Daemon { child, dir };
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::os::unix::net::UnixStream::connect(daemon.dir.join("canvasd.sock")).is_err() {
        assert!(Instant::now() < deadline, "daemon never came up");
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
}

fn canvas(daemon: &Daemon, args: &[&str], stdin: Option<&str>, cwd: Option<&std::path::Path>) -> Output {
    let mut cmd = Command::new(canvas_bin());
    cmd.arg("profile")
        .args(args)
        .env("CANVAS_DATA_DIR", &daemon.dir)
        .env("CANVAS_SOCKET", daemon.dir.join("canvasd.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().expect("failed to run canvas profile");
    let mut pipe = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        pipe.write_all(text.as_bytes()).unwrap();
    }
    drop(pipe);
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn set_reads_stdin_and_show_prints_the_text() {
    let d = spawn_daemon();
    let set = canvas(&d, &["set", "terse", "-"], Some("Post rarely.\n"), None);
    assert!(set.status.success(), "{}", err(&set));
    let show = canvas(&d, &["show", "terse"], None, None);
    assert_eq!(out(&show), "Post rarely.\n");
}

#[test]
fn list_names_profiles_assignments_and_the_builtin() {
    let d = spawn_daemon();
    canvas(&d, &["set", "terse", "-"], Some("x"), None);
    canvas(&d, &["assign", "terse"], None, None);
    canvas(&d, &["assign", "terse", "--repo", "octo/hello"], None, None);
    let list = out(&canvas(&d, &["list"], None, None));
    assert!(list.contains("terse  (global)"), "{list}");
    assert!(list.contains("repo octo/hello -> terse"), "{list}");
    assert!(list.contains("built-in default: present"), "{list}");
}

#[test]
fn delete_removes_the_profile_and_an_unknown_name_fails() {
    let d = spawn_daemon();
    canvas(&d, &["set", "terse", "-"], Some("x"), None);
    assert!(canvas(&d, &["delete", "terse"], None, None).status.success());
    let show = canvas(&d, &["show", "terse"], None, None);
    assert_eq!(show.status.code(), Some(1));
    assert!(err(&show).contains("no profile named \"terse\""));
    assert_eq!(canvas(&d, &["delete", "terse"], None, None).status.code(), Some(1));
}

#[test]
fn assign_global_is_what_effective_returns_and_unassign_clears_it() {
    let d = spawn_daemon();
    canvas(&d, &["set", "terse", "-"], Some("Post rarely.\n"), None);
    assert!(canvas(&d, &["assign", "terse"], None, None).status.success());
    let eff = canvas(&d, &["show", "--effective", "--repo", "octo/hello"], None, None);
    assert_eq!(out(&eff), "Post rarely.");
    assert!(canvas(&d, &["unassign"], None, None).status.success());
    let eff = out(&canvas(&d, &["show", "--effective", "--repo", "octo/hello"], None, None));
    assert_ne!(eff, "Post rarely.");
    assert!(!eff.is_empty(), "falls back to the built-in default");
}

#[test]
fn assign_to_an_unknown_profile_fails_loudly() {
    let d = spawn_daemon();
    let o = canvas(&d, &["assign", "nope", "--repo", "octo/hello"], None, None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o).trim(), "canvasd returned HTTP 400: no profile named \"nope\"");
}

#[test]
fn here_assigns_the_repo_the_hook_reads_with_cwd() {
    let d = spawn_daemon();
    let repo = d.dir.join("checkout");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git@github.com:octo/hello.git"],
    ] {
        let status = Command::new("git").arg("-C").arg(&repo).args(&args).status().unwrap();
        assert!(status.success());
    }
    canvas(&d, &["set", "terse", "-"], Some("Repo text.\n"), None);
    let assign = canvas(&d, &["assign", "terse", "--here"], None, Some(&repo));
    assert!(assign.status.success(), "{}", err(&assign));
    assert!(out(&assign).contains("octo/hello"));
    // No --repo: the daemon resolves ?cwd= itself, as the hook does.
    // Additive is the default mode, so a repo-only assignment is the built-in text then the repo's.
    let eff = out(&canvas(&d, &["show", "--effective"], None, Some(&repo)));
    assert!(eff.ends_with("\n\nRepo text."), "{eff}");
    assert!(canvas(&d, &["unassign", "--here"], None, Some(&repo)).status.success());
    assert!(!out(&canvas(&d, &["show", "--effective"], None, Some(&repo))).contains("Repo text."));
}

#[test]
fn here_outside_a_github_checkout_fails() {
    let d = spawn_daemon();
    canvas(&d, &["set", "terse", "-"], Some("x"), None);
    let o = canvas(&d, &["assign", "terse", "--here"], None, Some(&d.dir));
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("github.com origin"), "{}", err(&o));
}
