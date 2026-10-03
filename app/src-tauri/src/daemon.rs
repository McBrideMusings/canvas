// Makes Canvas.app a self-contained install: on launch, copies the canvas
// binary bundled into the app's Resources dir out to ~/.local/bin and
// registers it as a launchd agent, so the daemon survives quitting the app
// (canvas post needs somewhere to land at any time, app open or not) without
// requiring a separate `admin deploy canvas` step. Only touches disk or
// launchd when the bundled binary differs from what's already installed, so
// an ordinary relaunch never restarts the daemon and empties its in-memory
// stream.

const LABEL: &str = "com.piercemakes.canvasd";

// What the Settings window's Daemon tab reports (settings.js invokes
// `daemon_status`). `error` carries the reason install/start failed,
// if it did — the setup()-time install runs before any logger exists in a
// release build, so this struct is the only place that reason is visible.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonStatus {
    pub installed_path: String,
    pub installed: bool,
    pub up_to_date: bool,
    // A bundled binary exists and differs from the installed one: only a
    // relaunch (whose setup() runs ensure_daemon) brings them level. False
    // in a debug build, which bundles nothing, where up_to_date is always false.
    pub relaunch_needed: bool,
    pub loaded: bool,
    pub running: bool,
    pub error: Option<String>,
}

pub struct DaemonState(pub std::sync::Mutex<Option<String>>);

fn plist_path() -> std::path::PathBuf {
    dirs_home().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME").map(std::path::PathBuf::from).expect("HOME must be set")
}

pub fn installed_path() -> std::path::PathBuf {
    dirs_home().join(".local/bin/canvas")
}

fn gui_target() -> Result<String, String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| e.to_string())?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    Ok(format!("gui/{uid}"))
}

fn write_plist(canvas_path: &std::path::Path) -> Result<(), String> {
    let log_path = dirs_home().join("Library/Logs/canvasd.log");
    let contents = format!(
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
	<key>ProcessType</key>
	<string>Background</string>
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
        home = dirs_home().display(),
        log = log_path.display(),
    );
    let dir = plist_path();
    std::fs::create_dir_all(dir.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&dir, contents).map_err(|e| e.to_string())
}

fn service_loaded(gui: &str) -> bool {
    service_state(gui).0
}

// (loaded, running) — `launchctl print` exits 0 iff the label is loaded at
// all; "state = running" in its stdout distinguishes loaded-but-stopped
// (e.g. crashed past KeepAlive's retry budget) from actually up.
fn service_state(gui: &str) -> (bool, bool) {
    match std::process::Command::new("launchctl")
        .args(["print", &format!("{gui}/{LABEL}")])
        .output()
    {
        Ok(o) if o.status.success() => {
            let running = String::from_utf8_lossy(&o.stdout).contains("state = running");
            (true, running)
        }
        _ => (false, false),
    }
}

fn bootstrap(gui: &str) -> Result<(), String> {
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", gui, plist_path().to_str().unwrap()])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("launchctl bootstrap exited with {status}"))
    }
}

fn bootout(gui: &str) {
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{gui}/{LABEL}")])
        .status();
}

fn kickstart(gui: &str) {
    let _ = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &format!("{gui}/{LABEL}")])
        .status();
}

/// Installs or updates the daemon from the binary bundled into the app, and
/// makes sure it's running. Called once from setup(), before the main
/// window's waiting page starts polling canvasd. Returns the outcome so it
/// can be stashed in managed state — log::error! goes nowhere in a release
/// build (the log plugin is debug-only), so this is the only record of why
/// an install failed.
pub fn ensure_daemon(app: &tauri::AppHandle) -> DaemonStatus {
    let installed = installed_path();

    let resource_dir = match tauri::Manager::path(app).resource_dir() {
        Ok(dir) => dir,
        Err(e) => return failed(&installed, format!("couldn't resolve app resource dir: {e}")),
    };
    let bundled = resource_dir.join("canvas");
    let bundled_bytes = match std::fs::read(&bundled) {
        Ok(bytes) => bytes,
        Err(e) => {
            return failed(&installed, format!("no bundled daemon at {}: {e}", bundled.display()))
        }
    };

    let up_to_date = std::fs::read(&installed).ok().as_deref() == Some(bundled_bytes.as_slice());

    let gui = match gui_target() {
        Ok(gui) => gui,
        Err(e) => return failed(&installed, format!("couldn't determine launchd target: {e}")),
    };

    if up_to_date {
        if !service_loaded(&gui) {
            if let Err(e) = bootstrap(&gui) {
                return failed(&installed, format!("daemon installed but wouldn't start: {e}"));
            }
        }
        return query_status(app, None);
    }

    canvas_core::log::info(
        "installing bundled daemon",
        &[("from", &bundled.display()), ("to", &installed.display())],
    );
    if let Some(parent) = installed.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return failed(&installed, format!("couldn't create {}: {e}", parent.display()));
        }
    }
    if let Err(e) = std::fs::copy(&bundled, &installed) {
        return failed(&installed, format!("couldn't install daemon binary: {e}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)) {
            return failed(&installed, format!("couldn't make daemon binary executable: {e}"));
        }
    }

    if let Err(e) = write_plist(&installed) {
        return failed(&installed, format!("couldn't write launchd agent: {e}"));
    }

    if service_loaded(&gui) {
        bootout(&gui);
    }
    if let Err(e) = bootstrap(&gui) {
        kickstart(&gui);
        let (_, running) = service_state(&gui);
        if !running {
            return failed(&installed, format!("daemon installed but wouldn't start: {e}"));
        }
    }

    query_status(app, None)
}

fn failed(installed: &std::path::Path, error: String) -> DaemonStatus {
    log::error!("canvas daemon install: {error}");
    let gui = gui_target().ok();
    let (loaded, running) = gui.as_deref().map(service_state).unwrap_or((false, false));
    DaemonStatus {
        installed_path: installed.display().to_string(),
        installed: installed.exists(),
        up_to_date: false,
        relaunch_needed: false,
        loaded,
        running,
        error: Some(error),
    }
}

/// Live status with no side effects, for the Settings window to poll: does
/// the install exist, does it match what's bundled in this build of the app,
/// is launchd holding it loaded, is it actually running. `stored_error`
/// carries forward the reason from the setup()-time install attempt, if any
/// — a later successful poll (e.g. the user fixed a permissions problem and
/// reopened Settings) still shows it until the app relaunches, since this
/// function never re-attempts the install itself.
pub fn query_status(app: &tauri::AppHandle, stored_error: Option<String>) -> DaemonStatus {
    let installed = installed_path();
    let installed_bytes = std::fs::read(&installed).ok();

    let bundled_bytes = tauri::Manager::path(app)
        .resource_dir()
        .ok()
        .map(|dir| dir.join("canvas"))
        .and_then(|p| std::fs::read(p).ok());
    let up_to_date = bundled_bytes
        .as_deref()
        .is_some_and(|bundled| installed_bytes.as_deref() == Some(bundled));
    // A launch whose install failed would fail the same way again, so it offers
    // no relaunch (and the error is what the Daemon tab shows instead).
    let relaunch_needed = bundled_bytes.is_some() && !up_to_date && stored_error.is_none();

    let (loaded, running) = gui_target().ok().map(|gui| service_state(&gui)).unwrap_or((false, false));

    DaemonStatus {
        installed_path: installed.display().to_string(),
        installed: installed_bytes.is_some(),
        up_to_date,
        relaunch_needed,
        loaded,
        running,
        error: stored_error,
    }
}
