//! Android USB Prep — Rust backend.
//!
//! All privileged disk work (listing, wiping, formatting, copying, verifying,
//! ejecting) lives here as `#[tauri::command]` functions. The frontend never runs
//! shell commands itself.

mod copy;
mod platform;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// FAT32 cannot hold a single file of 4 GiB or more (max is 4,294,967,295 bytes).
pub const FAT32_MAX_FILE: u64 = 4_294_967_295;
/// Windows' built-in FAT32 formatter refuses partitions above 32 GiB.
pub const FAT32_WINDOWS_LIMIT: u64 = 32 * 1024 * 1024 * 1024;
/// Size used when capping FAT32 (32 GiB minus 1 MiB, so we stay safely under the limit).
pub const FAT32_CAP_BYTES: u64 = FAT32_WINDOWS_LIMIT - 1024 * 1024;

/// Folder tree created on every prepared stick.
pub const FOLDERS: &[&str] = &[
    "Android/data",
    "Android/obb",
    "Download",
    "DCIM",
    "Documents",
    "Movies",
    "Music",
    "Pictures",
];

const ALLOWED_EXT: &[&str] = &["apk", "xapk", "apks"];

static BUSY: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UsbDrive {
    /// Windows: physical disk number ("3"). macOS: whole-disk id ("disk4").
    pub id: String,
    /// Friendly hardware/volume name, e.g. "SanDisk Ultra".
    pub name: String,
    /// What the user sees, e.g. "SanDisk Ultra (E:) — 32 GB".
    pub display: String,
    pub size_bytes: u64,
    /// Current mount point if any ("E:\\" or "/Volumes/NO NAME").
    pub mount: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    Fat32,
    ExFat,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrepareOptions {
    pub drive_id: String,
    pub expected_size: u64,
    /// "fat32" or "exfat"
    pub file_system: String,
    pub label: String,
    pub cap_fat32: bool,
    pub copy_to_root: bool,
    pub files: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrepareResult {
    pub drive_id: String,
    pub mount: String,
    pub label: String,
    pub files: Vec<VerifiedFile>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub destinations: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Status {
    stage: &'static str,
    message: String,
}

/// Everything a platform backend must provide.
pub struct FormatRequest<'a> {
    pub drive_id: &'a str,
    pub expected_size: u64,
    pub fs: FsKind,
    pub label: &'a str,
    /// Some(bytes) = make the partition this size; None = use the whole stick.
    pub partition_size: Option<u64>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// FAT32-safe label: max 11 chars, A-Z 0-9 _ only. Falls back to ANDROID_USB.
pub fn sanitize_label(input: &str) -> String {
    let mut out = String::new();
    for c in input.trim().chars() {
        let c = c.to_ascii_uppercase();
        if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
            out.push(c);
        } else if c == ' ' || c == '-' || c == '.' {
            out.push('_');
        }
        if out.len() >= 11 {
            break;
        }
    }
    if out.trim_matches('_').is_empty() {
        "ANDROID_USB".to_string()
    } else {
        out
    }
}

/// Windows Explorer counts 1 GB = 1024^3 bytes, macOS Finder uses 1000^3.
/// Match whatever the user's own computer shows.
pub fn human_size(bytes: u64) -> String {
    let k: f64 = if cfg!(target_os = "windows") {
        1024.0
    } else {
        1000.0
    };
    let b = bytes as f64;
    if b >= k * k * k {
        let gb = b / (k * k * k);
        if gb >= 10.0 {
            format!("{:.0} GB", gb)
        } else {
            format!("{:.1} GB", gb)
        }
    } else if b >= k * k {
        format!("{:.1} MB", b / (k * k))
    } else if b >= k {
        format!("{:.0} KB", b / k)
    } else {
        format!("{} bytes", bytes)
    }
}

fn has_allowed_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| ALLOWED_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn emit_status(app: &AppHandle, stage: &'static str, message: impl Into<String>) {
    let _ = app.emit(
        "prep-status",
        Status {
            stage,
            message: message.into(),
        },
    );
}

/// Polls until we can create (and delete) a file on the new volume.
/// Windows Explorer / Defender often hold the brand-new drive for 1–3 seconds.
pub fn wait_until_writable(root: &Path, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    let probe = root.join(".usbprep_probe.tmp");
    loop {
        match std::fs::write(&probe, b"ok") {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
                return Ok(());
            }
            Err(e) => {
                if start.elapsed() >= timeout {
                    return Err(format!(
                        "The USB stick was formatted but didn't become ready in time ({}). \
                         Unplug it, plug it back in and try again.",
                        e
                    ));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}

struct BusyGuard;
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn app_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

#[tauri::command]
async fn list_usb_drives() -> Result<Vec<UsbDrive>, String> {
    tauri::async_runtime::spawn_blocking(platform::list_usb_drives)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn inspect_files(paths: Vec<String>) -> Result<Vec<FileInfo>, String> {
    let mut out = Vec::new();
    for p in paths {
        let path = PathBuf::from(&p);
        if !has_allowed_ext(&path) {
            continue;
        }
        let meta = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        out.push(FileInfo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone()),
            path: p,
            size: meta.len(),
        });
    }
    Ok(out)
}

#[tauri::command]
async fn prepare_usb(app: AppHandle, options: PrepareOptions) -> Result<PrepareResult, String> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err("A USB stick is already being prepared. Please wait.".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = BusyGuard;
        prepare_blocking(&app, options)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn eject_usb(drive_id: String, mount: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || platform::eject(&drive_id, &mount))
        .await
        .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// The main workflow
// ---------------------------------------------------------------------------

fn prepare_blocking(app: &AppHandle, opts: PrepareOptions) -> Result<PrepareResult, String> {
    // ---- 1. Validate everything BEFORE touching the disk -----------------
    let fs = match opts.file_system.to_ascii_lowercase().as_str() {
        "fat32" => FsKind::Fat32,
        "exfat" => FsKind::ExFat,
        other => return Err(format!("Unknown format option: {}", other)),
    };
    let label = sanitize_label(&opts.label);

    let mut files: Vec<FileInfo> = Vec::new();
    for p in &opts.files {
        let path = PathBuf::from(p);
        if !has_allowed_ext(&path) {
            return Err(format!("'{}' isn't an APK file.", p));
        }
        let meta = std::fs::metadata(&path)
            .map_err(|_| format!("Can't find the file '{}'. Was it moved or deleted?", p))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if fs == FsKind::Fat32 && meta.len() >= FAT32_MAX_FILE {
            return Err(format!(
                "'{}' is 4 GB or bigger, which the Standard (FAT32) format can't hold. \
                 Choose 'Large Files over 4 GB (exFAT)' instead.",
                name
            ));
        }
        files.push(FileInfo {
            path: p.clone(),
            name,
            size: meta.len(),
        });
    }

    let partition_size =
        if fs == FsKind::Fat32 && opts.cap_fat32 && opts.expected_size > FAT32_WINDOWS_LIMIT {
            Some(FAT32_CAP_BYTES)
        } else {
            None
        };

    if cfg!(target_os = "windows")
        && fs == FsKind::Fat32
        && partition_size.is_none()
        && opts.expected_size > FAT32_WINDOWS_LIMIT
    {
        return Err(
            "Windows can't format FAT32 bigger than 32 GB. Tick 'Cap FAT32 partition at 32 GB' \
             or choose 'Large Files over 4 GB (exFAT)' to use the full stick."
                .into(),
        );
    }

    let copies = if opts.copy_to_root { 2 } else { 1 };
    let needed: u64 = files.iter().map(|f| f.size).sum::<u64>() * copies;
    let usable = partition_size.unwrap_or(opts.expected_size) / 100 * 98; // ~2% for filesystem
    if needed > usable {
        return Err(format!(
            "The apps need {} but the stick only has about {} of space.",
            human_size(needed),
            human_size(usable)
        ));
    }

    // ---- 2. Wipe + format ------------------------------------------------
    emit_status(app, "format", "Erasing and formatting the USB stick…");
    let mount = platform::format_drive(&FormatRequest {
        drive_id: &opts.drive_id,
        expected_size: opts.expected_size,
        fs,
        label: &label,
        partition_size,
    })?;
    let root = PathBuf::from(&mount);
    emit_status(app, "format", "Waiting for the stick to be ready…");
    wait_until_writable(&root, Duration::from_secs(10))?;
    platform::after_mount(&root);

    // ---- 3. Folder tree --------------------------------------------------
    emit_status(app, "folders", "Creating Android folders…");
    for f in FOLDERS {
        let dir = root.join(f);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Couldn't create folder {}: {}", dir.display(), e))?;
    }

    // ---- 4. Copy + verify ------------------------------------------------
    let mut dest_dirs = vec![root.join("Download")];
    if opts.copy_to_root {
        dest_dirs.push(root.clone());
    }
    let verified = if files.is_empty() {
        Vec::new()
    } else {
        emit_status(app, "copy", "Copying apps…");
        copy::copy_and_verify(app, &files, &dest_dirs)?
    };

    // ---- 5. Finish -------------------------------------------------------
    emit_status(app, "finish", "Tidying up…");
    platform::before_eject_cleanup(&root);
    emit_status(app, "done", "Your USB stick is ready!");

    Ok(PrepareResult {
        drive_id: opts.drive_id,
        mount,
        label,
        files: verified,
    })
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            app_platform,
            list_usb_drives,
            inspect_files,
            prepare_usb,
            eject_usb
        ])
        .run(tauri::generate_context!())
        .expect("error while running Android USB Prep");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_sanitized() {
        assert_eq!(sanitize_label("android usb"), "ANDROID_USB");
        assert_eq!(sanitize_label("my-stick!!2024 extra"), "MY_STICK202");
        assert_eq!(sanitize_label("   "), "ANDROID_USB");
        assert_eq!(sanitize_label("!!!"), "ANDROID_USB");
        assert_eq!(sanitize_label("tv"), "TV");
    }
}
