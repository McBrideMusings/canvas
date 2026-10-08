// Makes Canvas.app a self-contained install: on launch, copies the canvas
// binary bundled into the app's Resources dir out to ~/.local/bin and
// registers it as a launchd agent, so the daemon survives quitting the app
// (canvas post needs somewhere to land at any time, app open or not) without
// requiring a separate `admin deploy` step. The install is
// `canvas_core::service::install`, the same one `canvas daemon install` runs,
// so it touches disk or launchd only when the bundled binary or the plist
// differs from what's installed, and an ordinary relaunch, or one right after
// a deploy, never restarts the daemon and empties its in-memory stream.

use canvas_core::service;

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

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .expect("HOME must be set")
}

pub fn installed_path() -> std::path::PathBuf {
    service::installed_path(&dirs_home())
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
        Err(e) => {
            return failed(
                &installed,
                format!("couldn't resolve app resource dir: {e}"),
            )
        }
    };
    let bundled = resource_dir.join("canvas");
    match service::install(&bundled, &dirs_home()) {
        Ok(outcome) => {
            if outcome.action != service::Action::Unchanged {
                canvas_core::log::info(
                    "installed bundled daemon",
                    &[
                        ("from", &bundled.display()),
                        ("binary_changed", &outcome.binary_changed),
                        ("plist_changed", &outcome.plist_changed),
                        ("action", &outcome.action.as_str()),
                    ],
                );
            }
            query_status(app, None)
        }
        Err(e) => failed(&installed, format!("daemon install failed: {e}")),
    }
}

fn failed(installed: &std::path::Path, error: String) -> DaemonStatus {
    log::error!("canvas daemon install: {error}");
    let gui = service::gui_target().ok();
    let (loaded, running) = gui.as_deref().map(service::state).unwrap_or((false, false));
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

    let (loaded, running) = service::gui_target()
        .ok()
        .map(|gui| service::state(&gui))
        .unwrap_or((false, false));

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
