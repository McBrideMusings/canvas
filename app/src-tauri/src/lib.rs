use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");

// Same env var the CLI already reads (cli/src/client.rs) to point at a
// non-default canvasd. The main window's waiting page (app/dist/index.html)
// invokes this command to learn where to poll and, once canvasd answers,
// where to navigate itself.
#[tauri::command]
fn canvas_url() -> String {
    std::env::var("CANVAS_URL").unwrap_or_else(|_| "http://127.0.0.1:8229".to_string())
}

// Where the pin state file lives: a single byte, "1" pinned or "0" unpinned,
// in the app's data dir. Plain text rather than serde_json — one bool isn't
// worth a new dependency or a parser.
fn pinned_state_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("pinned"))
}

fn read_pinned_state(app: &tauri::AppHandle) -> bool {
    let Ok(path) = pinned_state_path(app) else {
        return false;
    };
    std::fs::read_to_string(path)
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

fn write_pinned_state(app: &tauri::AppHandle, pinned: bool) -> Result<(), String> {
    let path = pinned_state_path(app)?;
    std::fs::write(path, if pinned { "1" } else { "0" }).map_err(|e| e.to_string())
}

// canvasd's own page (viewer/app.js) invokes these two to toggle and read the
// main window's always-on-top state; see capabilities/remote-pin.json for the
// origin restriction that lets a remote page reach them at all.
#[tauri::command]
fn set_pinned(app: tauri::AppHandle, pinned: bool) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.set_always_on_top(pinned).map_err(|e| e.to_string())?;
    }
    write_pinned_state(&app, pinned)
}

#[tauri::command]
fn get_pinned(app: tauri::AppHandle) -> bool {
    read_pinned_state(&app)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![canvas_url, set_pinned, get_pinned])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Applies whatever pin state was saved from a previous run before
            // the window is ever shown, so a pinned window stays on top of
            // others from the first frame after relaunch.
            if let Some(window) = app.get_webview_window("main") {
                if read_pinned_state(&app.handle()) {
                    let _ = window.set_always_on_top(true);
                }
            }

            let show_item = MenuItemBuilder::with_id("show", "Show/Hide Canvas").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let tray_menu = MenuBuilder::new(app).items(&[&show_item, &quit_item]).build()?;

            let icon = Image::from_bytes(TRAY_ICON)?;

            TrayIconBuilder::new()
                .icon(icon)
                .icon_as_template(true)
                .menu(&tray_menu)
                // A left click already pops this menu (below); a separate
                // on_tray_icon_event click handler would double-fire — macOS
                // sends one TrayIconEvent::Click per mouseDown *and* one per
                // mouseUp, so a toggle wired there fires twice per click and
                // collapses to a no-op. The menu item is the one toggle path.
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => toggle_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it instead of quitting the app or
            // dropping state; canvasd holds all stream state, so the window
            // can be shown again with nothing lost.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        match window.is_visible() {
            Ok(true) => {
                let _ = window.hide();
            }
            _ => {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    }
}
