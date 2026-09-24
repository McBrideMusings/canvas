use canvasd::build_router;
use canvasd::state::AppState;

pub fn run() {
    let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    rt.block_on(serve());
}

async fn serve() {
    let port: u16 = std::env::var("CANVAS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8229);

    let state = AppState::new();
    let app = build_router(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    println!("canvasd listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind canvasd port");
    axum::serve(listener, app)
        .await
        .expect("canvasd server error");
}
