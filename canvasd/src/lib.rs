pub mod routes;
pub mod state;
pub mod viewer;

use axum::routing::{get, post};
use axum::Router;
use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(viewer::index))
        .route("/api/sessions", post(routes::upsert_session))
        .route("/api/sessions/:id/end", post(routes::end_session))
        .route("/api/turns", post(routes::post_turn))
        .route("/api/posts", post(routes::post_explicit))
        .route("/api/state", get(routes::get_state))
        .route("/api/events", get(routes::events))
        .route("/api/file", get(routes::get_file))
        .route("/api/open", post(routes::open_path))
        .route("/*path", get(viewer::asset))
        .with_state(state)
}
