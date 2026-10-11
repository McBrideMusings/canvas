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
use std::sync::Mutex;
use std::time::Duration;

use tauri::http::{Request, Response};
use tauri::{AppHandle, Emitter};

const WAITING_PAGE: &str = include_str!("../../dist/index.html");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
// canvasd sends a keep-alive every 15s, so three missed ones mean the stream is dead.
const STREAM_TIMEOUT: Duration = Duration::from_secs(45);

// Headers canvasd sets that must reach the webview: an artifact file's CSP
// (its `connect-src 'none'` and `sandbox`), the CORS header its module
// scripts need in the pane's opaque origin, and a media file's range headers,
// so a video reads the bounded 206 slices canvasd serves rather than the
// whole file on every loop. wry sets Content-Length from the body.
const PASSED_HEADERS: &[&str] = &[
    "content-security-policy",
    "access-control-allow-origin",
    "accept-ranges",
    "content-range",
];

// Request headers the webview sends that canvasd reads.
const FORWARDED_HEADERS: &[&str] = &["content-type", "range"];

/// Why the last attempt to reach canvasd failed, so a daemon that stays down
/// logs one line per change of reason rather than one per request.
struct FailureLog(Mutex<Option<String>>);

impl FailureLog {
    const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Records a failure; true when its reason differs from the last one.
    fn failed(&self, reason: &str) -> bool {
        let mut last = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_deref() == Some(reason) {
            return false;
        }
        *last = Some(reason.to_string());
        true
    }

    /// Records a success; true when it ends a run of failures.
    fn recovered(&self) -> bool {
        let mut last = self.0.lock().unwrap_or_else(|e| e.into_inner());
        last.take().is_some()
    }
}

// Every proxied request shares one, since the waiting page and the viewer's
// calls each fail the same way while canvasd is down.
static PROXY_FAILURES: FailureLog = FailureLog::new();

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
    let headers = forwarded_headers(request.headers());
    match canvas_core::unix_http::request(
        &socket,
        method,
        path,
        &headers,
        request.body(),
        Some(REQUEST_TIMEOUT),
    ) {
        Ok(response) => {
            if PROXY_FAILURES.recovered() {
                canvas_core::log::info(
                    "bridge reaching canvasd again",
                    &[("method", &method), ("path", &path)],
                );
            }
            reply_with(
                response.status,
                response
                    .content_type
                    .as_deref()
                    .unwrap_or("application/octet-stream"),
                &response.headers,
                response.body,
            )
        }
        Err(e) => {
            // The first request to fail for a new reason is logged; the rest
            // (the waiting page reloads once a second) are not.
            if PROXY_FAILURES.failed(&e.to_string()) {
                canvas_core::log::warn(
                    "bridge request failed",
                    &[("method", &method), ("path", &path), ("error", &e)],
                );
            }
            reply(503, "text/html", WAITING_PAGE.as_bytes().to_vec())
        }
    }
}

fn forwarded_headers(headers: &tauri::http::HeaderMap) -> Vec<(&'static str, &str)> {
    FORWARDED_HEADERS
        .iter()
        .filter_map(|&name| Some((name, headers.get(name)?.to_str().ok()?)))
        .collect()
}

/// Runs forever on its own thread: connects to `/api/events`, emits a
/// `canvas-stream` event when the connection opens or drops, and a
/// `canvas-event` `{event, data}` for every server-sent event, reconnecting
/// once a second after any drop.
pub fn forward_events(app: AppHandle) {
    // Only a change in why the stream can't open is logged, so a daemon that
    // stays down writes one line rather than one a second.
    let failures = FailureLog::new();
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
                failures.recovered();
                canvas_core::log::info("event stream open", &[]);
                let _ = app.emit("canvas-stream", "open");
                read_events(&app, stream.body);
                let _ = app.emit("canvas-stream", "closed");
                canvas_core::log::warn("event stream closed", &[]);
            }
            Err(e) => {
                if failures.failed(&e) {
                    canvas_core::log::warn("event stream unavailable", &[("error", &e)]);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_request_reaches_canvasd_and_its_range_headers_come_back() {
        let request = Request::builder()
            .header("Range", "bytes=0-")
            .header("Accept", "*/*")
            .body(Vec::<u8>::new())
            .unwrap();
        assert_eq!(
            forwarded_headers(request.headers()),
            vec![("range", "bytes=0-")]
        );

        let canvasd = [
            ("accept-ranges".to_string(), "bytes".to_string()),
            ("content-range".to_string(), "bytes 0-99/1000".to_string()),
            ("x-other".to_string(), "dropped".to_string()),
        ];
        let response = reply_with(206, "video/mp4", &canvasd, vec![0; 100]);
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["content-range"], "bytes 0-99/1000");
        assert_eq!(response.headers()["accept-ranges"], "bytes");
        assert!(response.headers().get("x-other").is_none());
    }

    #[test]
    fn a_failure_is_reported_once_per_change_of_reason() {
        let failures = FailureLog::new();
        let refused = "Connection refused (os error 61)";
        let reported: Vec<bool> = [
            refused,
            refused,
            refused,
            "No such file or directory (os error 2)",
        ]
        .iter()
        .map(|reason| failures.failed(reason))
        .collect();
        assert_eq!(reported, vec![true, false, false, true]);

        assert!(failures.recovered());
        assert!(!failures.recovered());
        assert!(failures.failed(refused));
    }
}
