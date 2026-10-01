fn main() {
    // An app manifest puts our own commands under the same permission system as
    // plugins: `open_workspace` is granted to the bundled setup page only, never
    // to the remote workspace or a sign-in page.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["open_workspace"])),
    )
    .expect("failed to run tauri-build");
}
