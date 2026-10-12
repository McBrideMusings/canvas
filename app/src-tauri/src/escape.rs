//! Escape out of an artifact's full window while the page has focus. The
//! artifact runs in a sandboxed iframe with an opaque origin, so once the
//! person clicks into it every key goes to the page and the viewer's own
//! keydown listener never hears Escape. While the viewer says full window is
//! on (`set_artifact_full`), an AppKit local monitor takes a bare Escape aimed
//! at the main window before the webview sees it and emits `artifact-escape`
//! to the viewer, which replays it as a keydown on its own focused element, so
//! its menu, lightbox and drawer still take Escape first.

use std::sync::atomic::{AtomicBool, Ordering};

static FULL: AtomicBool = AtomicBool::new(false);

/// The viewer calls this on every entry to and exit from full window.
#[tauri::command]
pub fn set_artifact_full(full: bool) {
    let was = FULL.swap(full, Ordering::SeqCst);
    if was != full {
        canvas_core::log::info("artifact full window", &[("full", &full)]);
    }
}

/// The macOS virtual key code for Escape.
#[cfg(target_os = "macos")]
const ESCAPE_KEY_CODE: u16 = 53;

/// Installs the monitor; called once from `setup()` on the main thread. The
/// monitor lives as long as the app.
#[cfg(target_os = "macos")]
pub fn install(app: &tauri::AppHandle) {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
    use tauri::{Emitter, Manager};

    let Some(mtm) = MainThreadMarker::new() else {
        canvas_core::log::warn(
            "escape monitor not installed",
            &[("reason", &"not on the main thread")],
        );
        return;
    };
    let main_window = match app.get_webview_window("main").map(|w| w.ns_window()) {
        Some(Ok(ptr)) => ptr as usize,
        Some(Err(e)) => {
            canvas_core::log::warn("escape monitor not installed", &[("reason", &e)]);
            return;
        }
        None => {
            canvas_core::log::warn(
                "escape monitor not installed",
                &[("reason", &"no main window")],
            );
            return;
        }
    };
    let app = app.clone();
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a valid event for the call.
        let ev = unsafe { event.as_ref() };
        let modifiers = ev.modifierFlags()
            & (NSEventModifierFlags::Command
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Control);
        let to_main = ev
            .window(mtm)
            .is_some_and(|w| objc2::rc::Retained::as_ptr(&w) as usize == main_window);
        if !FULL.load(Ordering::SeqCst)
            || ev.keyCode() != ESCAPE_KEY_CODE
            || !modifiers.is_empty()
            || !to_main
        {
            return event.as_ptr();
        }
        let sent = app.emit_to("main", "artifact-escape", ());
        canvas_core::log::info("artifact escape", &[("emitted", &sent.is_ok())]);
        std::ptr::null_mut()
    });
    // SAFETY: the block returns the event it was given or null.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler)
    };
    match monitor {
        Some(monitor) => {
            // AppKit keeps the monitor until removeMonitor, which nothing calls.
            std::mem::forget(monitor);
            canvas_core::log::info("escape monitor installed", &[]);
        }
        None => canvas_core::log::warn(
            "escape monitor not installed",
            &[("reason", &"AppKit returned none")],
        ),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn install(_app: &tauri::AppHandle) {}
