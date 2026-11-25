// Android USB Prep — frontend (plain JavaScript, no build step).
// Uses Tauri v2's global API (enabled by "withGlobalTauri" in tauri.conf.json).

(() => {
  "use strict";

  const TAURI = window.__TAURI__;
  const invoke = TAURI.core.invoke;
  const listen = TAURI.event.listen;

  const FAT32_MAX_FILE = 4294967295;           // 4 GB single-file limit
  const FAT32_WINDOWS_LIMIT = 32 * 1024 ** 3;  // 32 GiB
  const FAT32_CAP_BYTES = FAT32_WINDOWS_LIMIT - 1024 ** 2;
  const ALLOWED = ["apk", "xapk", "apks"];
  const POLL_MS = 3000;

  const state = {
    platform: "other",
    drives: [],
    selectedId: null,
    files: [], // {path, name, size}
    busy: false,
    result: null,
  };

  const $ = (id) => document.getElementById(id);
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  };

  // ---------------------------------------------------------------- helpers
  // Windows Explorer counts 1 GB = 1024³ bytes, macOS Finder uses 1000³.
  // Match whatever the user's own computer shows.
  function humanSize(bytes) {
    const k = state.platform === "windows" ? 1024 : 1000;
    if (bytes >= k ** 3) { const g = bytes / k ** 3; return (g >= 10 ? g.toFixed(0) : g.toFixed(1)) + " GB"; }
    if (bytes >= k ** 2) return (bytes / k ** 2).toFixed(1) + " MB";
    if (bytes >= k) return (bytes / k).toFixed(0) + " KB";
    return bytes + " bytes";
  }

  function sanitizeLabel(s) {
    let out = "";
    for (const ch of (s || "").trim().toUpperCase()) {
      if (/[A-Z0-9_]/.test(ch)) out += ch;
      else if (ch === " " || ch === "-" || ch === ".") out += "_";
      if (out.length >= 11) break;
    }
    return out.replace(/_/g, "") === "" ? "ANDROID_USB" : out;
  }

  const ext = (p) => (p.split(".").pop() || "").toLowerCase();
  const fsChoice = () => document.querySelector('input[name="fs"]:checked').value;
  const selectedDrive = () => state.drives.find((d) => d.id === state.selectedId) || null;

  function showScreen(name) {
    for (const s of ["setup", "progress", "done", "error"]) {
      $("screen-" + s).classList.toggle("hidden", s !== name);
    }
  }

  async function openFilePicker() {
    const options = {
      multiple: true,
      directory: false,
      title: "Choose APK files",
      filters: [{ name: "Android apps", extensions: ALLOWED }],
    };
    // Prefer the plugin's global JS API; fall back to calling the plugin command directly.
    if (TAURI.dialog && TAURI.dialog.open) return TAURI.dialog.open(options);
    return invoke("plugin:dialog|open", { options });
  }

  // ---------------------------------------------------------------- drives
  async function refreshDrives(manual = false) {
    if (state.busy) return;
    const btn = $("btn-refresh");
    if (manual) { btn.disabled = true; btn.textContent = "⟳ Looking…"; }
    try {
      const drives = await invoke("list_usb_drives");
      const key = (list) => list.map((d) => d.id + ":" + d.sizeBytes + ":" + d.display).join("|");
      const changed = key(drives) !== key(state.drives);
      state.drives = drives;
      // Keep the selection only if the same stick (id + size) is still there.
      const still = drives.find((d) => d.id === state.selectedId);
      if (!still) state.selectedId = null;
      if (!state.selectedId && drives.length === 1) state.selectedId = drives[0].id;
      if (changed || manual) renderDrives();
      renderSummary();
    } catch (e) {
      console.error(e);
    } finally {
      if (manual) { btn.disabled = false; btn.textContent = "⟳ Refresh"; }
    }
  }

  function renderDrives() {
    const list = $("drive-list");
    list.innerHTML = "";
    $("drive-empty").classList.toggle("hidden", state.drives.length > 0);
    for (const d of state.drives) {
      const b = el("button", "drive" + (d.id === state.selectedId ? " selected" : ""));
      b.type = "button";
      b.append(el("span", "icon", d.id === state.selectedId ? "🔘" : "⚪"));
      const txt = el("span");
      txt.append(el("div", "dname", d.display));
      txt.append(el("div", "dsub", "USB stick · " + humanSize(d.sizeBytes)));
      b.append(txt);
      b.addEventListener("click", () => {
        state.selectedId = d.id;
        renderDrives();
        renderSummary();
      });
      list.append(b);
    }
  }

  // ---------------------------------------------------------------- files
  async function addPaths(paths) {
    if (!paths || !paths.length) return;
    const wanted = paths.filter((p) => ALLOWED.includes(ext(p)));
    const skipped = paths.length - wanted.length;
    if (wanted.length) {
      const infos = await invoke("inspect_files", { paths: wanted });
      for (const f of infos) {
        if (!state.files.some((x) => x.path === f.path)) state.files.push(f);
      }
    }
    renderFiles();
    renderSummary();
    if (skipped > 0) flash(`${skipped} file(s) skipped — only .apk, .xapk and .apks files can be added.`);
  }

  let flashUntil = 0;
  function flash(msg) {
    flashUntil = Date.now() + 3500;
    const line = $("space-line");
    line.textContent = msg;
    line.classList.add("bad");
    setTimeout(renderSummary, 3600);
  }

  function renderFiles() {
    const ul = $("file-list");
    ul.innerHTML = "";
    const fat = fsChoice() === "fat32";
    $("drop-hint").classList.toggle("hidden", state.files.length > 0);
    for (const f of state.files) {
      const li = el("li", fat && f.size >= FAT32_MAX_FILE ? "too-big" : "");
      li.append(el("span", "", "📦"));
      li.append(el("span", "fname", f.name));
      li.append(el("span", "fsize", humanSize(f.size)));
      const rm = el("button", "rm", "✕");
      rm.type = "button";
      rm.title = "Remove";
      rm.addEventListener("click", () => {
        state.files = state.files.filter((x) => x.path !== f.path);
        renderFiles();
        renderSummary();
      });
      li.append(rm);
      ul.append(li);
    }
    $("xapk-note").classList.toggle("hidden", !state.files.some((f) => ext(f.name) !== "apk"));
  }

  // ---------------------------------------------------------------- summary + warnings
  function plan() {
    const drive = selectedDrive();
    const fs = fsChoice();
    const cap = $("opt-cap").checked;
    const copies = $("opt-root").checked ? 2 : 1;
    const totalFiles = state.files.reduce((a, f) => a + f.size, 0);
    const needed = totalFiles * copies;
    let usable = 0, capped = false;
    if (drive) {
      capped = fs === "fat32" && cap && drive.sizeBytes > FAT32_WINDOWS_LIMIT;
      usable = Math.floor((capped ? FAT32_CAP_BYTES : drive.sizeBytes) * 0.98);
    }
    const tooBig = fs === "fat32" ? state.files.filter((f) => f.size >= FAT32_MAX_FILE) : [];
    const winFatNoCap = state.platform === "windows" && drive && fs === "fat32" && !cap &&
      drive.sizeBytes > FAT32_WINDOWS_LIMIT;
    return { drive, fs, cap, copies, totalFiles, needed, usable, capped, tooBig, winFatNoCap };
  }

  function renderSummary() {
    const p = plan();
    const line = $("space-line");
    if (Date.now() >= flashUntil) {
      line.classList.remove("bad");
      let t = state.files.length
        ? `${state.files.length} app(s) · ${humanSize(p.totalFiles)}${p.copies === 2 ? " (copied twice)" : ""}`
        : "No apps added — that's fine, the stick will still be set up.";
      if (p.drive) {
        const free = Math.max(0, p.usable - p.needed);
        t += ` · Free space on stick after preparing: ~${humanSize(free)}`;
        if (p.needed > p.usable) line.classList.add("bad");
      }
      line.textContent = t;
    }

    // Warnings with one-click fixes
    const box = $("warnings");
    box.innerHTML = "";
    let blocked = false;

    if (p.tooBig.length) {
      blocked = true;
      const w = el("div", "warning danger");
      w.append(el("div", "", `"${p.tooBig[0].name}" is 4 GB or bigger. The Standard (FAT32) format can't hold files that large.`));
      const a = el("div", "actions");
      const fix = el("button", "btn-link", "Switch to Large Files (exFAT)");
      fix.type = "button";
      fix.addEventListener("click", () => setFs("exfat"));
      a.append(fix);
      w.append(a);
      box.append(w);
    }

    if (p.winFatNoCap) {
      blocked = true;
      const w = el("div", "warning");
      w.append(el("div", "", "Windows can't make a FAT32 stick bigger than 32 GB."));
      const a = el("div", "actions");
      const fix1 = el("button", "btn-link", "Use 32 GB (recommended)");
      fix1.type = "button";
      fix1.addEventListener("click", () => { $("opt-cap").checked = true; renderSummary(); });
      const fix2 = el("button", "btn-link", "Use full size with exFAT");
      fix2.type = "button";
      fix2.addEventListener("click", () => setFs("exfat"));
      a.append(fix1, fix2);
      w.append(a);
      box.append(w);
    }

    if (p.drive && p.needed > p.usable) {
      blocked = true;
      const w = el("div", "warning danger");
      w.append(el("div", "", `Your apps need ${humanSize(p.needed)} but the stick only has about ${humanSize(p.usable)}. Remove some apps or use a bigger stick.`));
      box.append(w);
    }

    if (p.capped && !blocked) {
      const w = el("div", "warning");
      w.append(el("div", "", `Your stick is bigger than 32 GB, so it will be set up as a 32 GB FAT32 drive for best compatibility. (Change this under Advanced options.)`));
      box.append(w);
    }

    $("cap-row").classList.toggle("hidden", p.fs !== "fat32");

    const btn = $("btn-prepare");
    btn.disabled = !p.drive || blocked || state.busy;
    btn.textContent = p.drive ? "Prepare USB Drive" : "Pick a USB stick first";
  }

  function setFs(v) {
    document.querySelector(`input[name="fs"][value="${v}"]`).checked = true;
    $("advanced").open = true;
    renderFiles();
    renderSummary();
  }

  // ---------------------------------------------------------------- confirm + run
  function askConfirm() {
    const p = plan();
    if (!p.drive) return;
    $("modal-text").textContent =
      `Everything currently on '${p.drive.display}' will be erased. Continue?`;
    const fsName = p.fs === "fat32" ? "Standard (FAT32)" : "Large Files (exFAT)";
    $("modal-extra").textContent =
      `Format: ${fsName}${p.capped ? ", 32 GB partition" : ""} · Name: ${sanitizeLabel($("opt-label").value)} · ` +
      (state.files.length ? `${state.files.length} app(s) to copy.` : "No apps to copy.");
    $("modal").classList.remove("hidden");
    $("btn-cancel").focus();
  }

  const STAGE_ORDER = ["format", "folders", "copy", "finish"];
  function setStage(stage) {
    const idx = STAGE_ORDER.indexOf(stage);
    document.querySelectorAll(".stages li").forEach((li) => {
      const i = STAGE_ORDER.indexOf(li.dataset.stage);
      li.classList.toggle("done", stage === "done" || i < idx);
      li.classList.toggle("active", i === idx);
    });
    const fill = $("bar-fill");
    // Formatting has no byte progress — show a moving bar.
    fill.classList.toggle("indeterminate", stage === "format" || stage === "folders" || stage === "finish");
    if (stage === "done") { fill.classList.remove("indeterminate"); fill.style.width = "100%"; }
  }

  async function runPrepare() {
    $("modal").classList.add("hidden");
    const p = plan();
    if (!p.drive) return;
    state.busy = true;
    $("bar-fill").style.width = "0%";
    $("progress-text").textContent = "Starting…";
    setStage("format");
    showScreen("progress");

    const options = {
      driveId: p.drive.id,
      expectedSize: p.drive.sizeBytes,
      fileSystem: p.fs,
      label: sanitizeLabel($("opt-label").value),
      capFat32: p.cap,
      copyToRoot: $("opt-root").checked,
      files: state.files.map((f) => f.path),
    };

    try {
      const result = await invoke("prepare_usb", { options });
      state.result = result;
      showDone(result);
    } catch (e) {
      $("error-text").textContent = String(e);
      showScreen("error");
    } finally {
      state.busy = false;
    }
  }

  function showDone(r) {
    $("done-summary").textContent = r.files.length
      ? `The stick "${r.label}" (${r.mount}) is formatted, the Android folders are created, and these apps were copied to the Download folder and checked byte-for-byte:`
      : `The stick "${r.label}" (${r.mount}) is formatted and the Android folders are created.`;
    const ul = $("done-files");
    ul.innerHTML = "";
    for (const f of r.files) {
      const li = el("li", "", `${f.name} — ${humanSize(f.size)}`);
      li.title = "SHA-256: " + f.sha256;
      ul.append(li);
    }
    $("eject-msg").textContent = "";
    $("btn-eject").disabled = false;
    $("btn-eject").textContent = "Safely Eject USB";
    showScreen("done");
  }

  async function eject() {
    if (!state.result) return;
    const btn = $("btn-eject");
    btn.disabled = true;
    btn.textContent = "Ejecting…";
    try {
      const msg = await invoke("eject_usb", { driveId: state.result.driveId, mount: state.result.mount });
      $("eject-msg").textContent = "✓ " + msg;
      btn.textContent = "Ejected";
    } catch (e) {
      $("eject-msg").textContent = "Couldn't eject: " + e + " — close any windows showing the stick and try again.";
      btn.disabled = false;
      btn.textContent = "Try Eject Again";
    }
  }

  function resetToSetup() {
    state.result = null;
    showScreen("setup");
    refreshDrives(true);
  }

  // ---------------------------------------------------------------- wiring
  async function init() {
    try { state.platform = await invoke("app_platform"); } catch (_) {}

    $("btn-refresh").addEventListener("click", () => refreshDrives(true));
    $("btn-add").addEventListener("click", async () => {
      try {
        const picked = await openFilePicker();
        if (picked) await addPaths(Array.isArray(picked) ? picked : [picked]);
      } catch (e) { console.error(e); }
    });
    $("dropzone").addEventListener("click", (ev) => {
      if (ev.target.closest(".rm")) return;
      if (state.files.length === 0) $("btn-add").click();
    });
    document.querySelectorAll('input[name="fs"]').forEach((r) =>
      r.addEventListener("change", () => { renderFiles(); renderSummary(); }));
    $("opt-cap").addEventListener("change", renderSummary);
    $("opt-root").addEventListener("change", renderSummary);
    $("opt-label").addEventListener("blur", (e) => { e.target.value = sanitizeLabel(e.target.value); });
    $("btn-prepare").addEventListener("click", askConfirm);
    $("btn-cancel").addEventListener("click", () => $("modal").classList.add("hidden"));
    $("btn-confirm").addEventListener("click", runPrepare);
    $("btn-eject").addEventListener("click", eject);
    $("btn-again").addEventListener("click", resetToSetup);
    $("btn-back").addEventListener("click", resetToSetup);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape") $("modal").classList.add("hidden");
    });

    // Native drag & drop (Tauri gives us real file paths).
    const overlay = $("drop-overlay");
    const canDrop = () => !state.busy && !$("screen-setup").classList.contains("hidden");
    await listen("tauri://drag-enter", () => { if (canDrop()) { overlay.classList.remove("hidden"); $("dropzone").classList.add("drag"); } });
    await listen("tauri://drag-leave", () => { overlay.classList.add("hidden"); $("dropzone").classList.remove("drag"); });
    await listen("tauri://drag-drop", (e) => {
      overlay.classList.add("hidden");
      $("dropzone").classList.remove("drag");
      if (canDrop()) addPaths((e.payload && e.payload.paths) || []);
    });

    // Live progress from Rust.
    await listen("prep-status", (e) => {
      const { stage, message } = e.payload;
      setStage(stage);
      if (stage !== "copy") $("progress-text").textContent = message;
    });
    await listen("copy-progress", (e) => {
      const p = e.payload;
      const fill = $("bar-fill");
      fill.classList.remove("indeterminate");
      fill.style.width = p.percent.toFixed(1) + "%";
      const verb = p.phase === "verifying" ? "Checking" : "Copying";
      $("progress-text").textContent =
        `${verb} ${p.fileName} (${p.fileIndex} of ${p.fileCount}) — ${p.percent.toFixed(0)}%`;
    });

    renderFiles();
    await refreshDrives(true);
    setInterval(() => {
      if (!state.busy && $("modal").classList.contains("hidden") && !$("screen-setup").classList.contains("hidden")) {
        refreshDrives(false);
      }
    }, POLL_MS);
  }

  window.addEventListener("DOMContentLoaded", init);
})();
