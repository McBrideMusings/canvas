//! Everything between Canvas.app's webviews and canvasd's Unix socket.
//!
//! canvasd has no TCP port, so a webview can't fetch it directly. Two paths
//! stand in for it:
//!
//! - `proxy` answers every request to the `canvas://` scheme by forwarding it
//!   to the socket, so the viewer, its assets and its `/api/*` calls work
//!   unchanged.
//! - `forward_events` holds the one `/api/events` stream open and re-emits
//!   each event to the webviews as Tauri events. A custom scheme response is
//!   one whole body, so it can't carry a stream.

use std::io::BufRead;
use std::time::Duration;

use tauri::http::{Request, Response};
use tauri::{AppHandle, Emitter};

const WAITING_PAGE: &str = include_str!("../../dist/index.html");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
// canvasd sends a keep-alive every 15s, so three missed ones mean the stream is dead.
const STREAM_TIMEOUT: Duration = Duration::from_secs(45);

fn reply(status: u16, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Cache-Control", "no-store")
        .body(body)
        .expect("static response parts are valid")
}

/// Forwards one `canvas://localhost/...` request to canvasd and returns its
/// answer. Only pages on this scheme can reach it, and a card's sandboxed
/// iframe can send nothing but image GETs (its CSP has no `connect-src`).
pub fn proxy(request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let method = request.method().as_str();
    let path = request
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");

    if path.split('?').next() == Some("/api/events") {
        // The stream never ends; reading it as one body would hold a thread
        // forever. forward_events owns it.
        return reply(404, "text/plain", Vec::new());
    }
    let Some(socket) = canvas_core::paths::socket_path() else {
        return reply(503, "text/html", WAITING_PAGE.as_bytes().to_vec());
    };
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok());
    let headers: Vec<(&str, &str)> = content_type
        .map(|ct| vec![("Content-Type", ct)])
        .unwrap_or_default();
    match canvas_core::unix_http::request(
        &socket,
        method,
        path,
        &headers,
        request.body(),
        Some(REQUEST_TIMEOUT),
    ) {
        Ok(response) => reply(
            response.status,
            response
                .content_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
            response.body,
        ),
        Err(_) => reply(503, "text/html", WAITING_PAGE.as_bytes().to_vec()),
    }
}

/// Runs forever on its own thread: connects to `/api/events`, emits a
/// `canvas-stream` event when the connection opens or drops, and a
/// `canvas-event` `{event, data}` for every server-sent event, reconnecting
/// once a second after any drop.
pub fn forward_events(app: AppHandle) {
    loop {
        let stream = canvas_core::paths::socket_path()
            .and_then(|socket| canvas_core::unix_http::open_stream(&socket, "/api/events", STREAM_TIMEOUT).ok())
            .filter(|s| s.status == 200);
        if let Some(stream) = stream {
            let _ = app.emit("canvas-stream", "open");
            read_events(&app, stream.body);
            let _ = app.emit("canvas-stream", "closed");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn read_events(app: &AppHandle, reader: impl BufRead) {
    let mut event = String::new();
    let mut data = String::new();
    for line in reader.lines() {
        let Ok(line) = line else { return };
        if line.is_empty() {
            if !event.is_empty() {
                let _ = app.emit(
                    "canvas-event",
                    serde_json::json!({ "event": event, "data": data }),
                );
            }
            event.clear();
            data.clear();
        } else if let Some(name) = line.strip_prefix("event:") {
            event = name.trim().to_string();
        } else if let Some(chunk) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(chunk.strip_prefix(' ').unwrap_or(chunk));
        }
    }
}
