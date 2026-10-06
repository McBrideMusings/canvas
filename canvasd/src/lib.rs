pub mod artifact_routes;
pub mod artifacts;
pub mod cdn;
pub mod export;
pub mod profiles;
pub mod provenance;
pub mod refresh;
pub mod repo;
pub mod routes;
pub mod state;
pub mod stop_triggers;
pub mod store;
pub mod viewer;
pub mod watcher;

use axum::routing::{any, delete, get, post, put};
use axum::Router;
use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(viewer::index))
        .route("/canvas.css", get(viewer::stylesheet))
        .route("/api/sessions/:id/end", post(routes::end_session))
        .route("/api/sessions/:id", delete(routes::delete_session))
        .route(
            "/api/sessions/:id/cards",
            delete(routes::clear_session_cards),
        )
        .route("/api/posts", post(routes::post_explicit))
        .route("/api/state", get(routes::get_state))
        .route("/api/profiles/:kind", get(routes::get_profiles))
        .route(
            "/api/profiles/:kind/effective",
            get(routes::get_effective_profile),
        )
        .route(
            "/api/profiles/:kind/definitions",
            put(routes::set_profile_text),
        )
        .route("/api/profiles/:kind/mode", put(routes::set_profile_mode))
        .route(
            "/api/profiles/:kind/global",
            put(routes::set_global_profile),
        )
        .route("/api/profiles/:kind/repos", put(routes::set_repo_profile))
        .route(
            "/api/cards/:id",
            get(routes::get_card)
                .delete(routes::delete_card)
                .put(routes::update_card),
        )
        .route("/api/cards/:id/export", get(routes::export_card))
        .route("/api/cards/:id/focus", post(routes::focus_card))
        .route("/api/cards/:id/snapshot", post(routes::snapshot_card))
        .route(
            "/api/snapshots/:request",
            // A Retina capture of a tall card runs past axum's 2 MB default.
            post(routes::deliver_snapshot).layer(axum::extract::DefaultBodyLimit::max(64 << 20)),
        )
        .route(
            "/api/theme",
            post(routes::set_theme)
                .put(routes::report_theme)
                .get(routes::get_theme),
        )
        .route("/api/events", get(routes::events))
        .route("/api/cards/:id/images/:index", get(routes::get_card_image))
        .route(
            "/api/cards/:id/reply",
            post(routes::post_card_reply).get(routes::get_card_reply),
        )
        .route(
            "/api/cards/:id/data",
            put(routes::put_card_data).get(routes::get_card_data),
        )
        .route(
            "/api/artifacts",
            get(artifact_routes::list_artifacts).post(artifact_routes::new_artifact),
        )
        .route(
            "/api/artifacts/:id",
            get(artifact_routes::get_artifact).delete(artifact_routes::delete_artifact),
        )
        .route(
            "/api/artifacts/:id/put",
            post(artifact_routes::put_artifact),
        )
        .route(
            "/api/artifacts/:id/relink",
            post(artifact_routes::relink_artifact),
        )
        .route("/api/artifacts/:id/log", get(artifact_routes::artifact_log))
        .route(
            "/api/artifacts/:id/data",
            put(artifact_routes::put_artifact_data),
        )
        .route(
            "/api/artifacts/:id/errors",
            post(artifact_routes::report_script_error),
        )
        .route(
            "/api/artifacts/:id/open",
            post(artifact_routes::open_artifact_link),
        )
        .route(
            "/api/artifacts/:id/focus",
            post(artifact_routes::focus_artifact),
        )
        .route(
            "/api/artifacts/:id/snapshot",
            post(artifact_routes::snapshot_artifact),
        )
        .route(
            "/api/artifacts/:id/pane",
            post(artifact_routes::pane_artifact),
        )
        .route(
            "/api/artifact-pane",
            get(artifact_routes::get_pane)
                .put(artifact_routes::report_pane)
                .delete(artifact_routes::clear_pane),
        )
        .route("/artifacts/:id/", get(artifact_routes::artifact_entry))
        .route("/artifacts/:id/*path", get(artifact_routes::artifact_file))
        .route("/api/open", post(routes::open_path))
        .route("/*path", any(viewer::asset))
        .layer(axum::middleware::from_fn(log_request))
        .with_state(state)
}

/// Logs one line per request: method, path, status, duration, and for a 4xx
/// or 5xx the body the handler answered with, which is its error text.
async fn log_request(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use canvas_core::log::{self, Level};

    let started = std::time::Instant::now();
    let method = request.method().clone();
    let path = request
        .uri()
        .path_and_query()
        .map_or_else(|| request.uri().path().to_string(), |p| p.to_string());
    let response = next.run(request).await;
    let status = response.status();
    let ms = started.elapsed().as_millis();
    if !(status.is_client_error() || status.is_server_error()) {
        log::info(
            &format!("{method} {path}"),
            &[("status", &status.as_u16()), ("ms", &ms)],
        );
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    let text = log::error_text(&bytes);
    let level = if status.is_server_error() {
        Level::Error
    } else {
        Level::Warn
    };
    let status = status.as_u16();
    let mut fields: Vec<(&str, &dyn std::fmt::Display)> = vec![("status", &status), ("ms", &ms)];
    if let Some(text) = &text {
        fields.push(("error", text));
    }
    log::write(level, &format!("{method} {path}"), &fields);
    axum::response::Response::from_parts(parts, axum::body::Body::from(bytes))
}

/// Serve `app` on a Unix socket until the process exits. axum's own `serve`
/// only takes a TCP listener, so this runs the accept loop itself.
pub async fn serve_unix(listener: tokio::net::UnixListener, app: Router) {
    use hyper::body::Incoming;
    use hyper_util::rt::TokioIo;
    use tower::ServiceExt;

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                canvas_core::log::error("accept failed", &[("error", &e)]);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request: hyper::Request<Incoming>| {
                app.clone().oneshot(request)
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
