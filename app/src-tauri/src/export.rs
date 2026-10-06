//! The card menu's "Export post…". canvasd's `GET /api/cards/:id/export`
//! builds the page; this only fetches it (`canvas_core::export::fetch`), asks where to save
//! it and writes it there.

use std::path::PathBuf;

use canvas_core::export::{self, Exported};
use canvas_core::ExportWarning;
use serde::Serialize;
use tauri_plugin_dialog::DialogExt;

/// A written page: where it went, and what the export left out of it.
#[derive(Serialize)]
pub struct Saved {
    path: String,
    warnings: Vec<ExportWarning>,
}

/// Returns the written page, `None` when the dialog was cancelled, or a
/// one-line reason.
#[tauri::command]
pub async fn export_card(app: tauri::AppHandle, card_id: String) -> Result<Option<Saved>, String> {
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

fn save(app: &tauri::AppHandle, card_id: &str) -> Result<Option<Saved>, String> {
    let page = match export::fetch(card_id)? {
        Exported::Page(page) => page,
        Exported::Gone => return Err("canvasd has no post with that id".to_string()),
        Exported::Failed(error) => return Err(error),
    };
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
    Ok(Some(Saved {
        path,
        warnings: page.warnings,
    }))
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
