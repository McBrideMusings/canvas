use canvasd::build_router;
use canvasd::state::AppState;

pub fn run() {
    let rt = tokio::runtime::Runtime::new().expect("failed to start tokio runtime");
    rt.block_on(serve());
}

async fn serve() {
    let socket =
        canvas_core::paths::socket_path().expect("no HOME: cannot place the canvasd socket");
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).expect("failed to create the canvasd socket directory");
    }
    // A socket file outlives the process that made it, so a leftover file
    // means nothing until something answers on it. Only replace one nobody
    // answers, or a second daemon would steal the first one's clients.
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        eprintln!("canvasd: already running on {}", socket.display());
        std::process::exit(1);
    }
    let _ = std::fs::remove_file(&socket);

    let state = match canvas_core::paths::data_dir() {
        Some(dir) => AppState::open(&dir).await,
        None => {
            eprintln!("canvasd: no data directory; the stream will not persist");
            AppState::new()
        }
    };
    canvasd::state::spawn_liveness_sweep(state.clone());
    let app = build_router(state);

    let listener =
        tokio::net::UnixListener::bind(&socket).expect("failed to bind the canvasd socket");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .expect("failed to restrict the canvasd socket to its owner");
    }
    println!("canvasd listening on {}", socket.display());
    canvasd::serve_unix(listener, app).await;
}
