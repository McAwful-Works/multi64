import { userFacingErrorMessage } from "./user-error.js";
import { normalizeUsbPath, probeSavedCartFolderReachable } from "./saved-cart-path.js";

const { invoke } = window.__TAURI__.core;

const LIST_LIMIT = 500;

/** If the user never changes folder, upload after this much idle time (ms). */
const AUTO_UPLOAD_IDLE_MS = 5000;

/** Seconds shown in the countdown (keep in sync with `AUTO_UPLOAD_IDLE_MS`). */
const AUTO_UPLOAD_SECONDS = Math.max(1, Math.round(AUTO_UPLOAD_IDLE_MS / 1000));

/** @type {ReturnType<typeof setInterval> | null} */
let autoUploadTimerId = null;

/** Set true after opening a subfolder or using ↑ (any navigation). Cancels auto-upload. */
let userHasNavigated = false;

/** Last `cart_serial_list_dir_page` succeeded — cart / SD session is usable. */
let sdReady = false;

/** Picker has files to upload (main init path); drives reconnect polling when SD is missing. */
let pickerSessionActive = false;

/** True while an upload is running: the close button acts as Cancel. */
let uploadInFlight = false;

const STATUS_READY =
  "Ready — use Upload here, or wait for the automatic upload.";
const STATUS_NO_CART =
  "SD card not available. Connect your flash cart (USB); we'll keep checking.";

/** Poll when cart was absent so plugging in refreshes the folder list and can start auto-upload. */
const SD_RECONNECT_POLL_MS = 1500;

/** @type {ReturnType<typeof setInterval> | null} */
let sdReconnectPollId = null;

let sdReconnectInFlight = false;

function stopSdReconnectPolling() {
  if (sdReconnectPollId != null) {
    clearInterval(sdReconnectPollId);
    sdReconnectPollId = null;
  }
}

function syncSdReconnectPolling() {
  if (!pickerSessionActive) {
    stopSdReconnectPolling();
    return;
  }
  if (sdReady) {
    stopSdReconnectPolling();
    return;
  }
  if (sdReconnectPollId != null) {
    return;
  }
  void tryReconnectSd();
  sdReconnectPollId = setInterval(() => {
    void tryReconnectSd();
  }, SD_RECONNECT_POLL_MS);
}

async function tryReconnectSd() {
  if (sdReady || sdReconnectInFlight) return;
  if (document.body.classList.contains("upload-picker-uploading")) return;
  sdReconnectInFlight = true;
  try {
    try {
      await invoke("cart_serial_invalidate_probe_cache");
    } catch {
      /* ignore */
    }
    await loadFolderList();
  } finally {
    sdReconnectInFlight = false;
  }
}

function formatCartDisplay(normalizedPath) {
  const p = normalizeUsbPath(normalizedPath);
  return p ? `/${p}` : "/";
}

function usbParentPath(p) {
  const x = normalizeUsbPath(p);
  if (!x) return "";
  const i = x.lastIndexOf("/");
  return i < 0 ? "" : x.slice(0, i);
}

function joinUsb(parent, name) {
  const b = normalizeUsbPath(parent);
  const n = String(name || "").trim().replace(/\\/g, "/").replace(/^\/+/, "");
  if (!n) return b;
  return b ? `${b}/${n}` : n;
}

/** @type {string} cart folder path (empty = root) — upload destination */
let cartPath = "";

function setError(msg) {
  const el = document.getElementById("upload-picker-error");
  if (!el) return;
  if (msg) {
    el.textContent = msg;
    el.hidden = false;
  } else {
    el.textContent = "";
    el.hidden = true;
  }
}

/** Match `PANE_SHADE_DELAY_MS` in explorer.js — avoid flashing the overlay on fast/failed lists. */
const LIST_SHADE_DELAY_MS = 120;

/** @type {ReturnType<typeof setTimeout> | null} */
let listShadeDelayTimer = null;

let listLoadingDepth = 0;

function clearListShadeDelayTimer() {
  if (listShadeDelayTimer != null) {
    clearTimeout(listShadeDelayTimer);
    listShadeDelayTimer = null;
  }
}

function listShadeSetVisible(visible) {
  const shade = document.getElementById("upload-picker-shade");
  const wrap = document.getElementById("upload-picker-table-wrap");
  if (!shade) return;
  shade.hidden = !visible;
  shade.setAttribute("aria-hidden", visible ? "false" : "true");
  if (wrap) wrap.setAttribute("aria-busy", visible ? "true" : "false");
}

function beginListLoading() {
  listLoadingDepth++;
  if (listLoadingDepth === 1) {
    clearListShadeDelayTimer();
    listShadeDelayTimer = setTimeout(() => {
      listShadeDelayTimer = null;
      if (listLoadingDepth > 0) {
        listShadeSetVisible(true);
      }
    }, LIST_SHADE_DELAY_MS);
  }
}

function endListLoading() {
  clearListShadeDelayTimer();
  listLoadingDepth = Math.max(0, listLoadingDepth - 1);
  if (listLoadingDepth === 0) {
    listShadeSetVisible(false);
  }
}

function setBootLoading(loading) {
  const el = document.getElementById("upload-picker-boot");
  if (!el) return;
  el.hidden = !loading;
  el.setAttribute("aria-hidden", loading ? "false" : "true");
  el.setAttribute("aria-busy", loading ? "true" : "false");
  document.body.classList.toggle("upload-picker-booting", loading);
}

/** @param {string} [overrideText] When set, use instead of ready / no-cart lines. */
function applyIdleStatus(overrideText) {
  const bar = document.getElementById("upload-picker-status-bar");
  const txt = document.getElementById("upload-picker-status-text");
  const track = document.getElementById("upload-picker-status-track");
  const fill = document.getElementById("upload-picker-status-fill");
  if (!bar || !txt) return;
  bar.hidden = false;
  bar.classList.remove(
    "upload-picker-status-bar--success",
    "upload-picker-status-bar--warning",
    "upload-picker-status-bar--busy",
    "upload-picker-status-bar--countdown",
  );
  if (fill) {
    fill.classList.remove("indeterminate");
    fill.style.width = "0";
  }
  if (track) {
    track.classList.add("upload-picker-status-track--inactive");
    track.setAttribute("aria-hidden", "true");
  }
  if (overrideText !== undefined && overrideText !== null && String(overrideText).length) {
    txt.textContent = String(overrideText);
  } else {
    txt.textContent = sdReady ? STATUS_READY : STATUS_NO_CART;
  }
  const cancelAuto = document.getElementById("upload-picker-btn-cancel-auto");
  if (cancelAuto) cancelAuto.hidden = true;
}

/** @param {"idle" | "uploading" | "success" | "warning"} mode — warning = completed with skipped files */
function setUploadStatus(mode, message) {
  const bar = document.getElementById("upload-picker-status-bar");
  const txt = document.getElementById("upload-picker-status-text");
  const track = document.getElementById("upload-picker-status-track");
  const fill = document.getElementById("upload-picker-status-fill");
  if (!bar || !txt) return;
  bar.classList.remove(
    "upload-picker-status-bar--success",
    "upload-picker-status-bar--warning",
    "upload-picker-status-bar--busy",
    "upload-picker-status-bar--countdown",
  );
  if (fill) {
    fill.classList.remove("indeterminate");
    fill.style.width = "";
  }
  if (track) {
    track.classList.add("upload-picker-status-track--inactive");
    track.setAttribute("aria-hidden", "true");
  }
  if (mode === "idle") {
    applyIdleStatus();
    return;
  }
  bar.hidden = false;
  txt.textContent = message;
  if (mode === "uploading") {
    if (track) {
      track.classList.remove("upload-picker-status-track--inactive");
      track.setAttribute("aria-hidden", "false");
    }
    if (fill) {
      fill.classList.add("indeterminate");
      fill.style.width = "";
    }
    bar.classList.add("upload-picker-status-bar--busy");
  } else {
    if (track) {
      track.classList.remove("upload-picker-status-track--inactive");
      track.setAttribute("aria-hidden", "false");
    }
    if (fill) {
      fill.classList.remove("indeterminate");
      fill.style.width = "100%";
    }
    bar.classList.add(
      mode === "warning" ? "upload-picker-status-bar--warning" : "upload-picker-status-bar--success",
    );
  }
}

function setUploadingUi(loading) {
  document.body.classList.toggle("upload-picker-uploading", loading);
  const up = document.getElementById("upload-picker-btn-up");
  if (up) {
    if (loading) {
      up.dataset.prevDisabled = up.disabled ? "1" : "0";
      up.disabled = true;
    } else {
      const prev = up.dataset.prevDisabled;
      delete up.dataset.prevDisabled;
      up.disabled = prev === "1";
      renderPath();
    }
  }
  const wrap = document.getElementById("upload-picker-table-wrap");
  if (wrap) {
    if (loading) wrap.setAttribute("inert", "");
    else wrap.removeAttribute("inert");
  }
}

function renderPath() {
  const el = document.getElementById("upload-picker-path");
  if (el) el.textContent = cartPath ? `/${cartPath}` : "/";
  const up = document.getElementById("upload-picker-btn-up");
  if (up) {
    const atRoot = !normalizeUsbPath(cartPath);
    up.disabled = atRoot;
    up.setAttribute("aria-disabled", atRoot ? "true" : "false");
  }
  const sel = document.getElementById("upload-picker-selection");
  if (sel) {
    const disp = formatCartDisplay(cartPath);
    const line = `Destination: ${disp}`;
    sel.textContent = line;
    sel.title = line;
    sel.hidden = false;
  }
}

async function persistQuickUploadPath() {
  try {
    await invoke("explorer_set_quick_upload_cart_path", { path: cartPath });
  } catch {
    /* ignore */
  }
}

async function persistQuickUploadOverwrite() {
  const ov = document.getElementById("upload-picker-overwrite");
  try {
    await invoke("explorer_set_quick_upload_overwrite", {
      overwrite: ov?.checked === true,
    });
  } catch {
    /* ignore */
  }
}

/** @param {Record<string, unknown> | null} settings */
async function resolveInitialCartPathFromSettings(settings) {
  if (!settings) return "";
  return probeSavedCartFolderReachable(
    invoke,
    normalizeUsbPath(String(settings.quickUploadCartPath ?? "")),
  );
}

function updateAutoUploadIndicator(secondsLeft) {
  const bar = document.getElementById("upload-picker-status-bar");
  const txt = document.getElementById("upload-picker-status-text");
  const track = document.getElementById("upload-picker-status-track");
  const fill = document.getElementById("upload-picker-status-fill");
  if (!bar || !txt) return;
  if (secondsLeft == null || userHasNavigated) {
    const wasCountdown = bar.classList.contains("upload-picker-status-bar--countdown");
    bar.classList.remove("upload-picker-status-bar--countdown");
    if (track) {
      track.classList.add("upload-picker-status-track--inactive");
      track.setAttribute("aria-hidden", "true");
    }
    if (fill) {
      fill.classList.remove("indeterminate");
      fill.style.width = "0%";
    }
    if (wasCountdown) {
      applyIdleStatus();
    }
    return;
  }
  bar.hidden = false;
  bar.classList.remove(
    "upload-picker-status-bar--success",
    "upload-picker-status-bar--warning",
    "upload-picker-status-bar--busy",
  );
  bar.classList.add("upload-picker-status-bar--countdown");
  if (track) {
    track.classList.remove("upload-picker-status-track--inactive");
    track.setAttribute("aria-hidden", "false");
  }
  if (fill) {
    fill.classList.remove("indeterminate");
  }
  const cancelBtn = document.getElementById("upload-picker-btn-cancel-auto");
  if (cancelBtn) cancelBtn.hidden = false;
  txt.innerHTML = `Upload starts in <strong>${secondsLeft}</strong>s — open a subfolder, or press <strong>↑</strong> or <strong>×</strong> to cancel.`;
  if (fill) {
    const elapsed = AUTO_UPLOAD_SECONDS - secondsLeft;
    fill.style.width = `${(elapsed / AUTO_UPLOAD_SECONDS) * 100}%`;
  }
}

function clearAutoUploadTimer() {
  if (autoUploadTimerId != null) {
    clearInterval(autoUploadTimerId);
    autoUploadTimerId = null;
  }
  updateAutoUploadIndicator(null);
}

/** Schedule auto-upload if the user still has not navigated away from the initial folder. */
function scheduleAutoUpload() {
  clearAutoUploadTimer();
  if (userHasNavigated || !sdReady) return;
  let secondsLeft = AUTO_UPLOAD_SECONDS;
  updateAutoUploadIndicator(secondsLeft);
  autoUploadTimerId = setInterval(() => {
    secondsLeft -= 1;
    if (secondsLeft <= 0) {
      clearAutoUploadTimer();
      if (userHasNavigated) return;
      const btn = document.getElementById("upload-picker-btn-upload");
      if (btn?.disabled) return;
      void runUpload();
      return;
    }
    updateAutoUploadIndicator(secondsLeft);
  }, 1000);
}

function cancelAutoUploadBecauseNavigated() {
  userHasNavigated = true;
  clearAutoUploadTimer();
}

function openFolder(name) {
  cancelAutoUploadBecauseNavigated();
  cartPath = joinUsb(cartPath, name);
  renderPath();
  void persistQuickUploadPath();
  void loadFolderList();
}

async function loadFolderList() {
  const tbody = document.getElementById("upload-picker-tbody");
  if (!tbody) return false;
  tbody.innerHTML = "";
  setError("");
  beginListLoading();
  const path = normalizeUsbPath(cartPath);
  let ok = false;
  try {
    const all = [];
    let offset = 0;
    for (;;) {
      const page = await invoke("cart_serial_list_dir_page", {
        path,
        offset,
        limit: LIST_LIMIT,
        fresh: offset === 0,
      });
      all.push(...page.entries);
      offset += page.entries.length;
      if (offset >= page.total || page.entries.length === 0) break;
    }
    const dirs = all.filter((e) => e.isDir);
    dirs.sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" }));
    if (dirs.length === 0) {
      const tr = document.createElement("tr");
      tr.className = "upload-picker-empty";
      const td = document.createElement("td");
      td.className = "col-name";
      td.textContent = "No subfolders — uploads go into this folder.";
      tr.appendChild(td);
      tbody.appendChild(tr);
      sdReady = true;
      ok = true;
    } else {
      for (const e of dirs) {
        const tr = document.createElement("tr");
        tr.className = "upload-picker-folder-row";
        tr.setAttribute("role", "button");
        tr.setAttribute("tabindex", "0");
        tr.dataset.folderName = e.name;
        const td = document.createElement("td");
        td.className = "col-name";
        td.textContent = e.name;
        tr.appendChild(td);
        tr.addEventListener("click", () => openFolder(e.name));
        tr.addEventListener("keydown", (ev) => {
          if (ev.key === "Enter" || ev.key === " ") {
            ev.preventDefault();
            openFolder(e.name);
          }
        });
        tbody.appendChild(tr);
      }
      sdReady = true;
      ok = true;
    }
  } catch (e) {
    setError(userFacingErrorMessage(e, { context: "general" }));
    sdReady = false;
    ok = false;
  } finally {
    endListLoading();
  }
  if (ok && !userHasNavigated) {
    scheduleAutoUpload();
  }
  const bar = document.getElementById("upload-picker-status-bar");
  if (!bar?.classList.contains("upload-picker-status-bar--countdown")) {
    applyIdleStatus();
  }
  const uploadBtn = document.getElementById("upload-picker-btn-upload");
  if (uploadBtn && !document.body.classList.contains("upload-picker-uploading")) {
    uploadBtn.disabled = !sdReady;
  }
  syncSdReconnectPolling();
  return ok;
}

async function init() {
  try {
    let paths;
    try {
      paths = await invoke("upload_picker_get_paths");
    } catch (e) {
      setError(userFacingErrorMessage(e, { context: "general" }));
      applyIdleStatus("Couldn't read the file list for this upload.");
      pickerSessionActive = false;
      stopSdReconnectPolling();
      const uploadBtn0 = document.getElementById("upload-picker-btn-upload");
      if (uploadBtn0) uploadBtn0.disabled = true;
      return;
    }
    if (!paths.length) {
      setError("No files to upload.");
      applyIdleStatus("No files selected.");
      pickerSessionActive = false;
      stopSdReconnectPolling();
      const uploadBtn0 = document.getElementById("upload-picker-btn-upload");
      if (uploadBtn0) uploadBtn0.disabled = true;
      return;
    }
    pickerSessionActive = true;
    let settings = null;
    try {
      settings = await invoke("explorer_get_settings");
    } catch {
      /* use path/overwrite defaults */
    }
    userHasNavigated = false;
    cartPath = await resolveInitialCartPathFromSettings(settings);
    renderPath();
    await loadFolderList();
    const ov = document.getElementById("upload-picker-overwrite");
    if (ov && settings) ov.checked = settings.quickUploadOverwrite === true;
  } finally {
    setBootLoading(false);
  }
}

document.getElementById("upload-picker-btn-up")?.addEventListener("click", () => {
  if (!normalizeUsbPath(cartPath)) return;
  cancelAutoUploadBecauseNavigated();
  cartPath = usbParentPath(cartPath);
  renderPath();
  void persistQuickUploadPath();
  void loadFolderList();
});

document.getElementById("upload-picker-btn-close")?.addEventListener("click", () => {
  if (uploadInFlight) {
    // Mid-transfer this button is "Cancel"; closing the window here would leave the upload
    // running with nothing to report to.
    void invoke("explorer_cancel_operation").catch(() => {});
    const closeBtn = document.getElementById("upload-picker-btn-close");
    if (closeBtn) closeBtn.disabled = true;
    setUploadStatus("uploading", "Cancelling…");
    return;
  }
  clearAutoUploadTimer();
  stopSdReconnectPolling();
  void invoke("upload_picker_close");
});

document.getElementById("upload-picker-btn-cancel-auto")?.addEventListener("click", () => {
  cancelAutoUploadBecauseNavigated();
});

document.getElementById("upload-picker-overwrite")?.addEventListener("change", () => {
  void persistQuickUploadOverwrite();
  if (userHasNavigated) return;
  if (sdReady) {
    scheduleAutoUpload();
  } else {
    clearAutoUploadTimer();
    applyIdleStatus();
  }
});

async function runUpload() {
  const btn = document.getElementById("upload-picker-btn-upload");
  const closeBtn = document.getElementById("upload-picker-btn-close");
  const ow = document.getElementById("upload-picker-overwrite")?.checked === true;
  clearAutoUploadTimer();
  setError("");
  if (btn) btn.disabled = true;
  // The close button becomes a working Cancel for the duration of the transfer. It stays
  // enabled: upload_picker_run now shares the app's cancel state, so this actually stops it.
  if (closeBtn) {
    closeBtn.textContent = "Cancel";
    closeBtn.disabled = false;
  }
  uploadInFlight = true;
  setUploadingUi(true);
  setUploadStatus("uploading", "Uploading to the SD card…");
  const fill = document.getElementById("upload-picker-status-fill");
  const txt = document.getElementById("upload-picker-status-text");
  let unlisten = null;
  try {
    const eventApi = window.__TAURI__?.event;
    if (eventApi?.listen) {
      unlisten = await eventApi.listen("explorer-progress", (e) => {
        const payload = e.payload || {};
        const done = Number(payload.done) || 0;
        const total = Number(payload.total) || 1;
        const pct = total > 0 ? Math.min(100, (done / total) * 100) : 0;
        if (fill) {
          fill.classList.remove("indeterminate");
          fill.style.width = `${pct}%`;
        }
        if (txt) {
          const raw = typeof payload.message === "string" ? payload.message.trim() : "";
          const pctStr = `${Math.round(pct)}%`;
          txt.textContent = raw ? `${raw} (${pctStr})` : `Uploading to the SD card — ${pctStr}`;
        }
      });
    }
    const summary = await invoke("upload_picker_run", {
      cartParent: cartPath,
      overwrite: ow,
    });
    await persistQuickUploadPath();
    const uploaded = Number(summary?.uploaded) || 0;
    const skipped = Number(summary?.skipped) || 0;
    let doneMsg;
    let doneMode;
    if (skipped > 0) {
      doneMode = "warning";
      if (uploaded === 0) {
        doneMsg =
          skipped === 1
            ? "Nothing uploaded — that file is already on the SD card. Turn on \"Overwrite existing\" to replace it."
            : `Nothing uploaded — ${skipped} files are already on the SD card. Turn on \"Overwrite existing\" to replace them.`;
      } else {
        const noun = skipped === 1 ? "file was" : "files were";
        doneMsg = `Upload finished. ${skipped} ${noun} skipped (already on the SD card).`;
      }
    } else {
      doneMode = "success";
      doneMsg = "Upload finished.";
    }
    setUploadStatus(doneMode, doneMsg);
  } catch (e) {
    if (String(e).includes("Cancelled")) {
      setUploadStatus("warning", "Upload cancelled.");
    } else {
      setError(userFacingErrorMessage(e, { context: "general" }));
      setUploadStatus("idle");
    }
  } finally {
    uploadInFlight = false;
    if (typeof unlisten === "function") unlisten();
    if (btn) btn.disabled = !sdReady;
    // Restore the label here, not only on success: a failed or cancelled upload used to leave
    // an enabled button still reading "Cancel".
    if (closeBtn) {
      closeBtn.textContent = "Close";
      closeBtn.disabled = false;
    }
    setUploadingUi(false);
  }
}

document.getElementById("upload-picker-btn-upload")?.addEventListener("click", () => {
  void runUpload();
});

void init();
