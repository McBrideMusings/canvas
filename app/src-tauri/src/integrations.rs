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
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

use tauri::Manager;

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
}

// Agent name -> the stderr line of its last failed install.
pub struct IntegrationState(pub Mutex<HashMap<String, String>>);

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

fn run(args: &[&str]) -> Result<String, String> {
    let bin = canvas_bin();
    let out = Command::new(&bin)
        .args(args)
        .env("PATH", search_path())
        .output()
        .map_err(|e| format!("couldn't run {}: {e}", bin.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(last_line(&out.stderr).unwrap_or_else(|| format!("canvas exited with {}", out.status)))
    }
}

fn list() -> Result<Vec<IntegrationRow>, String> {
    let json = run(&["integrations", "list", "--json"])?;
    serde_json::from_str(&json)
        .map_err(|e| format!("couldn't read `canvas integrations list`: {e}"))
}

// Every agent's live state, with a failed install's stored error laid over
// whatever `list` says.
pub fn status(state: &IntegrationState) -> Result<Vec<IntegrationRow>, String> {
    let mut rows = list()?;
    let errors = state.0.lock().unwrap();
    for row in &mut rows {
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
    let result = run(&["integrations", "install", agent]).map(|_| ());
    let mut errors = state.0.lock().unwrap();
    match &result {
        Ok(()) => {
            errors.remove(agent);
        }
        Err(e) => {
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
    let Ok(rows) = list() else { return };
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

        let state = IntegrationState(Mutex::new(HashMap::new()));
        install_outdated_in(&state);
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

    #[test]
    fn install_refuses_an_agent_outside_the_fixed_list() {
        let state = IntegrationState(Mutex::new(HashMap::new()));
        assert_eq!(
            install(&state, "rm -rf").unwrap_err(),
            "unknown agent rm -rf"
        );
        assert!(state.0.lock().unwrap().is_empty());
    }
}
