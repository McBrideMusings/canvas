use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder, MenuItemKind};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;

mod bridge;
mod daemon;

const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");

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
// main window's always-on-top state; see capabilities/viewer.json for the
// window and origin scoping that lets the canvas:// page reach them.
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

// Builds the Settings window on canvasd's own /settings.html, or brings the
// one already open to the front. Closing it only hides it (on_window_event
// below), so a later call shows the same window again. The gear in the viewer
// and the app menu's ⌘, item both come here.
fn show_settings_window(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("settings") {
        window.show().map_err(|e| e.to_string())?;
        let _ = window.unminimize();
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let url: tauri::Url = "canvas://localhost/settings.html"
        .parse()
        .map_err(|e: <tauri::Url as std::str::FromStr>::Err| e.to_string())?;
    WebviewWindowBuilder::new(app, "settings", WebviewUrl::External(url))
        .title("Settings")
        .inner_size(640.0, 640.0)
        .min_inner_size(480.0, 360.0)
        .resizable(true)
        .minimizable(false)
        .maximizable(false)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// canvasd's page invokes this from the gear; see capabilities/viewer.json.
// Async so the window is built off the main thread's command dispatch.
#[tauri::command]
async fn open_settings(app: tauri::AppHandle) -> Result<(), String> {
    show_settings_window(&app)
}

// settings.js polls this to render the General tab's Daemon status. Reruns
// the live checks fresh each call and carries forward whatever error setup()
// hit installing the daemon, if any — see daemon::query_status.
#[tauri::command]
fn daemon_status(app: tauri::AppHandle, state: tauri::State<daemon::DaemonState>) -> daemon::DaemonStatus {
    let stored_error = state.0.lock().unwrap().clone();
    daemon::query_status(&app, stored_error)
}

// The card id a `canvas-post://<card id>` link named, waiting for the viewer to
// take it. A link that launches the app arrives before the page has loaded
// and started listening, so the id is held here rather than only emitted.
struct PendingCard(std::sync::Mutex<Option<String>>);

// The viewer calls this at startup and again on every `canvas-open-card`
// event; it returns the id once and clears it.
#[tauri::command]
fn take_pending_card(state: tauri::State<PendingCard>) -> Option<String> {
    state.0.lock().unwrap().take()
}

// Handles the URLs the OS handed the app (the `canvas-post` scheme in
// tauri.conf.json): shows the main window and tells the viewer which card to
// bring up.
fn open_card_links(app: &tauri::AppHandle, urls: &[tauri::Url]) {
    let Some(id) = urls
        .iter()
        .find(|u| u.scheme() == "canvas-post")
        .and_then(|u| u.host_str())
    else {
        return;
    };
    *app.state::<PendingCard>().0.lock().unwrap() = Some(id.to_string());
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let _ = app.emit("canvas-open-card", ());
}

// The default macOS menu with "Settings…" (⌘,) added under the app menu's
// About item.
fn build_app_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    let settings_item = MenuItemBuilder::with_id("settings", "Settings…")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        app_menu.insert(&settings_item, 1)?;
    }
    Ok(menu)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_deep_link::init())
        .register_asynchronous_uri_scheme_protocol("canvas", |_ctx, request, responder| {
            // The socket read blocks, so each request gets its own thread.
            std::thread::spawn(move || responder.respond(bridge::proxy(request)));
        })
        .invoke_handler(tauri::generate_handler![
            set_pinned,
            get_pinned,
            open_settings,
            daemon_status,
            take_pending_card
        ])
        .setup(|app| {
            // Debug builds aren't bundled (no Resources dir to install from);
            // `admin dev canvas` runs the daemon separately in that workflow.
            let install_error = if !cfg!(debug_assertions) {
                daemon::ensure_daemon(app.handle()).error
            } else {
                None
            };
            let handle = app.handle().clone();
            std::thread::spawn(move || bridge::forward_events(handle));
            app.manage(daemon::DaemonState(std::sync::Mutex::new(install_error)));
            app.manage(PendingCard(std::sync::Mutex::new(None)));

            let link_handle = app.handle().clone();
            app.deep_link()
                .on_open_url(move |event| open_card_links(&link_handle, &event.urls()));
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                open_card_links(app.handle(), &urls);
            }

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

            app.set_menu(build_app_menu(app.handle())?)?;

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
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "settings" {
                let _ = show_settings_window(app);
            }
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
