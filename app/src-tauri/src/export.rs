//! The card menu's "Export post…". canvasd's `GET /api/cards/:id/export`
//! builds the page; this only fetches it over the socket, asks where to save
//! it and writes it there.

use std::path::PathBuf;
use std::time::Duration;

use tauri_plugin_dialog::DialogExt;

/// The route's CDN downloads (bounded by `EXPORT_DOWNLOAD_SECS`) plus reading
/// and encoding the card's images, as the CLI waits.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(canvas_core::EXPORT_DOWNLOAD_SECS + 15);

/// Returns the written path, `None` when the dialog was cancelled, or a
/// one-line reason.
#[tauri::command]
pub async fn export_card(app: tauri::AppHandle, card_id: String) -> Result<Option<String>, String> {
    // It goes into a request path, so nothing but a card id's characters.
    if card_id.is_empty()
        || !card_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("not a post id".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        save(&app, &card_id).inspect_err(|error| {
            canvas_core::log::warn(
                "post export failed",
                &[("card", &card_id), ("error", error)],
            );
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn save(app: &tauri::AppHandle, card_id: &str) -> Result<Option<String>, String> {
    let page = fetch(card_id)?;
    let Some(path) = choose_path(app, card_id)? else {
        canvas_core::log::info("post export cancelled", &[("card", &card_id)]);
        return Ok(None);
    };
    std::fs::write(&path, &page.html)
        .map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
    let path = path.display().to_string();
    canvas_core::log::info(
        "post exported",
        &[
            ("card", &card_id),
            ("path", &path),
            ("bytes", &page.html.len()),
            ("warnings", &page.warnings.len()),
        ],
    );
    Ok(Some(path))
}

fn fetch(card_id: &str) -> Result<canvas_core::ExportResult, String> {
    let socket = canvas_core::paths::socket_path().ok_or("no HOME to place the socket")?;
    let response = canvas_core::unix_http::request(
        &socket,
        "GET",
        &format!("/api/cards/{card_id}/export"),
        &[],
        &[],
        Some(EXPORT_TIMEOUT),
    )
    .map_err(|e| format!("canvasd unreachable: {e}"))?;
    match response.status {
        200..=299 => serde_json::from_slice(&response.body)
            .map_err(|e| format!("canvasd sent an unreadable export: {e}")),
        404 => Err("canvasd has no post with that id".to_string()),
        status => Err(canvas_core::log::error_text(&response.body)
            .unwrap_or_else(|| format!("canvasd returned HTTP {status}"))),
    }
}

/// The save dialog, or in a debug build the path `debug::save_path` supplies.
fn choose_path(app: &tauri::AppHandle, card_id: &str) -> Result<Option<PathBuf>, String> {
    #[cfg(debug_assertions)]
    if let Some(answer) = crate::debug::save_path() {
        return Ok(answer);
    }
    let Some(chosen) = app
        .dialog()
        .file()
        .set_file_name(format!("{card_id}.html"))
        .add_filter("HTML page", &["html"])
        .blocking_save_file()
    else {
        return Ok(None);
    };
    chosen.into_path().map(Some).map_err(|e| e.to_string())
}
