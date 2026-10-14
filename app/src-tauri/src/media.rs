//! A card's videos play from disk through Tauri's asset protocol rather than
//! the `canvas://` bridge, since WebKit's player reads a file in thousands of
//! small ranges and each one through the bridge is a socket round trip to
//! canvasd. The asset protocol serves only paths in its scope, which starts
//! empty: this module allows each video path canvasd holds on a card, from
//! `/api/state` when the event stream opens and from each `card-upserted`
//! event, before the viewer hears of the card. Tauri's scope can't drop a
//! path, so one stays readable until the app quits.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager};

const STATE_TIMEOUT: Duration = Duration::from_secs(10);

static ALLOWED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The paths among `images` that are videos: absolute, as canvasd's image
/// route requires, so a relative one never resolves against the app's cwd.
fn video_paths(images: &[serde_json::Value]) -> impl Iterator<Item = &str> {
    images.iter().filter_map(|v| v.as_str()).filter(|p| {
        p.starts_with('/')
            && Path::new(p)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| canvas_core::VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
    })
}

/// Allows one card's videos (the card as canvasd serialises it).
pub fn allow_card(app: &AppHandle, card: &serde_json::Value) {
    let Some(images) = card.get("images").and_then(|i| i.as_array()) else {
        return;
    };
    let mut allowed = ALLOWED.lock().unwrap_or_else(|e| e.into_inner());
    let allowed = allowed.get_or_insert_with(HashSet::new);
    for path in video_paths(images) {
        if allowed.contains(path) {
            continue;
        }
        match app.asset_protocol_scope().allow_file(path) {
            Ok(()) => {
                allowed.insert(path.to_string());
                canvas_core::log::info("media allowed", &[("path", &path)]);
            }
            Err(e) => {
                canvas_core::log::warn("media allow failed", &[("path", &path), ("error", &e)])
            }
        }
    }
}

/// Allows every video on every card canvasd holds; an error when the state
/// can't be read, so the caller never lets the viewer load cards whose
/// videos the scope would refuse.
pub fn allow_state(app: &AppHandle, socket: &Path) -> Result<(), String> {
    let response =
        canvas_core::unix_http::request(socket, "GET", "/api/state", &[], &[], Some(STATE_TIMEOUT))
            .map_err(|e| e.to_string())?;
    if response.status != 200 {
        return Err(format!("canvasd answered HTTP {}", response.status));
    }
    let state: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
    for card in state
        .get("cards")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
    {
        allow_card(app, card);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_video_paths_are_picked() {
        let images = serde_json::json!([
            "/a/x.png",
            "/a/y.MP4",
            "/a/z.webm",
            "/a/w.mov.txt",
            "rel/v.mov",
            3
        ]);
        let picked: Vec<&str> = video_paths(images.as_array().unwrap()).collect();
        assert_eq!(picked, vec!["/a/y.MP4", "/a/z.webm"]);
    }
}
