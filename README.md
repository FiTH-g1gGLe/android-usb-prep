# Android USB Prep

A dead-simple Windows + Mac app that gets a USB stick ready for an Android TV / OTT box:

1. **Pick your USB stick** – only real USB sticks are shown. Internal drives are never listed.
2. **Add apps** – drag & drop `.apk` / `.xapk` / `.apks` files (optional).
3. **Press "Prepare USB Drive"** – it wipes the stick (MBR + FAT32 by default), creates the
   Android folders, copies your apps into `Download/`, checks every byte (size + SHA-256),
   then offers **Safely Eject USB**.

Folders created on the stick:

```
Android/data   Android/obb   Download   DCIM   Documents   Movies   Music   Pictures
```

---

## Easiest way to get the installers (no coding tools needed)

1. Put this folder in a GitHub repository.
2. Go to the repo's **Actions** tab → **Build apps** → **Run workflow**.
3. Wait ~10–15 minutes, then open **Releases**. A draft release has:
   - Windows: `Android USB Prep_1.0.0_x64_en-US.msi` and `Android USB Prep_1.0.0_x64-setup.exe`
   - Mac: `Android USB Prep_1.0.0_universal.dmg`

## Building on your own computer

You need **Node.js** (LTS) and **Rust** (https://rustup.rs) installed, plus:

- **Windows:** "Microsoft C++ Build Tools" (Desktop development with C++). WebView2 is already on Windows 10/11.
- **Mac:** Xcode Command Line Tools (`xcode-select --install`).

Then in this folder:

```bash
npm install
npm run tauri build
```

Where the results end up:

| Platform | File | Location |
|---|---|---|
| Windows installer | `.msi` | `src-tauri/target/release/bundle/msi/` |
| Windows installer | `-setup.exe` | `src-tauri/target/release/bundle/nsis/` |
| Windows standalone (no install) | `android-usb-prep.exe` | `src-tauri/target/release/` |
| Mac disk image | `.dmg` | `src-tauri/target/release/bundle/dmg/` |
| Mac app | `.app` | `src-tauri/target/release/bundle/macos/` |

Universal Mac build (Intel + Apple Silicon):
`rustup target add aarch64-apple-darwin x86_64-apple-darwin` then
`npm run tauri build -- --target universal-apple-darwin`.

**Testing while developing:** `npm run tauri dev`.
On Windows, open the terminal **as Administrator** first — the app requires admin rights, so a
normal terminal will fail with "The requested operation requires elevation".

---

## Note for Mac users (put this in your download page)

> This app isn't signed by Apple, so the first time you open it, **right-click the app → Open → Open**.
> On macOS 15 Sequoia or newer: try to open it once, then go to **System Settings → Privacy & Security** and click **Open Anyway**.

---

## How it stays safe

- **Windows:** only lists disks where `BusType = USB` and the disk is not a system or boot
  disk (works even for sticks like the SanDisk Extreme that pretend to be "Fixed" disks).
- **Mac:** only lists `diskutil list external physical` disks that are not internal and use USB.
  `disk0`/`disk1` and the disk running macOS are refused outright.
- Right before wiping, the app checks again that it's still a USB disk **and** that its size
  matches what you picked — so swapping sticks mid-way can't erase the wrong one.
- A confirmation box names the exact stick before anything is erased.

## Technical notes

- **Format:** FAT32 + MBR by default. exFAT is available for files over 4 GB. If FAT32 is chosen
  and any file is ≥ 4 GB, the app blocks and offers a one-click switch to exFAT.
- **Big sticks:** Windows can't make FAT32 above 32 GB, so on sticks > 32 GB the FAT32 partition is
  capped at 32 GB (on by default, both platforms). Untick it on a Mac to use the full stick as
  FAT32; on Windows you'll be offered exFAT instead.
- **Windows:** `Clear-Disk -RemoveData -RemoveOEM` removes hidden Linux/EFI/GPT leftovers,
  then `Initialize-Disk -PartitionStyle MBR`, one partition, `Format-Volume`. The partition is
  created without a drive letter and the letter is added after formatting, which stops Explorer's
  "You need to format the disk" pop-up. Then the app waits (up to 10 s) until the drive is writable.
- **Mac:** `diskutil eraseDisk FAT32|ExFAT ANDROID_USB MBRFormat /dev/diskN` (or
  `partitionDisk` for the 32 GB cap). Spotlight/fseventsd are switched off on the stick, files are
  written with a plain read/write loop (not `std::fs::copy`, which copies Mac metadata and creates
  `._` files on FAT drives), and `dot_clean -m` + `.DS_Store` removal run before eject.
- **Eject:** Windows flushes the write cache (`Write-VolumeCache`) and then ejects via the Shell;
  Mac uses `diskutil eject`.
- **Verification note:** the read-back check can be served from the computer's cache, so it
  catches copy errors and short writes but isn't a full hardware test of the stick.

## Project layout

```
src/                     Frontend (plain HTML/CSS/JS — no build step)
  index.html  styles.css  main.js
src-tauri/
  Cargo.toml             Rust dependencies
  build.rs               Registers command permissions + Windows admin manifest
  windows-app-manifest.xml
  tauri.conf.json        App settings (window, bundling, icons)
  capabilities/default.json  What the window may call (Tauri v2 security)
  src/lib.rs             Commands + main workflow
  src/copy.rs            Copy with progress + SHA-256 verify
  src/platform/windows.rs  PowerShell disk backend
  src/platform/macos.rs    diskutil disk backend
.github/workflows/build.yml  Builds Windows + Mac installers on GitHub
```

To change the icon: replace `src-tauri/icons/icon.png` with a 1024×1024 PNG and run
`npx tauri icon src-tauri/icons/icon.png`.
