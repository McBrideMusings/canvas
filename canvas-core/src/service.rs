//! The canvasd launchd agent: one renderer and one installer for its plist and
//! binary, shared by `canvas daemon install` (what `admin deploy` runs) and
//! Canvas.app's setup, so the two can never disagree about the agent and
//! restart the daemon over each other.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const LABEL: &str = "com.piercemakes.canvasd";

/// `~/.local/bin/canvas`, where launchd runs the daemon from.
pub fn installed_path(home: &Path) -> PathBuf {
    home.join(".local/bin/canvas")
}

pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

// No ProcessType key: launchd's default is Standard. `Background` would run
// canvasd, and every artifact refresh command it spawns, at darwin background
// priority with throttled disk I/O.
pub fn render_plist(canvas_path: &Path, home: &Path) -> String {
    let log_path = home.join("Library/Logs/canvasd.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{canvas}</string>
		<string>daemon</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>WorkingDirectory</key>
	<string>{home}</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        canvas = canvas_path.display(),
        home = home.display(),
        log = log_path.display(),
    )
}

/// What [`install`] did to launchd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Binary and plist already matched and canvasd was running.
    Unchanged,
    /// The plist matched and the agent was loaded: `kickstart -k` started the
    /// new binary, or a stopped daemon, leaving the registration alone.
    Restarted,
    /// The plist changed or the agent wasn't loaded: bootout (when loaded)
    /// and bootstrap.
    Loaded,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Unchanged => "unchanged",
            Action::Restarted => "restarted",
            Action::Loaded => "loaded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub binary_changed: bool,
    pub plist_changed: bool,
    pub action: Action,
}

/// Installs `source` as the daemon binary and writes the agent's plist, then
/// restarts the daemon only when one of them changed. Every restart empties
/// canvasd's in-memory stream, so an install where both already match
/// touches nothing.
pub fn install(source: &Path, home: &Path) -> Result<Outcome, String> {
    let installed = installed_path(home);
    let plist = plist_path(home);
    let desired = render_plist(&installed, home);
    let gui = gui_target()?;

    let source_bytes =
        std::fs::read(source).map_err(|e| format!("couldn't read {}: {e}", source.display()))?;
    let binary_changed = std::fs::read(&installed).ok().as_deref() != Some(source_bytes.as_slice());
    let plist_changed = std::fs::read_to_string(&plist).ok().as_deref() != Some(desired.as_str());
    let (loaded, running) = state(&gui);

    if !binary_changed && !plist_changed && running {
        return Ok(Outcome {
            binary_changed,
            plist_changed,
            action: Action::Unchanged,
        });
    }

    if binary_changed {
        replace_file(&installed, &source_bytes, 0o755)
            .map_err(|e| format!("couldn't install {}: {e}", installed.display()))?;
    }
    if plist_changed {
        replace_file(&plist, desired.as_bytes(), 0o644)
            .map_err(|e| format!("couldn't write {}: {e}", plist.display()))?;
    }

    // The files are already new, so the next install compares level: a restart
    // that fails here must not return while the old process keeps running, or
    // nothing would ever swap it. A failed kickstart falls through to a reload.
    if loaded
        && !plist_changed
        && launchctl(&["kickstart", "-k", &format!("{gui}/{LABEL}")]).is_ok()
    {
        return Ok(Outcome {
            binary_changed,
            plist_changed,
            action: Action::Restarted,
        });
    }
    reload(&gui, &plist, loaded)?;
    Ok(Outcome {
        binary_changed,
        plist_changed,
        action: Action::Loaded,
    })
}

/// Registers the plist afresh: bootout (when loaded), wait for the label to
/// go, bootstrap. A bootstrap that fails while the old registration lingers
/// gets a `kickstart -k` instead, and counts as done only if canvasd runs.
fn reload(gui: &str, plist: &Path, loaded: bool) -> Result<(), String> {
    if loaded {
        let _ = launchctl(&["bootout", &format!("{gui}/{LABEL}")]);
        wait_unloaded(gui);
    }
    let Err(e) = launchctl(&["bootstrap", gui, &plist.display().to_string()]) else {
        return Ok(());
    };
    let _ = launchctl(&["kickstart", "-k", &format!("{gui}/{LABEL}")]);
    if state(gui).1 {
        Ok(())
    } else {
        Err(e)
    }
}

/// Writes `bytes` to a sibling temp file and renames it over `path`. The
/// rename gives `path` a new inode, so a daemon still running the old binary
/// keeps its own mapped pages: writing through the live inode instead makes
/// macOS refuse to launch the new file (`OS_REASON_CODESIGNING`). The bytes
/// go in untouched, so a binary keeps its signature.
fn replace_file(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `gui/<uid>`, the launchd domain of the logged-in user.
pub fn gui_target() -> Result<String, String> {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("couldn't run id -u: {e}"))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || uid.is_empty() {
        return Err(format!("id -u gave no uid ({})", out.status));
    }
    Ok(format!("gui/{uid}"))
}

/// (loaded, running): `launchctl print` exits 0 iff the label is loaded at
/// all; "state = running" distinguishes loaded-but-stopped (crashed past
/// KeepAlive's retry budget) from actually up.
pub fn state(gui: &str) -> (bool, bool) {
    match std::process::Command::new("launchctl")
        .args(["print", &format!("{gui}/{LABEL}")])
        .output()
    {
        Ok(o) if o.status.success() => (
            true,
            String::from_utf8_lossy(&o.stdout).contains("state = running"),
        ),
        _ => (false, false),
    }
}

fn wait_unloaded(gui: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while state(gui).0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("couldn't run launchctl: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stderr.lines().next().unwrap_or("").trim();
    Err(format!(
        "launchctl {} exited with {}: {line}",
        args[0], out.status
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_the_daemon_at_standard_priority() {
        let plist = render_plist(Path::new("/h/.local/bin/canvas"), Path::new("/h"));
        assert!(!plist.contains("ProcessType"), "{plist}");
        assert!(plist.contains("<string>/h/.local/bin/canvas</string>"));
        assert!(plist.contains("<string>/h/Library/Logs/canvasd.log</string>"));
    }

    #[test]
    fn replace_file_swaps_in_a_new_inode_and_keeps_the_old_one_whole() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("canvas-service-{}", std::process::id()));
        let path = dir.join("bin/canvas");
        replace_file(&path, b"old", 0o755).unwrap();
        // A running daemon holds the old file open.
        let held = std::fs::File::open(&path).unwrap();
        let old_ino = std::fs::metadata(&path).unwrap().ino();

        replace_file(&path, b"new", 0o755).unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        assert_ne!(meta.ino(), old_ino);
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let mut old = String::new();
        std::io::Read::read_to_string(&mut &held, &mut old).unwrap();
        assert_eq!(old, "old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
