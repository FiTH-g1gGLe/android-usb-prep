//! Byte-level copy with live progress + size and SHA-256 verification.
//!
//! We deliberately do NOT use `std::fs::copy`: on macOS it calls `fcopyfile`
//! with COPYFILE_ALL, which also copies extended attributes — and on a FAT32 /
//! exFAT stick those turn into `._filename` AppleDouble clutter. A plain
//! read/write loop writes only the file's data.

use crate::{FileInfo, VerifiedFile};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const CHUNK: usize = 1024 * 1024; // 1 MiB
const EMIT_EVERY: Duration = Duration::from_millis(100);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CopyProgress {
    /// "copying" or "verifying"
    phase: &'static str,
    file_name: String,
    /// 1-based index of the current job
    file_index: usize,
    file_count: usize,
    /// Bytes processed across copy + verify for the whole run
    done_bytes: u64,
    total_bytes: u64,
    percent: f64,
}

struct Progress<'a> {
    app: &'a AppHandle,
    done: u64,
    total: u64,
    last: Instant,
}

impl<'a> Progress<'a> {
    fn add(
        &mut self,
        n: u64,
        phase: &'static str,
        name: &str,
        idx: usize,
        count: usize,
        force: bool,
    ) {
        self.done += n;
        if force || self.last.elapsed() >= EMIT_EVERY {
            self.last = Instant::now();
            let percent = if self.total == 0 {
                100.0
            } else {
                (self.done as f64 / self.total as f64 * 100.0).min(100.0)
            };
            let _ = self.app.emit(
                "copy-progress",
                CopyProgress {
                    phase,
                    file_name: name.to_string(),
                    file_index: idx,
                    file_count: count,
                    done_bytes: self.done,
                    total_bytes: self.total,
                    percent,
                },
            );
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn copy_one(
    src: &Path,
    dst: &Path,
    prog: &mut Progress,
    name: &str,
    idx: usize,
    count: usize,
) -> Result<String, String> {
    let mut input =
        File::open(src).map_err(|e| format!("Couldn't open '{}': {}", src.display(), e))?;
    let mut output =
        File::create(dst).map_err(|e| format!("Couldn't write '{}': {}", dst.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|e| format!("Error reading '{}': {}", src.display(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n]).map_err(|e| {
            format!(
                "Error writing to the USB stick ({}). It may be full, faulty or was unplugged.",
                e
            )
        })?;
        prog.add(n as u64, "copying", name, idx, count, false);
    }
    output.flush().map_err(|e| e.to_string())?;
    // Make sure the data really reached the stick before we verify.
    output
        .sync_all()
        .map_err(|e| format!("Couldn't finish writing to the USB stick: {}", e))?;
    prog.add(0, "copying", name, idx, count, true);
    Ok(to_hex(&hasher.finalize()))
}

fn hash_file(
    path: &Path,
    prog: &mut Progress,
    name: &str,
    idx: usize,
    count: usize,
) -> Result<String, String> {
    let mut f = File::open(path)
        .map_err(|e| format!("Couldn't re-open '{}' to verify: {}", path.display(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("Error reading back '{}': {}", path.display(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        prog.add(n as u64, "verifying", name, idx, count, false);
    }
    prog.add(0, "verifying", name, idx, count, true);
    Ok(to_hex(&hasher.finalize()))
}

pub fn copy_and_verify(
    app: &AppHandle,
    files: &[FileInfo],
    dest_dirs: &[PathBuf],
) -> Result<Vec<VerifiedFile>, String> {
    // Build the job list: every file × every destination folder.
    let mut jobs: Vec<(usize, PathBuf)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        for d in dest_dirs {
            jobs.push((i, d.join(&f.name)));
        }
    }
    let count = jobs.len();
    let total: u64 = jobs.iter().map(|(i, _)| files[*i].size).sum::<u64>() * 2; // copy + verify

    let mut prog = Progress {
        app,
        done: 0,
        total,
        last: Instant::now()
            .checked_sub(EMIT_EVERY)
            .unwrap_or_else(Instant::now),
    };

    let mut results: Vec<VerifiedFile> = files
        .iter()
        .map(|f| VerifiedFile {
            name: f.name.clone(),
            size: f.size,
            sha256: String::new(),
            destinations: Vec::new(),
        })
        .collect();

    for (n, (i, dst)) in jobs.iter().enumerate() {
        let f = &files[*i];
        let idx = n + 1;
        let src = Path::new(&f.path);

        let src_hash = copy_one(src, dst, &mut prog, &f.name, idx, count)?;

        // Verify size first (cheap), then the SHA-256 of what's actually on the stick.
        let dst_len = std::fs::metadata(dst).map(|m| m.len()).unwrap_or(u64::MAX);
        if dst_len != f.size {
            return Err(format!(
                "Verification failed for '{}': size on stick is {} bytes, expected {}. \
                 The stick may be faulty or fake-capacity.",
                f.name, dst_len, f.size
            ));
        }
        let dst_hash = hash_file(dst, &mut prog, &f.name, idx, count)?;
        if dst_hash != src_hash {
            return Err(format!(
                "Verification failed for '{}': the copy on the stick doesn't match the original. \
                 The stick may be faulty or fake-capacity.",
                f.name
            ));
        }

        let r = &mut results[*i];
        r.sha256 = src_hash;
        r.destinations.push(dst.display().to_string());
    }
    Ok(results)
}
