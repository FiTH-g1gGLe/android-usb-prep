//! Windows backend — PowerShell Storage cmdlets (Get-Disk, Clear-Disk, ...).
//!
//! Safety rules:
//!  * Only disks with BusType = USB that are NOT system and NOT boot disks are
//!    ever listed or touched. (We don't rely on DriveType = Removable, because
//!    some sticks like the SanDisk Extreme report as "Fixed".)
//!  * Right before wiping we re-check BusType/IsSystem/IsBoot AND that the disk
//!    size still matches what the user picked, in case sticks were swapped.

use crate::{human_size, FormatRequest, FsKind, UsbDrive};
use serde_json::Value;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ERR_TAG: &str = "USBPREP_ERROR:";

// ---------------------------------------------------------------------------
// PowerShell runner
// ---------------------------------------------------------------------------

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Runs a PowerShell script via -EncodedCommand (no quoting problems), hidden
/// window, errors captured as `USBPREP_ERROR: message` on stdout.
fn run_ps(body: &str) -> Result<String, String> {
    let script = format!(
        "$ProgressPreference = 'SilentlyContinue'\n\
         $ErrorActionPreference = 'Stop'\n\
         [Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n\
         try {{\n{body}\n}} catch {{\n  Write-Output ('{tag} ' + $_.Exception.Message)\n  exit 1\n}}\n",
        body = body,
        tag = ERR_TAG
    );
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();

    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &base64(&utf16),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Couldn't start PowerShell: {}", e))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if let Some(pos) = stdout.find(ERR_TAG) {
        let msg = stdout[pos + ERR_TAG.len()..].trim().to_string();
        return Err(friendly_error(&msg));
    }
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(friendly_error(if stderr.is_empty() {
            &stdout
        } else {
            &stderr
        }));
    }
    Ok(stdout)
}

fn friendly_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("access denied") || lower.contains("access is denied") {
        "Windows blocked access to the USB stick. Please close this app and open it again, \
         choosing 'Yes' when Windows asks for permission."
            .into()
    } else if lower.contains("write protected") || lower.contains("write-protected") {
        "The USB stick is write-protected. If it has a little lock switch on the side, slide \
         it to unlock, then try again."
            .into()
    } else {
        raw.to_string()
    }
}

/// Pulls `KEY=value` out of script output.
fn tagged(stdout: &str, key: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim().to_string()))
}

fn parse_disk_number(id: &str) -> Result<u32, String> {
    id.trim()
        .parse::<u32>()
        .map_err(|_| "Invalid drive selection. Press Refresh and pick the stick again.".to_string())
}

// ---------------------------------------------------------------------------
// 1. List USB drives
// ---------------------------------------------------------------------------

const LIST_SCRIPT: &str = r#"
$result = @()
$disks = @(Get-Disk | Where-Object { $_.BusType -eq 'USB' -and -not $_.IsSystem -and -not $_.IsBoot -and $_.Size -gt 0 })
foreach ($d in $disks) {
  $letters = @()
  try {
    $letters = @(Get-Partition -DiskNumber $d.Number -ErrorAction Stop |
      Where-Object { $_.DriveLetter -and $_.DriveLetter -ne [char]0 } |
      ForEach-Object { [string]$_.DriveLetter })
  } catch {}
  $result += [pscustomobject]@{
    number  = [int]$d.Number
    name    = [string]$d.FriendlyName
    size    = [uint64]$d.Size
    letters = $letters
  }
}
Write-Output ('USBPREP_JSON=' + (ConvertTo-Json -InputObject @($result) -Compress -Depth 4))
"#;

pub fn list_usb_drives() -> Result<Vec<UsbDrive>, String> {
    let out = run_ps(LIST_SCRIPT)?;
    let json = match tagged(&out, "USBPREP_JSON=") {
        Some(j) if !j.is_empty() => j,
        _ => return Ok(Vec::new()),
    };
    let value: Value = serde_json::from_str(&json).unwrap_or(Value::Array(vec![]));
    // PowerShell may give a single object instead of a 1-item array.
    let items = match value {
        Value::Array(a) => a,
        v @ Value::Object(_) => vec![v],
        _ => vec![],
    };

    let mut drives = Vec::new();
    for item in items {
        let number = match item.get("number").and_then(Value::as_u64) {
            Some(n) => n,
            None => continue,
        };
        let size = item.get("size").and_then(Value::as_u64).unwrap_or(0);
        if size == 0 {
            continue; // empty card reader slot
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "USB Drive".to_string());
        let letters: Vec<String> = match item.get("letters") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            Some(Value::String(s)) => vec![s.clone()],
            _ => vec![],
        };
        let letter_part = if letters.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                letters
                    .iter()
                    .map(|l| format!("{}:", l))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        drives.push(UsbDrive {
            id: number.to_string(),
            display: format!("{}{} — {}", name, letter_part, human_size(size)),
            name,
            size_bytes: size,
            mount: letters.first().map(|l| format!("{}:\\", l)),
        });
    }
    Ok(drives)
}

// ---------------------------------------------------------------------------
// 2. Wipe + MBR + single partition + format
// ---------------------------------------------------------------------------

pub fn format_drive(req: &FormatRequest) -> Result<String, String> {
    let n = parse_disk_number(req.drive_id)?;
    // Label is already sanitized to [A-Z0-9_]{1,11} so it's safe inside quotes.
    let label = crate::sanitize_label(req.label);
    let (fs, mbr) = match req.fs {
        FsKind::Fat32 => ("FAT32", "FAT32"),
        FsKind::ExFat => ("exFAT", "IFS"),
    };
    let size_arg = match req.partition_size {
        Some(bytes) => format!("-Size {}", bytes),
        None => "-UseMaximumSize".to_string(),
    };

    let script = format!(
        r#"
$n = {n}
$d = Get-Disk -Number $n
if ($d.BusType -ne 'USB' -or $d.IsSystem -or $d.IsBoot) {{
  throw 'Safety check failed: that disk is not a removable USB stick, so it was not touched.'
}}
if ([uint64]$d.Size -ne [uint64]{expected}) {{
  throw 'The USB stick seems to have changed. Press Refresh, pick it again and retry.'
}}
if ($d.IsOffline)  {{ Set-Disk -Number $n -IsOffline $false }}
if ($d.IsReadOnly) {{ Set-Disk -Number $n -IsReadOnly $false }}

# Wipe everything: hidden Linux/EFI partitions, GPT tables, OEM partitions.
if ($d.PartitionStyle -ne 'RAW') {{
  Clear-Disk -Number $n -RemoveData -RemoveOEM -Confirm:$false
}}
Start-Sleep -Milliseconds 800

# Android boxes want MBR, never GPT.
$d = Get-Disk -Number $n
if ($d.PartitionStyle -eq 'RAW') {{
  Initialize-Disk -Number $n -PartitionStyle MBR
}} elseif ($d.PartitionStyle -ne 'MBR') {{
  Set-Disk -Number $n -PartitionStyle MBR
}}
Start-Sleep -Milliseconds 500

# Create the partition WITHOUT a drive letter first, so Explorer doesn't pop up
# its "You need to format the disk" dialog before we've formatted it.
$p = New-Partition -DiskNumber $n {size_arg} -MbrType {mbr} -IsActive
$null = Format-Volume -Partition $p -FileSystem {fs} -NewFileSystemLabel '{label}' -Confirm:$false -Force
Start-Sleep -Milliseconds 500

$pn = $p.PartitionNumber
$p = Get-Partition -DiskNumber $n -PartitionNumber $pn
if (-not $p.DriveLetter -or $p.DriveLetter -eq [char]0) {{
  Add-PartitionAccessPath -DiskNumber $n -PartitionNumber $pn -AssignDriveLetter
}}

# Drive-letter mount race: poll up to ~6 seconds for the letter to appear.
$letter = $null
for ($i = 0; $i -lt 20; $i++) {{
  $p = Get-Partition -DiskNumber $n -PartitionNumber $pn
  if ($p.DriveLetter -and $p.DriveLetter -ne [char]0) {{ $letter = [string]$p.DriveLetter; break }}
  Start-Sleep -Milliseconds 300
}}
if (-not $letter) {{ throw 'Windows did not give the USB stick a drive letter. Unplug it, plug it back in and try again.' }}
Write-Output ('USBPREP_LETTER=' + $letter)
"#,
        n = n,
        expected = req.expected_size,
        size_arg = size_arg,
        mbr = mbr,
        fs = fs,
        label = label,
    );

    let out = run_ps(&script)?;
    let letter = tagged(&out, "USBPREP_LETTER=")
        .and_then(|l| l.chars().next())
        .filter(|c| c.is_ascii_alphabetic())
        .ok_or_else(|| "Formatting finished but no drive letter was reported.".to_string())?;
    // Writability polling happens in lib.rs (wait_until_writable).
    Ok(format!("{}:\\", letter.to_ascii_uppercase()))
}

pub fn after_mount(_root: &Path) {}

pub fn before_eject_cleanup(_root: &Path) {}

// ---------------------------------------------------------------------------
// 3. Safe eject
// ---------------------------------------------------------------------------

pub fn eject(_drive_id: &str, mount: &str) -> Result<String, String> {
    let letter = mount
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .ok_or_else(|| "Unknown drive letter.".to_string())?
        .to_ascii_uppercase();

    let script = format!(
        r#"
$L = '{letter}'
# Flush Windows' write cache to the stick first — this is what makes removal safe.
try {{ Write-VolumeCache -DriveLetter $L }} catch {{}}
$shell = New-Object -ComObject Shell.Application
$item = $shell.Namespace(17).ParseName("$($L):")
if ($item) {{ $item.InvokeVerb('Eject') }}
$gone = $false
for ($i = 0; $i -lt 10; $i++) {{
  Start-Sleep -Milliseconds 500
  if (-not (Test-Path "$($L):\")) {{ $gone = $true; break }}
}}
if ($gone) {{ Write-Output 'USBPREP_EJECT=ok' }} else {{ Write-Output 'USBPREP_EJECT=flushed' }}
"#,
        letter = letter
    );
    let out = run_ps(&script)?;
    match tagged(&out, "USBPREP_EJECT=").as_deref() {
        Some("ok") => Ok("Ejected! You can now unplug the USB stick.".into()),
        _ => Ok(
            "All files are saved. Windows didn't fully eject the stick (another window may be \
             using it), but it's now safe to unplug."
                .into(),
        ),
    }
}
