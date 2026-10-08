//! Fetching one card's export from canvasd (`GET /api/cards/:id/export`),
//! for `canvas export` and Canvas.app's "Export post…" alike.

use std::time::Duration;

use crate::{unix_http, ExportResult, EXPORT_DOWNLOAD_SECS};

/// The route's CDN downloads (bounded by `EXPORT_DOWNLOAD_SECS`) plus reading
/// and encoding the card's images.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(EXPORT_DOWNLOAD_SECS + 15);

/// What canvasd answered for one card's export.
pub enum Exported {
    Page(ExportResult),
    /// canvasd holds no card with that id.
    Gone,
    /// canvasd answered with this error.
    Failed(String),
}

/// The card as a standalone page, plus its warnings. `Err` only when canvasd
/// can't be reached or its answer can't be read.
pub fn fetch(card_id: &str) -> Result<Exported, String> {
    let path = format!("/api/cards/{}/export", unix_http::percent_encode(card_id));
    let response = unix_http::call("GET", &path, &[], &[], EXPORT_TIMEOUT)
        .map_err(|e| unix_http::failure_text(&e))?;
    match response.status {
        200..=299 => serde_json::from_slice(&response.body)
            .map(Exported::Page)
            .map_err(|e| format!("canvasd returned malformed JSON: {e}")),
        404 => Ok(Exported::Gone),
        status => Ok(Exported::Failed(
            crate::log::error_text(&response.body)
                .unwrap_or_else(|| format!("canvasd returned HTTP {status}")),
        )),
    }
}
