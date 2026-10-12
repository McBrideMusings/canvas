//! Debug builds only: a way for a program to drive the dev app without
//! clicking it, which would take focus. `CANVAS_DEBUG_DIR` names a folder
//! (`scripts/verify-app.sh` sets it); nothing here runs without it.
//!
//! - `eval/*.js`: each file is run in the main window once, in name order,
//!   then deleted; a name ending `.settings.js` runs in the Settings window
//!   instead, which then opens without taking focus.
//! - `CANVAS_DEBUG_WINDOW=x,y`: moves the main window to that point (logical
//!   pixels, screen coordinates) at start, without activating it, so a recording
//!   can put it on a Retina display.
//! - `save-path`: while it exists, an export writes to the path it holds
//!   instead of showing the save dialog; an empty file answers "cancelled".
//! - `escape`: once it exists it is deleted and a bare Escape key-down is sent
//!   through `NSApplication.sendEvent` to the main window, the path a key
//!   press takes, so the app's key monitor and the focused page both see it.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, LogicalPosition, Manager};

fn dir() -> Option<PathBuf> {
    std::env::var_os("CANVAS_DEBUG_DIR").map(PathBuf::from)
}

/// Whether a program is driving this app through `CANVAS_DEBUG_DIR`.
pub fn active() -> bool {
    dir().is_some()
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
            let label = if file.to_string_lossy().ends_with(".settings.js") {
                "settings"
            } else {
                "main"
            };
            let result = match (script, app.get_webview_window(label)) {
                (Ok(script), Some(window)) => window.eval(&script).map_err(|e| e.to_string()),
                (Err(e), _) => Err(e.to_string()),
                (_, None) => Err(format!("no {label} window")),
            };
            match result {
                Ok(()) => canvas_core::log::info("debug eval", &[("file", &file.display())]),
                Err(e) => canvas_core::log::warn(
                    "debug eval failed",
                    &[("file", &file.display()), ("error", &e)],
                ),
            }
        }
        let escape = queue.with_file_name("escape");
        if std::fs::remove_file(&escape).is_ok() {
            send_escape(&app);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Sends the main window a bare Escape key-down on the main thread.
#[cfg(target_os = "macos")]
fn send_escape(app: &AppHandle) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
    use objc2_foundation::{NSPoint, NSString};

    let Some(Ok(ptr)) = app.get_webview_window("main").map(|w| w.ns_window()) else {
        canvas_core::log::warn("debug escape failed", &[("error", &"no main window")]);
        return;
    };
    let ptr = ptr as usize;
    let result = app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // SAFETY: the main window's NSWindow lives as long as the app.
        let window = unsafe { &*(ptr as *const NSWindow) };
        let escape = NSString::from_str("\u{1b}");
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags(0),
            0.0,
            window.windowNumber(),
            None,
            &escape,
            &escape,
            false,
            53,
        );
        match event {
            Some(event) => {
                NSApplication::sharedApplication(mtm).sendEvent(&event);
                canvas_core::log::info("debug escape sent", &[("window", &window.windowNumber())]);
            }
            None => canvas_core::log::warn("debug escape failed", &[("error", &"AppKit made no event")]),
        }
    });
    if let Err(e) = result {
        canvas_core::log::warn("debug escape failed", &[("error", &e)]);
    }
}

#[cfg(not(target_os = "macos"))]
fn send_escape(_app: &AppHandle) {}
