// Build script:
//  1. Registers our custom Rust commands with Tauri v2's permission system, so the
//     capability file (capabilities/default.json) can allow them explicitly.
//  2. On Windows, embeds an application manifest with `requireAdministrator`, so
//     Windows shows the normal UAC "Do you want to allow this app..." prompt at launch.
//     (Formatting disks needs admin rights.)

const WINDOWS_MANIFEST: &str = include_str!("windows-app-manifest.xml");

fn main() {
    let mut attrs =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "app_platform",
            "list_usb_drives",
            "inspect_files",
            "prepare_usb",
            "eject_usb",
        ]));

    // build.rs runs on the *host*, so check the *target* OS via Cargo's env var.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        attrs = attrs.windows_attributes(
            tauri_build::WindowsAttributes::new().app_manifest(WINDOWS_MANIFEST),
        );
    }

    tauri_build::try_build(attrs).expect("failed to run tauri-build");
}
