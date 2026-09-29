//! Where canvasd keeps its files, shared by the daemon, the CLI and the app so
//! all three find the same socket.

use std::path::PathBuf;

/// `CANVAS_DATA_DIR`, else `~/Library/Application Support/canvas`.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CANVAS_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/canvas"))
}

/// The Unix socket canvasd listens on: `CANVAS_SOCKET`, else `canvasd.sock` in
/// the data dir.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CANVAS_SOCKET") {
        return Some(PathBuf::from(path));
    }
    Some(data_dir()?.join("canvasd.sock"))
}
