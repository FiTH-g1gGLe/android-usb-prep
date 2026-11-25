//! Picks the right disk backend for the OS we're compiled for.
//!
//! Every backend exposes the same functions:
//!   list_usb_drives()  -> Vec<UsbDrive>
//!   format_drive(req)  -> mount root ("E:\\" or "/Volumes/ANDROID_USB")
//!   after_mount(root)  -> stop OS clutter (Spotlight etc.)
//!   before_eject_cleanup(root)
//!   eject(drive_id, mount) -> friendly message

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod unsupported;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use unsupported::*;
