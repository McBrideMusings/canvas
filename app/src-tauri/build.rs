fn main() {
  // Registering an app manifest at all switches every app command onto ACL
  // enforcement — canvas_url included, even though it isn't the command this
  // change cares about. It keeps working because capabilities/default.json
  // grants it "allow-canvas-url" explicitly; drop that grant and the waiting
  // page's own invoke("canvas_url") starts failing with "Command not found".
  tauri_build::try_build(
    tauri_build::Attributes::new().app_manifest(
      tauri_build::AppManifest::new()
        .commands(&["canvas_url", "set_pinned", "get_pinned", "open_settings", "daemon_status"]),
    ),
  )
  .expect("failed to run tauri-build");
}
