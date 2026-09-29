fn main() {
  // Registering an app manifest at all switches every app command onto ACL
  // enforcement: a command nothing grants in capabilities/ fails with
  // "Command not found". capabilities/viewer.json grants each one below.
  tauri_build::try_build(
    tauri_build::Attributes::new().app_manifest(
      tauri_build::AppManifest::new()
        .commands(&["set_pinned", "get_pinned", "open_settings", "daemon_status"]),
    ),
  )
  .expect("failed to run tauri-build");
}
