fn main() {
    // An app manifest puts our own commands under the same permission system as
    // plugins: `open_workspace` is granted to the bundled setup page only, never
    // to the remote workspace or a sign-in page. The dictation commands are
    // granted to the workspace the person chose, and nothing else.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&[
                "open_workspace",
                "dictation_status",
                "dictation_install",
                "dictation_start",
                "dictation_stop",
                "dictation_cancel",
            ])),
    )
    .expect("failed to run tauri-build");
}
