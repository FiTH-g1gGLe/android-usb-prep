//! macOS backend — `diskutil` with plist output.
//!
//! Safety rules:
//!  * Only `diskutil list -plist external physical` disks are considered, so the
//!    internal Apple SSD / APFS containers (disk0, disk1, ...) are never listed.
//!  * Each candidate is double-checked with `diskutil info`: Internal = false,
//!    BusProtocol = USB, and it must not host the running system ("/").
//!  * Right before erasing, those checks run again, plus a size match.

use crate::{human_size, FormatRequest, FsKind, UsbDrive};
use plist::{Dictionary, Value};
use std::io::Cursor;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const DISKUTIL: &str = "/usr/sbin/diskutil";

fn diskutil(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(DISKUTIL)
        .args(args)
        .output()
        .map_err(|e| format!("Couldn't run diskutil: {}", e))?;
    if !out.status.success() {
        let msg = format!(
            "{} {}",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
        return Err(friendly_error(msg.trim()));
    }
    Ok(out.stdout)
}

fn friendly_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("could not unmount") || lower.contains("resource busy") {
        "macOS couldn't unmount the USB stick because something is using it. Close any Finder \
         windows or apps showing files from the stick, then try again."
            .into()
    } else if lower.contains("write-protected") || lower.contains("read-only") {
        "The USB stick is write-protected. If it has a lock switch, slide it to unlock.".into()
    } else {
        raw.to_string()
    }
}

fn plist_dict(bytes: Vec<u8>) -> Result<Dictionary, String> {
    let v = Value::from_reader(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    v.into_dictionary()
        .ok_or_else(|| "Unexpected diskutil output".to_string())
}

fn info(id: &str) -> Result<Dictionary, String> {
    plist_dict(diskutil(&["info", "-plist", id])?)
}

fn get_str<'a>(d: &'a Dictionary, k: &str) -> Option<&'a str> {
    d.get(k).and_then(Value::as_string)
}
fn get_u64(d: &Dictionary, k: &str) -> Option<u64> {
    d.get(k).and_then(Value::as_unsigned_integer)
}
fn get_bool(d: &Dictionary, k: &str) -> Option<bool> {
    d.get(k).and_then(Value::as_boolean)
}

/// "disk4" -> true, "disk4s1" -> false
fn is_whole_disk_id(id: &str) -> bool {
    id.strip_prefix("disk")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn validate_id(id: &str) -> Result<(), String> {
    if !is_whole_disk_id(id) {
        return Err("Invalid drive selection. Press Refresh and pick the stick again.".into());
    }
    if id == "disk0" || id == "disk1" {
        return Err("Safety check failed: refusing to touch an internal disk.".into());
    }
    Ok(())
}

/// Returns Ok(info) only if this whole disk is an external USB device that isn't the boot disk.
fn safe_usb_info(id: &str) -> Result<Dictionary, String> {
    validate_id(id)?;
    let inf = info(id)?;
    if get_bool(&inf, "Internal").unwrap_or(true) {
        return Err("Safety check failed: that disk is internal, so it was not touched.".into());
    }
    if get_str(&inf, "BusProtocol").unwrap_or("") != "USB" {
        return Err(
            "Safety check failed: that disk isn't a USB stick, so it was not touched.".into(),
        );
    }
    if get_str(&inf, "MountPoint") == Some("/") {
        return Err("Safety check failed: that disk runs macOS.".into());
    }
    Ok(inf)
}

/// Entries of AllDisksAndPartitions from `diskutil list -plist ...`
fn list_entries(args: &[&str]) -> Result<Vec<Dictionary>, String> {
    let dict = plist_dict(diskutil(args)?)?;
    Ok(dict
        .get("AllDisksAndPartitions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_dictionary().cloned())
                .collect()
        })
        .unwrap_or_default())
}

/// First mounted volume on a disk entry: (volume name, mount point)
fn first_mount(entry: &Dictionary) -> Option<(String, String)> {
    if let Some(mp) = get_str(entry, "MountPoint") {
        let name = get_str(entry, "VolumeName").unwrap_or("USB").to_string();
        return Some((name, mp.to_string()));
    }
    entry
        .get("Partitions")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_dictionary)
        .find_map(|p| {
            let mp = get_str(p, "MountPoint")?;
            let name = get_str(p, "VolumeName").unwrap_or("USB");
            Some((name.to_string(), mp.to_string()))
        })
}

// ---------------------------------------------------------------------------
// 1. List
// ---------------------------------------------------------------------------

pub fn list_usb_drives() -> Result<Vec<UsbDrive>, String> {
    let entries = list_entries(&["list", "-plist", "external", "physical"])?;
    let mut drives = Vec::new();
    for e in entries {
        let id = match get_str(&e, "DeviceIdentifier") {
            Some(id) if is_whole_disk_id(id) => id.to_string(),
            _ => continue,
        };
        let inf = match safe_usb_info(&id) {
            Ok(i) => i,
            Err(_) => continue, // silently hide anything that isn't a safe USB stick
        };
        let size = get_u64(&inf, "TotalSize")
            .or_else(|| get_u64(&inf, "Size"))
            .or_else(|| get_u64(&e, "Size"))
            .unwrap_or(0);
        if size == 0 {
            continue;
        }
        let hw_name = get_str(&inf, "MediaName")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "USB Drive".to_string());
        let mount = first_mount(&e);
        let display = match &mount {
            Some((vol, mp)) => format!("{} ({}) — {}", vol, mp, human_size(size)),
            None => format!("{} (not mounted) — {}", hw_name, human_size(size)),
        };
        drives.push(UsbDrive {
            id,
            name: mount.as_ref().map(|m| m.0.clone()).unwrap_or(hw_name),
            display,
            size_bytes: size,
            mount: mount.map(|m| m.1),
        });
    }
    Ok(drives)
}

// ---------------------------------------------------------------------------
// 2. Erase whole disk as MBR + FAT32 / exFAT
// ---------------------------------------------------------------------------

pub fn format_drive(req: &FormatRequest) -> Result<String, String> {
    let id = req.drive_id;
    let inf = safe_usb_info(id)?;
    let size = get_u64(&inf, "TotalSize")
        .or_else(|| get_u64(&inf, "Size"))
        .unwrap_or(0);
    if size != req.expected_size {
        return Err(
            "The USB stick seems to have changed. Press Refresh, pick it again and retry.".into(),
        );
    }

    let node = format!("/dev/{}", id);
    let label = crate::sanitize_label(req.label);
    let personality = match req.fs {
        FsKind::Fat32 => "FAT32",
        FsKind::ExFat => "ExFAT",
    };

    // Free the disk from Finder/Spotlight first; eraseDisk alone can fail on busy sticks.
    let _ = diskutil(&["unmountDisk", "force", &node]);

    match req.partition_size {
        // Capped FAT32: one 32 GB FAT32 partition + the rest left as free space.
        Some(_) => {
            diskutil(&[
                "partitionDisk",
                &node,
                "2",
                "MBR",
                personality,
                &label,
                "32G",
                "Free Space",
                "FREE",
                "R",
            ])?;
        }
        // Whole stick. Always MBRFormat — many Android boxes can't mount GPT.
        None => {
            diskutil(&["eraseDisk", personality, &label, "MBRFormat", &node])?;
        }
    }

    // Find the new partition and wait for macOS to mount it
    // (it may be /Volumes/ANDROID_USB 1 if a same-named volume exists).
    let start = Instant::now();
    let mut tried_mount = false;
    loop {
        let entries = list_entries(&["list", "-plist", &node]).unwrap_or_default();
        if let Some(entry) = entries.first() {
            if let Some((_, mp)) = first_mount(entry) {
                return Ok(mp);
            }
            if !tried_mount && start.elapsed() > Duration::from_secs(3) {
                tried_mount = true;
                if let Some(part) = entry
                    .get("Partitions")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(Value::as_dictionary)
                    .and_then(|p| get_str(p, "DeviceIdentifier"))
                {
                    let _ = diskutil(&["mount", part]);
                }
            }
        }
        if start.elapsed() > Duration::from_secs(15) {
            return Err(
                "The stick was formatted but macOS didn't mount it. Unplug it, plug it back in \
                 and try again."
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Stop Spotlight indexing and fseventsd logging on the stick.
pub fn after_mount(root: &Path) {
    let _ = std::fs::write(root.join(".metadata_never_index"), b"");
    let fsev = root.join(".fseventsd");
    if std::fs::create_dir_all(&fsev).is_ok() {
        let _ = std::fs::write(fsev.join("no_log"), b"");
    }
    let _ = Command::new("/usr/bin/mdutil")
        .args(["-i", "off"])
        .arg(root)
        .output();
}

/// Remove ._* AppleDouble files and .DS_Store clutter before handing the stick back.
pub fn before_eject_cleanup(root: &Path) {
    let _ = Command::new("/usr/sbin/dot_clean")
        .arg("-m")
        .arg(root)
        .output();
    remove_ds_store(root, 0);
}

fn remove_ds_store(dir: &Path, depth: u32) {
    if depth > 4 {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".DS_Store" || name.starts_with("._") {
                let _ = std::fs::remove_file(&p);
            } else if p.is_dir() && !name.starts_with('.') {
                remove_ds_store(&p, depth + 1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Safe eject
// ---------------------------------------------------------------------------

pub fn eject(drive_id: &str, mount: &str) -> Result<String, String> {
    validate_id(drive_id)?;
    if !mount.is_empty() {
        before_eject_cleanup(Path::new(mount));
    }
    let node = format!("/dev/{}", drive_id);
    diskutil(&["eject", &node])?;
    Ok("Ejected! You can now unplug the USB stick.".into())
}
