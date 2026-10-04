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

// Headers canvasd sets that must reach the webview: an artifact file's CSP
// (its `connect-src 'none'` and `sandbox`) and the CORS header its module
// scripts need in the pane's opaque origin.
const PASSED_HEADERS: &[&str] = &["content-security-policy", "access-control-allow-origin"];

fn reply(status: u16, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    reply_with(status, content_type, &[], body)
}

fn reply_with(
    status: u16,
    content_type: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> Response<Vec<u8>> {
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Cache-Control", "no-store");
    for (name, value) in headers {
        if PASSED_HEADERS.contains(&name.as_str()) {
            builder = builder.header(name.as_str(), value.as_str());
        }
    }
    builder.body(body).unwrap_or_else(|e| {
        canvas_core::log::error("bridge: canvasd sent an unusable header", &[("error", &e)]);
        let mut response = Response::new(Vec::new());
        *response.status_mut() = tauri::http::StatusCode::BAD_GATEWAY;
        response
    })
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
        canvas_core::log::error("bridge: no HOME to place the socket", &[]);
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
        Ok(response) => reply_with(
            response.status,
            response
                .content_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
            &response.headers,
            response.body,
        ),
        Err(e) => {
            canvas_core::log::warn(
                "bridge request failed",
                &[("method", &method), ("path", &path), ("error", &e)],
            );
            reply(503, "text/html", WAITING_PAGE.as_bytes().to_vec())
        }
    }
}

/// Runs forever on its own thread: connects to `/api/events`, emits a
/// `canvas-stream` event when the connection opens or drops, and a
/// `canvas-event` `{event, data}` for every server-sent event, reconnecting
/// once a second after any drop.
pub fn forward_events(app: AppHandle) {
    // Only a change in why the stream can't open is logged, so a daemon that
    // stays down writes one line rather than one a second.
    let mut last_failure: Option<String> = None;
    loop {
        let stream = match canvas_core::paths::socket_path() {
            None => Err("no HOME to place the socket".to_string()),
            Some(socket) => match canvas_core::unix_http::open_stream(&socket, "/api/events", STREAM_TIMEOUT) {
                Ok(s) if s.status == 200 => Ok(s),
                Ok(s) => Err(format!("canvasd answered HTTP {}", s.status)),
                Err(e) => Err(e.to_string()),
            },
        };
        match stream {
            Ok(stream) => {
                last_failure = None;
                canvas_core::log::info("event stream open", &[]);
                let _ = app.emit("canvas-stream", "open");
                read_events(&app, stream.body);
                let _ = app.emit("canvas-stream", "closed");
                canvas_core::log::warn("event stream closed", &[]);
            }
            Err(e) => {
                if last_failure.as_deref() != Some(e.as_str()) {
                    canvas_core::log::warn("event stream unavailable", &[("error", &e)]);
                }
                last_failure = Some(e);
            }
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
