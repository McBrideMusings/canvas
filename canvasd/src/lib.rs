pub mod repo;
pub mod routes;
pub mod state;
pub mod store;
pub mod viewer;

use axum::routing::{any, delete, get, post};
use axum::{middleware, Router};
use state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(viewer::index))
        .route("/api/sessions", post(routes::upsert_session))
        .route("/api/sessions/:id/end", post(routes::end_session))
        .route("/api/sessions/:id", delete(routes::delete_session))
        .route("/api/sessions/:id/cards", delete(routes::clear_session_cards))
        .route("/api/posts", post(routes::post_explicit))
        .route("/api/state", get(routes::get_state))
        .route("/api/cards/:id", delete(routes::delete_card))
        .route("/api/events", get(routes::events))
        .route("/api/cards/:id/images/:index", get(routes::get_card_image))
        .route("/api/open", post(routes::open_path))
        .route("/*path", any(viewer::asset))
        .layer(middleware::from_fn(routes::require_loopback_host))
        .with_state(state)
}
