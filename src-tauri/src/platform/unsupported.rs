//! Fallback so the project still compiles on Linux (e.g. for CI checks).
//! Disk operations are intentionally not implemented here.

use crate::{FormatRequest, UsbDrive};
use std::path::Path;

pub fn list_usb_drives() -> Result<Vec<UsbDrive>, String> {
    Ok(Vec::new())
}

pub fn format_drive(_req: &FormatRequest) -> Result<String, String> {
    Err("This app only supports Windows and macOS.".into())
}

pub fn after_mount(_root: &Path) {}

pub fn before_eject_cleanup(_root: &Path) {}

pub fn eject(_drive_id: &str, _mount: &str) -> Result<String, String> {
    Err("This app only supports Windows and macOS.".into())
}
