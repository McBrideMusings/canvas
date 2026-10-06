//! Debug builds only: a way for a program to drive the dev app without
//! clicking it, which would take focus. `CANVAS_DEBUG_DIR` names a folder
//! (`scripts/verify-app.sh` sets it); nothing here runs without it.
//!
//! - `eval/*.js`: each file is run in the main window once, in name order,
//!   then deleted.
//! - `CANVAS_DEBUG_WINDOW=x,y`: moves the main window to that point (logical
//!   pixels, screen coordinates) at start, without activating it, so a recording
//!   can put it on a Retina display.
//! - `save-path`: while it exists, an export writes to the path it holds
//!   instead of showing the save dialog; an empty file answers "cancelled".

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, LogicalPosition, Manager};

fn dir() -> Option<PathBuf> {
    std::env::var_os("CANVAS_DEBUG_DIR").map(PathBuf::from)
}

/// `None` when no `save-path` file stands in for the dialog; otherwise the
/// dialog's answer.
pub fn save_path() -> Option<Option<PathBuf>> {
    let text = std::fs::read_to_string(dir()?.join("save-path")).ok()?;
    let path = text.trim();
    Some((!path.is_empty()).then(|| PathBuf::from(path)))
}

/// Moves the main window to `CANVAS_DEBUG_WINDOW` (`x,y`) when it is set.
pub fn place_window(app: &AppHandle) {
    let Ok(spec) = std::env::var("CANVAS_DEBUG_WINDOW") else {
        return;
    };
    let point = spec
        .split_once(',')
        .and_then(|(x, y)| Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?)));
    let (Some((x, y)), Some(window)) = (point, app.get_webview_window("main")) else {
        canvas_core::log::warn("debug window placement ignored", &[("spec", &spec)]);
        return;
    };
    match window.set_position(LogicalPosition::new(x, y)) {
        Ok(()) => canvas_core::log::info("debug window placed", &[("x", &x), ("y", &y)]),
        Err(e) => canvas_core::log::warn("debug window placement failed", &[("error", &e)]),
    }
}

/// Runs forever on its own thread, polling `eval/` five times a second.
pub fn run_eval_queue(app: AppHandle) {
    let Some(queue) = dir().map(|d| d.join("eval")) else {
        return;
    };
    canvas_core::log::info("debug eval queue", &[("dir", &queue.display())]);
    loop {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&queue)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
            .collect();
        files.sort();
        for file in files {
            let script = std::fs::read_to_string(&file);
            let _ = std::fs::remove_file(&file);
            let result = match (script, app.get_webview_window("main")) {
                (Ok(script), Some(window)) => window.eval(&script).map_err(|e| e.to_string()),
                (Err(e), _) => Err(e.to_string()),
                (_, None) => Err("no main window".to_string()),
            };
            match result {
                Ok(()) => canvas_core::log::info("debug eval", &[("file", &file.display())]),
                Err(e) => canvas_core::log::warn(
                    "debug eval failed",
                    &[("file", &file.display()), ("error", &e)],
                ),
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
