pub mod profiles;
pub mod repo;
pub mod routes;
pub mod state;
pub mod stop_triggers;
pub mod store;
pub mod viewer;

use axum::routing::{any, delete, get, post, put};
use axum::Router;
use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(viewer::index))
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
        .route("/api/events", get(routes::events))
        .route("/api/cards/:id/images/:index", get(routes::get_card_image))
        .route(
            "/api/cards/:id/reply",
            post(routes::post_card_reply).get(routes::get_card_reply),
        )
        .route("/api/open", post(routes::open_path))
        .route("/*path", any(viewer::asset))
        .with_state(state)
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
                eprintln!("canvasd: accept failed: {e}");
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
