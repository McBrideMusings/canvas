use canvasd::build_router;
use canvasd::state::AppState;

/// `canvas daemon install`: installs this binary as the launchd agent's, the
/// same install Canvas.app runs on launch, restarting canvasd only when the
/// binary or the plist changed.
pub fn install() -> Result<serde_json::Value, String> {
    let exe = std::env::current_exe().map_err(|e| format!("couldn't locate this binary: {e}"))?;
    let home = std::env::var_os("HOME").ok_or("no HOME: cannot place the launchd agent")?;
    let outcome = canvas_core::service::install(&exe, std::path::Path::new(&home))?;
    canvas_core::log::info(
        "daemon install",
        &[
            ("from", &exe.display()),
            ("binary_changed", &outcome.binary_changed),
            ("plist_changed", &outcome.plist_changed),
            ("action", &outcome.action.as_str()),
        ],
    );
    Ok(serde_json::json!({
        "binaryChanged": outcome.binary_changed,
        "plistChanged": outcome.plist_changed,
        "action": outcome.action.as_str(),
    }))
}

pub fn run() {
    canvas_core::log::init_background(canvas_core::log::Process::Daemon);
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
        canvas_core::log::error(
            "refusing to start: already running",
            &[("socket", &socket.display())],
        );
        canvas_core::log::flush();
        std::process::exit(1);
    }
    let _ = std::fs::remove_file(&socket);

    let state = match canvas_core::paths::data_dir() {
        Some(dir) => AppState::open(&dir).await,
        None => {
            eprintln!("canvasd: no data directory; the stream will not persist");
            canvas_core::log::warn("no data directory; the stream will not persist", &[]);
            AppState::new()
        }
    };
    canvasd::state::spawn_liveness_sweep(state.clone());
    canvasd::refresh::spawn_refresh_loop(state.clone());
    canvasd::watcher::spawn_artifact_watcher(state.clone());
    let app = build_router(state);

    let listener =
        tokio::net::UnixListener::bind(&socket).expect("failed to bind the canvasd socket");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
            .expect("failed to restrict the canvasd socket to its owner");
    }
    println!("canvasd listening on {}", socket.display());
    canvas_core::log::info(
        "daemon started",
        &[
            ("socket", &socket.display()),
            ("pid", &std::process::id()),
            ("version", &env!("CARGO_PKG_VERSION")),
        ],
    );
    canvasd::serve_unix(listener, app).await;
}
