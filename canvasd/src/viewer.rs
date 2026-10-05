use axum::extract::Path;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../viewer/"]
struct Viewer;

fn serve(path: &str) -> Response {
    match Viewer::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.essence_str().to_string())],
                file.data,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn index() -> Response {
    serve("index.html")
}

/// The public Canvas stylesheet an artifact or post links as `/canvas.css`:
/// the tokens the viewer itself uses, then the base element styles only a
/// linking page gets. `canvas post` inlines this same text into a post.
pub fn canvas_css() -> String {
    ["canvas-tokens.css", "canvas-base.css"]
        .iter()
        .filter_map(|name| Viewer::get(name))
        .map(|file| String::from_utf8_lossy(&file.data).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn stylesheet() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css")],
        canvas_css(),
    )
        .into_response()
}

/// The wildcard catch-all matches every path regardless of method, so a
/// non-GET request to a path that isn't a registered API route (e.g. a
/// removed endpoint like the old `/api/turns`) would otherwise get axum's
/// built-in 405 for this route instead of a plain 404. Handling every method
/// here and rejecting anything but GET keeps that a genuine "not found".
pub async fn asset(method: Method, Path(path): Path<String>) -> Response {
    if method != Method::GET {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve(&path)
}
