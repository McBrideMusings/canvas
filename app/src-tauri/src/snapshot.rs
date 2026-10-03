//! The app's half of `canvas snapshot`. canvasd sends the viewer a
//! `card-snapshot` event; the viewer scrolls the card into view and calls
//! `snapshot_reply` with the card's on-screen rect, or with why it can't show
//! the card. This captures that rect of the window's WKWebView, so the PNG is
//! the card exactly as rendered in the active theme, and posts the PNG (or the
//! reason) to canvasd's `/api/snapshots/:request`, which answers the waiting
//! CLI.

use std::time::Duration;

use serde::Deserialize;

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// A rect in the webview's CSS pixels, origin top left.
#[derive(Debug, Deserialize)]
pub struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// `request` is canvasd's id for the snapshot, a UUID; `clipped` says the card
/// ran past the window, so the PNG stops at its edge.
#[tauri::command]
pub async fn snapshot_reply(
    webview: tauri::Webview,
    request: String,
    rect: Option<Rect>,
    clipped: Option<bool>,
    error: Option<String>,
) -> Result<(), String> {
    // It goes into a request line, so nothing but a UUID's characters.
    if request.is_empty() || !request.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err("not a snapshot request id".to_string());
    }
    let answer = match (rect, error) {
        (_, Some(reason)) => Err(reason),
        (Some(rect), None) => capture(&webview, &rect).await,
        (None, None) => Err("the viewer sent neither a rect nor a reason".to_string()),
    };
    let (content_type, body) = match answer {
        Ok(png) => ("image/png", png),
        Err(reason) => {
            canvas_core::log::warn(
                "snapshot not captured",
                &[("request", &request), ("reason", &reason)],
            );
            (
                "application/json",
                serde_json::json!({ "error": reason })
                    .to_string()
                    .into_bytes(),
            )
        }
    };
    let clipped = clipped.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || upload(&request, content_type, clipped, &body))
        .await
        .map_err(|e| e.to_string())?
}

/// Sends the answer to canvasd.
fn upload(request: &str, content_type: &str, clipped: bool, body: &[u8]) -> Result<(), String> {
    let socket = canvas_core::paths::socket_path().ok_or("no HOME to place the socket")?;
    let path = format!("/api/snapshots/{request}");
    let mut headers = vec![("Content-Type", content_type)];
    if clipped {
        headers.push(("X-Canvas-Clipped", "true"));
    }
    let result = canvas_core::unix_http::request(
        &socket,
        "POST",
        &path,
        &headers,
        body,
        Some(UPLOAD_TIMEOUT),
    );
    match result {
        Ok(response) if response.status == 404 => {
            canvas_core::log::warn(
                "snapshot arrived after canvasd stopped waiting",
                &[("request", &request), ("bytes", &body.len())],
            );
            Ok(())
        }
        Ok(response) => {
            canvas_core::log::info(
                "snapshot sent",
                &[
                    ("request", &request),
                    ("type", &content_type),
                    ("bytes", &body.len()),
                    ("clipped", &clipped),
                    ("status", &response.status),
                ],
            );
            Ok(())
        }
        Err(e) => {
            canvas_core::log::warn(
                "snapshot upload failed",
                &[("request", &request), ("error", &e)],
            );
            Err(e.to_string())
        }
    }
}

/// Captures `rect` of the webview as a PNG at the screen's pixel density.
#[cfg(target_os = "macos")]
async fn capture(webview: &tauri::Webview, rect: &Rect) -> Result<Vec<u8>, String> {
    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_foundation::{NSDictionary, NSError};
    use objc2_web_kit::{WKSnapshotConfiguration, WKWebView};

    if rect.width < 1.0 || rect.height < 1.0 {
        return Err("the card has no visible area in the window".to_string());
    }
    let frame = CGRect::new(
        CGPoint::new(rect.x, rect.y),
        CGSize::new(rect.width, rect.height),
    );
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    webview
        .with_webview(move |platform| {
            // with_webview runs this on the main thread.
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = tx.send(Err("capture ran off the main thread".to_string()));
                return;
            };
            // SAFETY: on macOS `inner()` is the window's live WKWebView.
            let view: &WKWebView = unsafe { &*platform.inner().cast::<WKWebView>() };
            let config = unsafe { WKSnapshotConfiguration::new(mtm) };
            unsafe {
                config.setRect(frame);
                config.setAfterScreenUpdates(true);
            }
            let done = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                // SAFETY: WebKit passes either a valid image or a valid error.
                let result = match unsafe { (image.as_ref(), error.as_ref()) } {
                    (Some(image), _) => png_of(image),
                    (None, Some(error)) => Err(error.localizedDescription().to_string()),
                    (None, None) => Err("WebKit returned no image".to_string()),
                };
                let _ = tx.send(result);
            });
            unsafe { view.takeSnapshotWithConfiguration_completionHandler(Some(&config), &done) };
        })
        .map_err(|e| e.to_string())?;

    fn png_of(image: &NSImage) -> Result<Vec<u8>, String> {
        let tiff = image
            .TIFFRepresentation()
            .ok_or("the capture has no bitmap")?;
        let bitmap =
            NSBitmapImageRep::imageRepWithData(&tiff).ok_or("the capture has no bitmap")?;
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }
        .ok_or("the capture could not be encoded as PNG")?;
        Ok(png.to_vec())
    }

    tauri::async_runtime::spawn_blocking(move || {
        rx.recv_timeout(CAPTURE_TIMEOUT)
            .unwrap_or_else(|_| Err("WebKit did not finish the capture".to_string()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "macos"))]
async fn capture(_webview: &tauri::Webview, _rect: &Rect) -> Result<Vec<u8>, String> {
    Err("snapshots are captured only on macOS".to_string())
}
