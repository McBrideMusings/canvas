//! The export route against a local stand-in CDN, reached through the debug
//! build's `CANVAS_CDN_ORIGIN` override: no test here touches the internet.
//! A release build ignores the override, so these run only in a debug one.
#![cfg(debug_assertions)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use canvas_core::{Card, ExportResult, ExportWarning, ExportWarningKind, MAX_ASSET_BYTES};
use canvasd::build_router;
use canvasd::state::AppState;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

/// Starts the stand-in once per test binary and points the override at it.
fn stand_in_cdn() {
    static ORIGIN: OnceLock<String> = OnceLock::new();
    let origin = ORIGIN.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || serve(stream));
            }
        });
        origin
    });
    std::env::set_var(canvasd::cdn::OVERRIDE_ENV, origin);
}

fn serve(mut stream: std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap() > 2 {
        line.clear();
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let location = match path {
        "/unpkg.com/moved.js" => Some("/cdnjs.cloudflare.com/ajax/libs/lib/1.0/lib.min.js"),
        "/unpkg.com/away.js" => Some("https://example.com/x.js"),
        "/unpkg.com/lit" => Some("https://unpkg.com/lit@3/index.js"),
        "/unpkg.com/theme" => Some("https://unpkg.com/theme@2/a.css"),
        "/unpkg.com/sub" => Some("https://unpkg.com/theme@2/sub/s.css"),
        _ => None,
    };
    if let Some(location) = location {
        let location = location.replace("/cdnjs", "https://cdnjs");
        let head = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(head.as_bytes());
        return;
    }
    let (status, body): (&str, Vec<u8>) = match path {
        "/cdnjs.cloudflare.com/ajax/libs/lib/1.0/lib.min.js" => {
            ("200 OK", b"window.libLoaded=true;".to_vec())
        }
        "/unpkg.com/theme@1/a.css" => ("200 OK", b"@import \"b.css\";.a{color:red}".to_vec()),
        "/unpkg.com/theme@1/b.css" => (
            "200 OK",
            b"@font-face{font-family:F;src:url(https://fonts.gstatic.com/s/f/v1/f.woff2) format('woff2')}"
                .to_vec(),
        ),
        "/fonts.gstatic.com/s/f/v1/f.woff2" => ("200 OK", b"wOF2".to_vec()),
        "/unpkg.com/lit@3/index.js" => (
            "200 OK",
            b"import\"./lit-html.js\";export const v=1;".to_vec(),
        ),
        "/unpkg.com/lit@3/lit-html.js" => ("200 OK", b"export const h=2;".to_vec()),
        "/unpkg.com/theme@2/a.css" => (
            "200 OK",
            b"@import \"b.css\";@import \"https://unpkg.com/sub\";.a{color:red}".to_vec(),
        ),
        "/unpkg.com/theme@2/sub/s.css" => ("200 OK", b"@import \"c.css\";.s{}".to_vec()),
        "/unpkg.com/theme@2/sub/c.css" => ("200 OK", b".c{color:green}".to_vec()),
        "/unpkg.com/theme@2/b.css" => ("200 OK", b".b{color:blue}".to_vec()),
        "/unpkg.com/big.js" => ("200 OK", vec![b'x'; MAX_ASSET_BYTES + 100]),
        "/unpkg.com/slow.js" => {
            std::thread::sleep(Duration::from_secs(12));
            ("200 OK", b"late".to_vec())
        }
        _ => ("500 Internal Server Error", b"boom".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

async fn export(html: &str) -> ExportResult {
    stand_in_cdn();
    let app = build_router(AppState::new());
    let post = Request::builder()
        .method("POST")
        .uri("/api/posts")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"session_id": "s1", "cwd": "/tmp/proj", "agent": "claude-code", "html": html})
                .to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(post).await.unwrap();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let card: Card = serde_json::from_slice(&bytes).unwrap();
    let get = Request::builder()
        .uri(format!("/api/cards/{}/export", card.id))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(get).await.unwrap();
    assert!(response.status().is_success());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn scripts_stylesheets_imports_and_fonts_are_inlined() {
    let r = export(
        r#"<link rel="stylesheet" href="https://unpkg.com/theme@1/a.css">
<script src="https://cdnjs.cloudflare.com/ajax/libs/lib/1.0/lib.min.js"></script>
<p class="a">hi</p>"#,
    )
    .await;
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(!r.html.contains("http"), "{}", r.html);
    assert!(r.html.contains("<script>window.libLoaded=true;</script>"));
    assert!(r.html.contains(
        r#"<style>@font-face{font-family:F;src:url("data:font/woff2;base64,d09GMg==") format('woff2')}.a{color:red}</style>"#
    ), "{}", r.html);
}

#[tokio::test]
async fn references_in_a_redirected_download_resolve_from_where_it_landed() {
    let r = export(
        r#"<link rel="stylesheet" href="https://unpkg.com/theme"><script type="module" src="https://unpkg.com/lit"></script>"#,
    )
    .await;
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(
        r.html
            .contains("<style>.b{color:blue}.c{color:green}.s{}.a{color:red}</style>"),
        "{}",
        r.html
    );
    let start = r.html.find(r#"<script type="importmap">"#).unwrap() + 25;
    let end = start + r.html[start..].find("</script>").unwrap();
    let map: serde_json::Value = serde_json::from_str(&r.html[start..end]).unwrap();
    let data = |js: &str| {
        format!(
            "data:text/javascript;base64,{}",
            canvas_core::base64(js.as_bytes())
        )
    };
    assert_eq!(
        map,
        json!({"imports": {
            "https://unpkg.com/lit": data(r#"import"https://unpkg.com/lit@3/lit-html.js";export const v=1;"#),
            "https://unpkg.com/lit@3/lit-html.js": data("export const h=2;"),
        }})
    );
}

#[tokio::test]
async fn a_failed_download_keeps_its_link_and_warns() {
    let r = export(
        r#"<script src="https://unpkg.com/missing.js"></script><script src="https://cdnjs.cloudflare.com/ajax/libs/lib/1.0/lib.min.js"></script>"#,
    )
    .await;
    assert!(r
        .html
        .contains(r#"<script src="https://unpkg.com/missing.js"></script>"#));
    assert!(r.html.contains("<script>window.libLoaded=true;</script>"));
    assert_eq!(
        r.warnings,
        vec![ExportWarning {
            kind: ExportWarningKind::FetchFailed,
            target: "https://unpkg.com/missing.js".into(),
            reason: "HTTP 500".into(),
        }]
    );
}

#[tokio::test]
async fn oversize_and_slow_downloads_warn() {
    let r = export(
        r#"<script src="https://unpkg.com/big.js"></script><script src="https://unpkg.com/slow.js"></script>"#,
    )
    .await;
    let got: Vec<_> = r
        .warnings
        .iter()
        .map(|w| (w.kind, w.target.as_str(), w.reason.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                ExportWarningKind::FetchFailed,
                "https://unpkg.com/big.js",
                "over 512 KB"
            ),
            (
                ExportWarningKind::FetchFailed,
                "https://unpkg.com/slow.js",
                "timed out after 10s"
            ),
        ]
    );
    assert!(r
        .html
        .contains(r#"<script src="https://unpkg.com/slow.js">"#));
}

#[tokio::test]
async fn redirects_are_followed_only_on_the_allowed_hosts() {
    let r = export(
        r#"<script src="https://unpkg.com/moved.js"></script><script src="https://unpkg.com/away.js"></script>"#,
    )
    .await;
    assert!(r.html.contains("<script>window.libLoaded=true;</script>"));
    assert!(r
        .html
        .contains(r#"<script src="https://unpkg.com/away.js"></script>"#));
    assert_eq!(
        r.warnings,
        vec![ExportWarning {
            kind: ExportWarningKind::FetchFailed,
            target: "https://unpkg.com/away.js".into(),
            reason: "redirected off the allowed hosts to https://example.com/x.js".into(),
        }]
    );
}
