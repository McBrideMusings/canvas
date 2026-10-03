// Keeps each coding agent's Canvas hooks current by running the installed
// `canvas integrations list --json` and `canvas integrations install <agent>`
// (cli/src/integrations/), and feeds the Settings window's Integrations tab.
// The app adds no detection or install logic of its own.
//
// A failed install's stderr line is held here until that agent's next
// successful install, so the tab can show it after the launch-time attempt
// has long finished. Nothing is persisted; a relaunch retries.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::Manager;

// How long `canvas integrations list` may run before it is killed.
const LIST_TIMEOUT: Duration = Duration::from_secs(60);

// How long `canvas integrations install` may run before it is killed. It runs
// `claude plugin` or `codex` commands, which can be slow; a Retry click waits
// on the per-agent install lock for up to this long.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

// After the child exits, how long to wait for its output pipes to close.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

// The agent names `canvas integrations install` accepts. A command argument
// outside this list never reaches a subprocess.
const AGENTS: [&str; 2] = ["claude-code", "codex"];

// `canvas integrations list --json` rows, in the shape it prints them.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct IntegrationRow {
    pub agent: String,
    pub found: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    // True while an install for this agent is running or waiting on the lock.
    // Set by `status`; `canvas integrations list` doesn't print it.
    #[serde(default)]
    pub installing: bool,
}

#[derive(Default)]
pub struct IntegrationState {
    // Agent name -> the stderr line of its last failed install.
    errors: Mutex<HashMap<String, String>>,
    // Agent name -> held for the length of that agent's install subprocess, so
    // a Retry click during the launch-time pass waits instead of running a
    // second install over the same hooks file and plugin state.
    installing: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    // Agent name -> installs running or waiting on that lock.
    pending: Mutex<HashMap<String, usize>>,
}

impl IntegrationState {
    fn install_lock(&self, agent: &str) -> Arc<Mutex<()>> {
        let mut locks = self.installing.lock().unwrap();
        locks.entry(agent.to_string()).or_default().clone()
    }

    // Counts one install for `agent` until the guard drops, so a panic can't
    // leave the agent marked as installing.
    fn pending(&self, agent: &str) -> PendingGuard<'_> {
        *self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(agent.to_string())
            .or_default() += 1;
        PendingGuard(self, agent.to_string())
    }
}

struct PendingGuard<'a>(&'a IntegrationState, String);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = pending.get_mut(&self.1) {
            *n -= 1;
            if *n == 0 {
                pending.remove(&self.1);
            }
        }
    }
}

// The installed binary, which the daemon step has just brought up to date;
// CANVAS_BIN points at a stand-in instead (how the failing-install case is
// exercised without breaking a real agent).
fn canvas_bin() -> PathBuf {
    cfg!(debug_assertions)
        .then(|| std::env::var_os("CANVAS_BIN"))
        .flatten()
        .map(PathBuf::from)
        .unwrap_or_else(crate::daemon::installed_path)
}

// An app opened from Finder or launchd inherits `/usr/bin:/bin:/usr/sbin:/sbin`,
// which has neither `claude` (~/.local/bin) nor `codex` (Homebrew), so the
// CLI's PATH lookup would report every agent as not found.
fn search_path() -> OsString {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let extra = [
        home.as_ref().map(|h| h.join(".local/bin")),
        Some(PathBuf::from("/opt/homebrew/bin")),
        Some(PathBuf::from("/usr/local/bin")),
    ];
    for dir in extra.into_iter().flatten() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

// The last non-empty stderr line: `canvas integrations` prints exactly one
// line on failure.
fn last_line(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

// Runs `bin` for at most `timeout`. A call that outlives it is killed along
// with its process group (`canvas integrations install` spawns `claude` and
// `codex`), and the error is one line, since it lands in `IntegrationState`.
fn run_bin(bin: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(bin)
        .args(args)
        .env("PATH", search_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("couldn't run {}: {e}", bin.display()))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                kill_group(&mut child);
                return Err(format!(
                    "`canvas {}` didn't finish within {}s and was stopped",
                    args.join(" "),
                    timeout.as_secs()
                ));
            }
            Err(e) => {
                kill_group(&mut child);
                return Err(format!("couldn't wait for {}: {e}", bin.display()));
            }
        }
    };
    // The child has exited, so both pipes close once any grandchild that
    // inherited them does too; a reader that never finishes is not waited on.
    let stdout = stdout.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    let stderr = stderr.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    } else {
        Err(last_line(&stderr).unwrap_or_else(|| format!("canvas exited with {status}")))
    }
}

// Reads a pipe to its end on its own thread, so a full pipe can't stall the
// child while the caller polls for its exit.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

fn kill_group(child: &mut Child) {
    let group = format!("-{}", child.id());
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &group])
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

fn list() -> Result<Vec<IntegrationRow>, String> {
    let json = run_bin(
        &canvas_bin(),
        &["integrations", "list", "--json"],
        LIST_TIMEOUT,
    )?;
    serde_json::from_str(&json)
        .map_err(|e| format!("couldn't read `canvas integrations list`: {e}"))
}

// Every agent's live state, with a failed install's stored error laid over
// whatever `list` says.
pub fn status(state: &IntegrationState) -> Result<Vec<IntegrationRow>, String> {
    let mut rows = list()?;
    let errors = state.errors.lock().unwrap();
    let pending = state.pending.lock().unwrap();
    for row in &mut rows {
        row.installing = pending.contains_key(&row.agent);
        if let Some(e) = errors.get(&row.agent) {
            row.error = Some(e.clone());
        }
    }
    Ok(rows)
}

pub fn install(state: &IntegrationState, agent: &str) -> Result<(), String> {
    if !AGENTS.contains(&agent) {
        return Err(format!("unknown agent {agent}"));
    }
    install_with(&canvas_bin(), state, agent)
}

fn install_with(bin: &Path, state: &IntegrationState, agent: &str) -> Result<(), String> {
    let _pending = state.pending(agent);
    let lock = state.install_lock(agent);
    let _installing = lock.lock().unwrap_or_else(|e| e.into_inner());
    let result = run_bin(bin, &["integrations", "install", agent], INSTALL_TIMEOUT).map(|_| ());
    let mut errors = state.errors.lock().unwrap();
    match &result {
        Ok(()) => {
            canvas_core::log::info("integration installed", &[("agent", &agent)]);
            errors.remove(agent);
        }
        Err(e) => {
            canvas_core::log::error("integration install failed", &[("agent", &agent), ("error", e)]);
            errors.insert(agent.to_string(), e.clone());
        }
    }
    result
}

// Launch-time pass: install for each detected agent that is out of date or
// not installed. A failure lands in `state` for the tab to show.
pub fn install_outdated(app: &tauri::AppHandle) {
    install_outdated_in(&app.state::<IntegrationState>());
}

fn install_outdated_in(state: &IntegrationState) {
    let rows = match list() {
        Ok(rows) => rows,
        Err(e) => {
            canvas_core::log::error("integration sync: list failed", &[("error", &e)]);
            return;
        }
    };
    for row in rows {
        let stale = matches!(row.status.as_deref(), Some("out of date" | "not installed"));
        if row.found && stale {
            let _ = install(state, &row.agent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_line_takes_the_final_non_empty_line() {
        assert_eq!(
            last_line(b"warning\nerror: cannot write hooks.json\n\n").as_deref(),
            Some("error: cannot write hooks.json")
        );
        assert_eq!(last_line(b"  \n"), None);
    }

    // A stand-in `canvas`: `list` reports codex out of date, and `install`
    // fails with a stderr line, until the test creates a `fixed` file beside it.
    #[test]
    fn a_failed_install_keeps_its_error_until_the_next_success() {
        let dir =
            std::env::temp_dir().join(format!("canvas-integrations-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("canvas");
        std::fs::write(
            &stub,
            format!(
                r#"#!/bin/sh
fixed="{dir}/fixed"
if [ "$2" = list ]; then
  if [ -e "$fixed" ]; then s='"current"'; else s='"out of date"'; fi
  echo "[{{\"agent\":\"claude-code\",\"found\":true,\"status\":\"current\",\"error\":null}},{{\"agent\":\"codex\",\"found\":true,\"status\":$s,\"error\":null}}]"
elif [ -e "$fixed" ]; then
  exit 0
else
  echo "noise" >&2
  echo "error: cannot write hooks.json: Permission denied" >&2
  exit 1
fi
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("CANVAS_BIN", &stub);

        let state = IntegrationState::default();
        install_outdated_in(&state);
        assert!(state.pending.lock().unwrap().is_empty());
        let rows = status(&state).unwrap();
        let codex = rows.iter().find(|r| r.agent == "codex").unwrap();
        assert_eq!(
            codex.error.as_deref(),
            Some("error: cannot write hooks.json: Permission denied")
        );
        assert!(rows
            .iter()
            .find(|r| r.agent == "claude-code")
            .unwrap()
            .error
            .is_none());

        std::fs::write(dir.join("fixed"), "").unwrap();
        install(&state, "codex").unwrap();
        let rows = status(&state).unwrap();
        let codex = rows.iter().find(|r| r.agent == "codex").unwrap();
        assert_eq!(
            (codex.error.as_deref(), codex.status.as_deref()),
            (None, Some("current"))
        );

        std::env::remove_var("CANVAS_BIN");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A stand-in that sleeps past the limit and leaves a grandchild holding
    // its pipes: the call returns a one-line error at the limit, not after the sleep.
    #[test]
    fn a_hung_call_is_killed_at_the_timeout() {
        let dir = std::env::temp_dir().join(format!("canvas-hang-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("canvas");
        std::fs::write(&stub, "#!/bin/sh\nsleep 30 &\nsleep 30\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let started = Instant::now();
        let err = run_bin(
            &stub,
            &["integrations", "install", "codex"],
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            err,
            "`canvas integrations install codex` didn't finish within 1s and was stopped"
        );
        assert!(!err.contains('\n'));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A stand-in whose `install` records when it starts and ends, one line per
    // event, tagged with the agent. Two installs for one agent never interleave
    // (start,end,start,end); two agents' installs do.
    #[test]
    fn installs_for_one_agent_run_one_after_the_other() {
        let dir = std::env::temp_dir().join(format!("canvas-lock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("canvas");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\necho \"start $3\" >> {log}\nsleep 0.5\necho \"end $3\" >> {log}\n",
                log = dir.join("log").display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let state = IntegrationState::default();
        std::thread::scope(|s| {
            for agent in ["codex", "codex", "claude-code"] {
                let (stub, state) = (&stub, &state);
                s.spawn(move || install_with(stub, state, agent).unwrap());
            }
        });

        assert!(state.pending.lock().unwrap().is_empty());
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        let codex: Vec<&str> = log.lines().filter(|l| l.ends_with("codex")).collect();
        assert_eq!(
            codex,
            ["start codex", "end codex", "start codex", "end codex"]
        );
        let first_end = log.lines().position(|l| l == "end codex").unwrap();
        let claude_start = log.lines().position(|l| l == "start claude-code").unwrap();
        assert!(claude_start < first_end, "different agents should overlap");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // `status` marks an agent installing from when an install is requested, even
    // while it waits on the lock, until it finishes.
    #[test]
    fn an_agent_is_pending_from_request_to_finish() {
        let dir = std::env::temp_dir().join(format!("canvas-pending-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("canvas");
        std::fs::write(&stub, "#!/bin/sh\nsleep 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let state = IntegrationState::default();
        std::thread::scope(|s| {
            for _ in 0..2 {
                let (stub, state) = (&stub, &state);
                s.spawn(move || install_with(stub, state, "codex").unwrap());
            }
            std::thread::sleep(Duration::from_millis(200));
            // One install is running and the other waits on its lock.
            let pending = state.pending.lock().unwrap().get("codex").copied();
            assert_eq!(pending, Some(2));
        });
        assert!(state.pending.lock().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_refuses_an_agent_outside_the_fixed_list() {
        let state = IntegrationState::default();
        assert_eq!(
            install(&state, "rm -rf").unwrap_err(),
            "unknown agent rm -rf"
        );
        assert!(state.errors.lock().unwrap().is_empty());
    }
}
