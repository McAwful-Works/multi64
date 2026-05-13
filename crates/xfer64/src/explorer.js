import { userFacingErrorMessage } from "./user-error.js";
import { normalizeUsbPath, probeSavedCartFolderReachable } from "./saved-cart-path.js";

const { invoke } = window.__TAURI__.core;

const LS_PC = "multi64.explorer.pcPath";
const LS_USB_COM = "multi64.explorer.usbCom";
const LS_SHOW_HIDDEN_CART = "multi64.explorer.showHiddenCart";
const LS_SHOW_HIDDEN_PC = "multi64.explorer.showHiddenPc";
/** multi64d HTTP base (override for non-default listen). */
const LS_DAEMON_LISTEN = "multi64.explorer.daemonListen";
const DEFAULT_DAEMON_LISTEN = "http://127.0.0.1:38765";

/** Normalized cart device when Settings last loaded (`loadExplorerSettings`). Used to skip cart/USB refresh on Save if unchanged. */
let explorerSettingsCartDeviceAtLoad = "auto";

/** @type {{ cartDevice: string, ed64RomLinearBase: number | null, developerMode: boolean } | null} Persisted settings fingerprint; skip USB probe when unchanged (see `explorerSettingsCommitUsbEquivalent`). */
let lastExplorerSettingsCommit = null;

/** @param {unknown} v */
function normalizeEd64BaseSnapshot(v) {
  if (v == null || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n >>> 0 : null;
}

/** @param {Record<string, unknown>} s */
function buildExplorerSettingsCommit(s) {
  return {
    cartDevice: normalizeCartDeviceSetting(s.cartDevice),
    ed64RomLinearBase: normalizeEd64BaseSnapshot(s.ed64RomLinearBase),
    developerMode: !!s.developerMode,
  };
}

/** True if USB hint refresh can be skipped: same commit, or auto→manual cart matching last probe. */
function explorerSettingsCommitUsbEquivalent(prev, next) {
  if (prev == null) return false;
  if (prev.developerMode !== next.developerMode || prev.ed64RomLinearBase !== next.ed64RomLinearBase) {
    return false;
  }
  if (prev.cartDevice === next.cartDevice) return true;
  return (
    prev.cartDevice === "auto" &&
    ((next.cartDevice === "sc64" && lastAutoUsbCartKind === "sc64") ||
      (next.cartDevice === "ed64_beta" && lastAutoUsbCartKind === "ed64"))
  );
}

/** @param {string | undefined} raw */
function normalizeCartDeviceSetting(raw) {
  const r = (raw || "").trim();
  if (r === "ed64_beta") return "ed64_beta";
  if (r === "sc64") return "sc64";
  return "auto";
}

function daemonListenUrl() {
  return localStorage.getItem(LS_DAEMON_LISTEN) || DEFAULT_DAEMON_LISTEN;
}

/**
 * Pause multi64d (release COM) whenever it is running, run `fn`, then resume the daemon.
 * Uses `probe.up` (daemon `/health` OK), not COM matching — so we always interrupt when multi64d is active.
 * @param {() => Promise<unknown>} fn
 * @param {{ confirm?: boolean, actionPane?: "cart" | "pc" }} [opts]
 *   - `confirm: false` (default) — no dialog before release/resume.
 *   - `confirm: true` — ask before releasing the daemon (copy/mkdir/rename).
 *   - `actionPane` — when set with `confirm: true`, clears pane action loading before the multi64d confirm dialog.
 * @returns {Promise<boolean>} `true` if the user cancelled the preflight dialog (only when `confirm` is true).
 */
async function withCartDaemonYield(fn, opts = {}) {
  const confirm = opts.confirm === true;
  const actionPane = opts.actionPane;
  const listen = daemonListenUrl();
  let probe;
  try {
    probe = await invoke("explorer_daemon_probe", { listen });
  } catch {
    await fn();
    return false;
  }
  if (!probe.up) {
    await fn();
    return false;
  }
  if (confirm) {
    if (actionPane) endPaneActionLoading(actionPane);
    const ok = await showExplorerConfirm(
      "The Multi64 bridge (multi64d) is using this cart on the same COM port.\n\nIt will pause while this finishes, then resume. Continue?"
    );
    if (!ok) return true;
  }
  await invoke("explorer_daemon_release", { listen });
  try {
    await fn();
  } finally {
    await invoke("explorer_daemon_resume", { listen }).catch(() => {});
  }
  return false;
}

/**
 * @type {{
 *   cart: {
 *     path: string;
 *     history: string[];
 *     histIndex: number;
 *     selected: Set<string>;
 *     anchorPath: string | null;
 *     listEntries: Array<{ path: string; name: string; isDir: boolean; size: number; modifiedMs?: number; hidden?: boolean }>;
 *   };
 *   pc: {
 *     path: string;
 *     history: string[];
 *     histIndex: number;
 *     selected: Set<string>;
 *     anchorPath: string | null;
 *     listEntries: Array<{ path: string; name: string; isDir: boolean; size: number; modifiedMs?: number; hidden?: boolean }>;
 *   };
 * }}
 */
const state = {
  cart: { path: "", history: [], histIndex: -1, selected: new Set(), anchorPath: null, listEntries: [] },
  pc: { path: "", history: [], histIndex: -1, selected: new Set(), anchorPath: null, listEntries: [] },
};

/** @typedef {"name" | "size" | "modified" | "type"} ExplorerSortKey */
/** @typedef {"asc" | "desc"} ExplorerSortDir */

/** @type {{ cart: { key: ExplorerSortKey, dir: ExplorerSortDir }, pc: { key: ExplorerSortKey, dir: ExplorerSortDir } }} */
const paneSort = {
  cart: { key: "name", dir: "asc" },
  pc: { key: "name", dir: "asc" },
};

/** Rows above/below the viewport kept mounted for smoother scrolling. */
const EXPLORER_VIRTUAL_OVERSCAN = 10;

/** Fallback until the first real row is measured (see `explorerRowHeightPx`). */
const EXPLORER_LIST_ROW_HEIGHT_DEFAULT_PX = 32;

/** Measured from a rendered data row; used for virtual slice + marquee math. */
let explorerRowHeightPx = EXPLORER_LIST_ROW_HEIGHT_DEFAULT_PX;

/** Skip rebuilding tbody when the visible index range is unchanged. */
const lastVirtualRange = { cart: /** @type {{ start: number, end: number } | null} */ (null), pc: null };

function getExplorerRowHeightPx() {
  return explorerRowHeightPx;
}

/** Which file list last had interaction (keyboard shortcuts apply here). */
let focusedPane = "cart";

/** In-app pane-to-pane drag only (JSON in `text/plain`; WebView2 is picky about custom MIME types). Drops from Windows Explorer are ignored. */
const DRAG_INTERNAL_PREFIX = "multi64-explorer:";

/** OS / Explorer file drags — we only accept in-app drags. */
function isExternalFileDrag(dt) {
  if (!dt || !dt.types) return false;
  const types = [...dt.types];
  if (types.includes("Files")) return true;
  if (types.includes("text/uri-list")) return true;
  return false;
}

/**
 * @type {{
 *   pane: "cart" | "pc";
 *   wrap: HTMLElement;
 *   startClientX: number;
 *   startClientY: number;
 *   ctrlKey: boolean;
 *   el: HTMLDivElement;
 *   selRafId: number | null;
 *   pendingClientX: number;
 *   pendingClientY: number;
 * } | null}
 */
let marquee = null;

/** @type {ReturnType<typeof setTimeout> | null} */
let operationHideTimer = null;

const OP_HIDE_MS = 4000;
const NAME_TOOLTIP_DELAY_MS = 1000;

/** Set by Cancel; pairs with `explorer_cancel_operation` for long Rust commands. */
let progressCancelRequested = false;

function resetProgressCancel() {
  progressCancelRequested = false;
}

function requestProgressCancel() {
  progressCancelRequested = true;
  void invoke("explorer_cancel_operation").catch(() => {});
}

function isCancelledBackendError(e) {
  return String(e).includes("Cancelled");
}

/** @type {ReturnType<typeof setTimeout> | null} */
let nameTooltipTimer = null;

/** @type {{ pane: "cart" | "pc", path: string, t: number } | null} */
let renameNameClickArm = null;

/** @type {{ pane: "cart" | "pc", path: string, input: HTMLInputElement, originalName: string } | null} */
let inlineRenameState = null;

/** Min ms between two clicks on the same name so a double-click (open folder) is not mistaken for rename. */
const INLINE_RENAME_MIN_GAP_MS = 280;

function basenameForMessage(p) {
  const s = String(p || "").replace(/\\/g, "/").replace(/^\/+/, "");
  const i = s.lastIndexOf("/");
  return i >= 0 ? s.slice(i + 1) : s || "item";
}

/**
 * Human-readable copy action for the progress line.
 * @param {"fs" | "export" | "import"} mode
 */
function copyActionLabel(mode) {
  if (mode === "import") return "Copying to SD card";
  if (mode === "export") return "Copying from SD card";
  return "Copying to Windows folder";
}

/**
 * @param {Record<string, unknown>} step
 * @param {"fs" | "export" | "import"} mode
 */
function currentItemLabelForCopyStep(step, mode) {
  if (mode === "import") {
    return basenameForMessage(step.cartPath) || "file";
  }
  if (mode === "export") {
    return basenameForMessage(step.cartPath) || basenameForMessage(step.destPc) || "file";
  }
  return basenameForMessage(step.destPc) || basenameForMessage(step.srcPc) || "file";
}

/**
 * @param {Record<string, unknown>} step
 * @param {"fs" | "export" | "import"} mode
 */
function formatCopyProgressMessage(step, mode, i, n) {
  const name = currentItemLabelForCopyStep(step, mode);
  const rem = n - i - 1;
  const action = copyActionLabel(mode);
  if (n <= 1) return `${action} — ${name}`;
  return `${action} — ${name} · ${i + 1} of ${n} (${rem} left)`;
}

/**
 * @param {Record<string, unknown>} step
 * @param {"fs" | "export" | "import"} mode
 */
function formatSkipProgressMessage(step, mode, i, n) {
  const name = currentItemLabelForCopyStep(step, mode);
  const rem = n - i - 1;
  if (n <= 1) return `Skipping — ${name}`;
  return `Skipping — ${name} · ${i + 1} of ${n} (${rem} left)`;
}

function clearOperationHideTimer() {
  if (operationHideTimer != null) {
    clearTimeout(operationHideTimer);
    operationHideTimer = null;
  }
}

/** @param {"cart" | "pc"} pane */
function hideOtherOperationPane(pane) {
  const other = pane === "cart" ? "pc" : "cart";
  const otherRoot = document.getElementById(`explorer-operation-${other}`);
  if (!otherRoot) return;
  otherRoot.classList.add("hidden");
  otherRoot.setAttribute("aria-hidden", "true");
  const otherFill = document.getElementById(`explorer-operation-fill-${other}`);
  if (otherFill) {
    otherFill.style.width = "0";
    otherFill.classList.remove("indeterminate");
  }
  otherRoot.classList.remove("explorer-operation--done", "explorer-operation--error");
}

/**
 * @param {"cart" | "pc"} pane
 * @param {boolean} [determinate] When true, show 0% bar (updates via `explorer-progress` events or manual width).
 */
function showOperationProgress(message, pane, determinate = false) {
  clearOperationHideTimer();
  hideOtherOperationPane(pane);
  const root = document.getElementById(`explorer-operation-${pane}`);
  const text = document.getElementById(`explorer-operation-text-${pane}`);
  const fill = document.getElementById(`explorer-operation-fill-${pane}`);
  if (!root || !text || !fill) return;
  root.classList.remove("hidden");
  root.setAttribute("aria-hidden", "false");
  root.classList.remove("explorer-operation--done", "explorer-operation--error");
  text.textContent = message;
  if (determinate) {
    fill.classList.remove("indeterminate");
    fill.style.width = "0%";
  } else {
    fill.classList.add("indeterminate");
    fill.style.width = "";
  }
}

/** @type {{ cart: number, pc: number }} */
const paneLoadingDepth = { cart: 0, pc: 0 };

/** Ref-counted overlay for copy/delete/rename/mkdir (immediate shade, no folder-load delay). */
const paneActionDepth = { cart: 0, pc: 0 };

/** Avoid flashing the list overlay when directory listing returns almost immediately. */
const PANE_SHADE_DELAY_MS = 120;

/**
 * Chunk size for `fs_list_dir_page` / `cart_serial_list_dir_page` (Rust caches full sorted list per path).
 * Keep ≤ 8192 — backend clamps `limit` to that range.
 */
const EXPLORER_LIST_PAGE_SIZE = 4000;

/**
 * Load full cart directory by paging IPC (avoids one giant payload for huge folders).
 * @param {string} listPath
 * @param {boolean} forceRefresh pass true on F5 so Rust bypasses its cart list cache for that path
 */
async function listCartDirPaged(listPath, forceRefresh) {
  const all = [];
  let offset = 0;
  const lim = EXPLORER_LIST_PAGE_SIZE;
  let exfat = false;
  for (;;) {
    const page = await invoke("cart_serial_list_dir_page", {
      path: listPath,
      offset,
      limit: lim,
      fresh: forceRefresh && offset === 0,
    });
    exfat = page.exfat;
    all.push(...page.entries);
    offset += page.entries.length;
    if (offset >= page.total || page.entries.length === 0) break;
  }
  return { entries: all, exfat };
}

/**
 * @param {string} path
 * @param {boolean} forceRefresh
 */
async function listPcDirPaged(path, forceRefresh) {
  const all = [];
  let offset = 0;
  const lim = EXPLORER_LIST_PAGE_SIZE;
  for (;;) {
    const page = await invoke("fs_list_dir_page", {
      path,
      offset,
      limit: lim,
      fresh: forceRefresh && offset === 0,
    });
    all.push(...page.entries);
    offset += page.entries.length;
    if (offset >= page.total || page.entries.length === 0) break;
  }
  return all;
}

/** @type {{ cart: ReturnType<typeof setTimeout> | null, pc: ReturnType<typeof setTimeout> | null }} */
const paneShadeDelayTimer = { cart: null, pc: null };

/** @type {{ cart: string, pc: string }} */
const paneShadePendingText = { cart: "", pc: "" };

/** @param {"cart" | "pc"} pane */
function clearPaneShadeDelayTimer(pane) {
  const t = paneShadeDelayTimer[pane];
  if (t != null) {
    clearTimeout(t);
    paneShadeDelayTimer[pane] = null;
  }
}

/** @param {"cart" | "pc"} pane */
function paneLoadingSetVisible(pane, visible, text = "Loading…") {
  const shade = document.getElementById(`table-shade-${pane}`);
  const textEl = document.getElementById(`table-shade-text-${pane}`);
  const wrapEl = document.getElementById(`table-wrap-${pane}`);
  if (!shade) return;
  if (textEl) textEl.textContent = text;
  shade.hidden = !visible;
  shade.setAttribute("aria-hidden", visible ? "false" : "true");
  if (wrapEl) wrapEl.setAttribute("aria-busy", visible ? "true" : "false");
}

/** @param {"cart" | "pc"} pane */
function beginPaneLoading(pane, text = "Reading folder…") {
  paneLoadingDepth[pane]++;
  if (paneLoadingDepth[pane] === 1) {
    clearPaneShadeDelayTimer(pane);
    paneShadePendingText[pane] = text;
    paneShadeDelayTimer[pane] = setTimeout(() => {
      paneShadeDelayTimer[pane] = null;
      if (paneLoadingDepth[pane] > 0 && paneActionDepth[pane] === 0) {
        paneLoadingSetVisible(pane, true, paneShadePendingText[pane]);
      }
    }, PANE_SHADE_DELAY_MS);
  } else {
    paneShadePendingText[pane] = text;
  }
}

/** @param {"cart" | "pc"} pane */
function endPaneLoading(pane) {
  clearPaneShadeDelayTimer(pane);
  paneLoadingDepth[pane] = Math.max(0, paneLoadingDepth[pane] - 1);
  if (paneLoadingDepth[pane] === 0 && paneActionDepth[pane] === 0) paneLoadingSetVisible(pane, false);
}

/** @param {"cart" | "pc"} pane */
function beginPaneActionLoading(pane, text = "Working…") {
  clearPaneShadeDelayTimer(pane);
  paneActionDepth[pane]++;
  paneLoadingSetVisible(pane, true, text);
}

/** @param {"cart" | "pc"} pane */
function endPaneActionLoading(pane) {
  if (paneActionDepth[pane] === 0) return;
  paneActionDepth[pane]--;
  if (paneActionDepth[pane] > 0) return;
  if (paneLoadingDepth[pane] > 0) {
    paneLoadingSetVisible(pane, true, paneShadePendingText[pane]);
  } else {
    paneLoadingSetVisible(pane, false);
  }
}

/** Hide the copy/delete progress strip without a completion toast (e.g. empty copy plan). */
function hideOperationProgressPane(pane) {
  clearOperationHideTimer();
  const root = document.getElementById(`explorer-operation-${pane}`);
  const fill = document.getElementById(`explorer-operation-fill-${pane}`);
  if (!root || !fill) return;
  root.classList.add("hidden");
  root.setAttribute("aria-hidden", "true");
  fill.style.width = "0";
  fill.classList.remove("indeterminate");
  root.classList.remove("explorer-operation--done", "explorer-operation--error");
}

/** App bar: COM / daemon / probe feedback (ref-counted for overlapping calls). */
let usbLoadingDepth = 0;

function setUsbLoading(visible, text = "Scanning…") {
  const el = document.getElementById("usb-loading");
  const textEl = document.getElementById("usb-loading-text");
  if (!el) return;
  if (textEl) textEl.textContent = text;
  el.hidden = !visible;
  el.setAttribute("aria-hidden", visible ? "false" : "true");
}

function beginUsbLoading(text = "Scanning…") {
  usbLoadingDepth++;
  if (usbLoadingDepth === 1) setUsbLoading(true, text);
}

function endUsbLoading() {
  usbLoadingDepth = Math.max(0, usbLoadingDepth - 1);
  if (usbLoadingDepth === 0) setUsbLoading(false);
}

/**
 * Listens for backend `explorer-progress` { done, total } while `fn` runs, then sets 100%.
 * @param {"cart" | "pc"} pane
 * @param {() => Promise<unknown>} fn
 */
async function runWithProgress(pane, message, fn) {
  resetProgressCancel();
  showOperationProgress(message, pane, true);
  const fill = document.getElementById(`explorer-operation-fill-${pane}`);
  let unlisten = null;
  let cancelled = false;
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
        const msg = payload.message;
        if (typeof msg === "string" && msg.length) {
          const textEl = document.getElementById(`explorer-operation-text-${pane}`);
          if (textEl) textEl.textContent = msg;
        }
        if (payload.refreshCart) {
          void loadCartPane({ preserveSelection: true, forceRefresh: true });
        }
        if (payload.refreshPc) {
          void loadPcPane({ preserveSelection: true, forceRefresh: true });
        }
      });
    }
    await fn();
  } catch (e) {
    if (isCancelledBackendError(e)) cancelled = true;
    throw e;
  } finally {
    if (typeof unlisten === "function") unlisten();
    if (fill) {
      fill.classList.remove("indeterminate");
      fill.style.width = cancelled ? "0%" : "100%";
    }
  }
}

/** @param {"cart" | "pc"} pane */
function finishOperationProgress(message, isError = false, pane) {
  clearOperationHideTimer();
  const root = document.getElementById(`explorer-operation-${pane}`);
  const text = document.getElementById(`explorer-operation-text-${pane}`);
  const fill = document.getElementById(`explorer-operation-fill-${pane}`);
  if (!root || !text || !fill) return;
  root.classList.remove("hidden");
  root.setAttribute("aria-hidden", "false");
  root.classList.toggle("explorer-operation--done", !isError);
  root.classList.toggle("explorer-operation--error", !!isError);
  text.textContent = message;
  fill.classList.remove("indeterminate");
  fill.style.width = "100%";
  operationHideTimer = setTimeout(() => {
    root.classList.add("hidden");
    root.setAttribute("aria-hidden", "true");
    fill.style.width = "0";
    fill.classList.remove("indeterminate");
    root.classList.remove("explorer-operation--done", "explorer-operation--error");
    operationHideTimer = null;
  }, OP_HIDE_MS);
}

function normalizePath(p) {
  const s = String(p || "").trim();
  if (!s) return "";
  return s.replace(/\//g, "\\");
}

function usbParentPath(p) {
  const x = normalizeUsbPath(p);
  if (!x) return "";
  const i = x.lastIndexOf("/");
  return i < 0 ? "" : x.slice(0, i);
}

function cartRelPathForMkdir(name) {
  const base = normalizeUsbPath(state.cart.path);
  const n = name.trim().replace(/\\/g, "/").replace(/^\/+/, "");
  if (!n) return "";
  return base ? `${base}/${n}` : n;
}

/** @param {string} parentUsbPath */
function cartRelPathForRename(parentUsbPath, newName) {
  const n = newName.trim().replace(/\\/g, "/").replace(/^\/+/, "");
  if (!n) return "";
  const base = normalizeUsbPath(parentUsbPath);
  return base ? `${base}/${n}` : n;
}

/** Parent folder path including trailing backslash (Windows). */
function dirnameWin(fullPath) {
  const s = normalizePath(fullPath);
  const i = s.lastIndexOf("\\");
  if (i < 0) return "";
  return s.slice(0, i + 1);
}

function cancelRenameNameClickArm() {
  renameNameClickArm = null;
}

/** Select the stem (text before the last ".") like Windows Explorer; names starting with "." keep full selection. */
function selectFilenameStemInInput(input) {
  const v = input.value;
  const lastDot = v.lastIndexOf(".");
  if (lastDot <= 0) {
    input.select();
    return;
  }
  input.setSelectionRange(0, lastDot);
}

function removeInlineRenameDocPointerListener() {
  if (!inlineRenameState?.docPointerDown) return;
  document.removeEventListener("pointerdown", inlineRenameState.docPointerDown, true);
}

/** Restores the name cell to a span and clears inline rename state (shared by cancel and commit). */
function restoreInlineRenameNameCellToSpan(originalName) {
  if (!inlineRenameState) return;
  removeInlineRenameDocPointerListener();
  const { input } = inlineRenameState;
  const tr = input.closest("tr");
  const span = document.createElement("span");
  span.className = "explorer-name-text";
  span.textContent = originalName;
  input.replaceWith(span);
  inlineRenameState = null;
  cancelRenameNameClickArm();
  hideNameTooltip();
  if (tr) setupNameTooltipForRow(tr, originalName);
}

/** Drop inline rename state when the given pane's listing is about to be replaced. */
function abandonInlineRenameIfPane(pane) {
  if (inlineRenameState && inlineRenameState.pane === pane) {
    removeInlineRenameDocPointerListener();
    inlineRenameState = null;
  }
  cancelRenameNameClickArm();
}

/**
 * @param {string} fromPath
 * @param {string} newNameTrimmed
 */
async function runRenameCartFromPaths(fromPath, newNameTrimmed) {
  const parent = usbParentPath(normalizeUsbPath(fromPath));
  const toPath = cartRelPathForRename(parent, newNameTrimmed);
  if (!toPath) throw new Error("Invalid name");
  beginPaneActionLoading("cart", "Preparing…");
  try {
    const cancelled = await withCartDaemonYield(
      async () => {
        endPaneActionLoading("cart");
        showOperationProgress(`Renaming on SD card — "${newNameTrimmed}"…`, "cart");
        await invoke("cart_serial_rename_cart", { from: normalizeUsbPath(fromPath), to: toPath });
        await loadCartPane();
      },
      { confirm: true, actionPane: "cart" }
    );
    if (cancelled) return;
    finishOperationProgress(`Renamed to "${newNameTrimmed}".`, false, "cart");
  } finally {
    endPaneActionLoading("cart");
  }
}

/**
 * @param {string} fromPath
 * @param {string} newNameTrimmed
 */
async function runRenamePcFromPaths(fromPath, newNameTrimmed) {
  const parent = dirnameWin(fromPath);
  const toPath = `${parent}${newNameTrimmed}`;
  showOperationProgress(`Renaming on Windows — "${newNameTrimmed}"…`, "pc");
  await invoke("fs_rename", { from: fromPath, to: toPath });
  await loadPcPane();
  finishOperationProgress(`Renamed to "${newNameTrimmed}".`, false, "pc");
}

function cancelInlineRenameRestoreDOMOnly() {
  if (!inlineRenameState) return;
  restoreInlineRenameNameCellToSpan(inlineRenameState.originalName);
}

async function commitInlineRename() {
  if (!inlineRenameState) return;
  const { pane, path: fromPath, input, originalName } = inlineRenameState;
  const newName = input.value.trim();
  if (!newName || newName === originalName) {
    cancelInlineRenameRestoreDOMOnly();
    return;
  }
  restoreInlineRenameNameCellToSpan(originalName);
  try {
    if (pane === "cart") {
      await runRenameCartFromPaths(fromPath, newName);
    } else {
      await runRenamePcFromPaths(fromPath, newName);
    }
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: pane }), true, pane);
    if (pane === "cart") await loadCartPane({ forceRefresh: true });
    else await loadPcPane({ forceRefresh: true });
  }
}

/**
 * Inline rename in the name cell (slow second click on the filename, like Windows Explorer).
 * @param {"cart" | "pc"} pane
 * @param {string} path
 */
function startInlineRename(pane, path) {
  cancelRenameNameClickArm();
  if (inlineRenameState) return;
  ensurePathVisibleInPane(pane, path);
  const tr = findRowInPaneDom(pane, path);
  if (!tr) return;
  const textEl = tr.querySelector(".explorer-name-text");
  if (!textEl) return;
  const originalName = textEl.textContent;
  const input = document.createElement("input");
  input.type = "text";
  input.className = "explorer-name-input";
  input.value = originalName;
  input.setAttribute("aria-label", "Rename");
  textEl.replaceWith(input);
  const onDocPointerDown = (ev) => {
    if (!inlineRenameState) return;
    if (ev.target === inlineRenameState.input) return;
    void commitInlineRename();
  };
  inlineRenameState = { pane, path, input, originalName, docPointerDown: onDocPointerDown };
  document.addEventListener("pointerdown", onDocPointerDown, true);
  hideNameTooltip();
  input.focus();
  selectFilenameStemInInput(input);
  input.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      ev.preventDefault();
      ev.stopPropagation();
      cancelInlineRenameRestoreDOMOnly();
    } else if (ev.key === "Enter") {
      ev.preventDefault();
      ev.stopPropagation();
      input.blur();
    }
  });
  input.addEventListener("blur", () => {
    queueMicrotask(() => void commitInlineRename());
  });
  input.addEventListener("click", (ev) => ev.stopPropagation());
}

/** Cumulative Windows path segments from root (e.g. `C:\`, `C:\Users`, …). */
function splitWindowsPathSegments(fullPath) {
  const p = normalizePath(fullPath);
  if (!p) return [];
  const out = [];
  if (/^[A-Za-z]:/i.test(p)) {
    const drive = p.slice(0, 2);
    let rest = p.slice(2).replace(/^\\+/, "");
    let cum = `${drive}\\`;
    out.push(cum);
    if (!rest) return out;
    for (const part of rest.split("\\").filter(Boolean)) {
      cum = `${cum.replace(/\\+$/, "")}\\${part}`;
      out.push(cum);
    }
  } else if (p.startsWith("\\\\")) {
    const without = p.slice(2);
    const parts = without.split("\\").filter(Boolean);
    if (parts.length === 0) return [p];
    let cum = `\\\\${parts[0]}`;
    out.push(cum);
    for (let i = 1; i < parts.length; i++) {
      cum = `${cum}\\${parts[i]}`;
      out.push(cum);
    }
  } else {
    let cum = "";
    for (const part of p.split("\\").filter(Boolean)) {
      cum = cum ? `${cum}\\${part}` : part;
      out.push(cum);
    }
  }
  const last = out[out.length - 1];
  if (last !== p) out.push(p);
  return out;
}

function fillCartPathSelect(sel) {
  if (!sel) return;
  const inner = normalizeUsbPath(state.cart.path);
  sel.innerHTML = "";
  const optRoot = document.createElement("option");
  optRoot.value = "";
  optRoot.textContent = "/";
  optRoot.title = "Root of SD";
  sel.appendChild(optRoot);
  if (inner) {
    const parts = inner.split("/").filter(Boolean);
    let acc = "";
    for (const seg of parts) {
      acc = acc ? `${acc}/${seg}` : seg;
      const o = document.createElement("option");
      o.value = acc;
      o.textContent = `/${acc}`;
      o.title = `/${acc}`;
      sel.appendChild(o);
    }
  }
  sel.value = inner;
}

function fillPcPathSelect(sel) {
  if (!sel) return;
  const p = normalizePath(state.pc.path);
  sel.innerHTML = "";
  if (!p) {
    const o = document.createElement("option");
    o.value = "";
    o.textContent = "— Select folder —";
    sel.appendChild(o);
    sel.value = "";
    return;
  }
  const segs = splitWindowsPathSegments(state.pc.path);
  for (const seg of segs) {
    const o = document.createElement("option");
    o.value = seg;
    o.textContent = seg;
    o.title = seg;
    sel.appendChild(o);
  }
  sel.value = p;
  if (sel.value !== p) {
    const o = document.createElement("option");
    o.value = p;
    o.textContent = p;
    o.title = p;
    sel.appendChild(o);
    sel.value = p;
  }
}

function formatSize(n) {
  if (n === 0) return "";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatDate(ms) {
  if (ms == null) return "—";
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return "—";
  }
}

function fileTypeLabel(entry) {
  if (entry.isDir) return "Folder";
  const n = entry.name.toLowerCase();
  if (n.endsWith(".z64") || n.endsWith(".n64") || n.endsWith(".v64")) return "ROM";
  if (n.endsWith(".sav") || n.endsWith(".eep")) return "Save";
  return "File";
}

function escapeHtml(s) {
  const d = document.createElement("div");
  d.textContent = s;
  return d.innerHTML;
}

/** @param {"cart" | "pc"} pane */
function showHiddenForPane(pane) {
  const id = pane === "cart" ? "btn-show-hidden-cart" : "btn-show-hidden-pc";
  return document.getElementById(id)?.getAttribute("aria-pressed") === "true";
}

/** @param {Array<{ hidden?: boolean }>} entries */
function filterHiddenEntries(entries, showHidden) {
  if (showHidden) return entries;
  return entries.filter((e) => !e.hidden);
}

/**
 * File explorers usually keep folders together above files; then sort within those buckets.
 * @param {"cart" | "pc"} pane
 * @param {Array<{ path: string, name: string, isDir: boolean, size: number, modifiedMs?: number | null, hidden?: boolean }>} entries
 */
function sortEntriesForPane(pane, entries) {
  const { key, dir } = paneSort[pane];
  const sign = dir === "asc" ? 1 : -1;
  const sorted = [...entries].sort((a, b) => {
    if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;

    if (key === "size") {
      const d = (a.size - b.size) * sign;
      if (d !== 0) return d;
    } else if (key === "modified") {
      const am = a.modifiedMs == null ? null : Number(a.modifiedMs);
      const bm = b.modifiedMs == null ? null : Number(b.modifiedMs);
      if (am == null && bm != null) return 1;
      if (am != null && bm == null) return -1;
      if (am != null && bm != null) {
        const d = (am - bm) * sign;
        if (d !== 0) return d;
      }
    } else if (key === "type") {
      const at = fileTypeLabel(a).toLowerCase();
      const bt = fileTypeLabel(b).toLowerCase();
      const d = at.localeCompare(bt) * sign;
      if (d !== 0) return d;
    }

    return a.name.toLowerCase().localeCompare(b.name.toLowerCase()) * sign;
  });
  return sorted;
}

/** @param {"cart" | "pc"} pane */
function updatePaneSortHeaderIndicators(pane) {
  const tableId = pane === "cart" ? "table-cart" : "table-pc";
  const table = document.getElementById(tableId);
  if (!table) return;
  const { key, dir } = paneSort[pane];
  for (const th of table.querySelectorAll("thead th[data-sort-key]")) {
    const k = th.getAttribute("data-sort-key");
    const active = k === key;
    th.setAttribute("aria-sort", active ? (dir === "asc" ? "ascending" : "descending") : "none");
    th.setAttribute(
      "title",
      `${th.textContent?.trim() || "Column"}${active ? ` (${dir === "asc" ? "ascending" : "descending"})` : ""}`
    );
  }
}

/**
 * @param {"cart" | "pc"} pane
 * @param {ExplorerSortKey} key
 */
function setPaneSort(pane, key) {
  if (paneSort[pane].key === key) {
    paneSort[pane].dir = paneSort[pane].dir === "asc" ? "desc" : "asc";
  } else {
    paneSort[pane].key = key;
    paneSort[pane].dir = "asc";
  }
  updatePaneSortHeaderIndicators(pane);
  state[pane].listEntries = sortEntriesForPane(pane, state[pane].listEntries);
  lastVirtualRange[pane] = null;
  renderExplorerPane(pane);
}

function setupExplorerSortHeaders() {
  for (const pane of ["cart", "pc"]) {
    const tableId = pane === "cart" ? "table-cart" : "table-pc";
    const table = document.getElementById(tableId);
    if (!table) continue;
    for (const th of table.querySelectorAll("thead th[data-sort-key]")) {
      th.setAttribute("tabindex", "0");
      th.setAttribute("role", "button");
      th.addEventListener("click", () => {
        const key = th.getAttribute("data-sort-key");
        if (key === "name" || key === "size" || key === "modified" || key === "type") {
          setPaneSort(pane, key);
        }
      });
      th.addEventListener("keydown", (ev) => {
        if (ev.key !== "Enter" && ev.key !== " ") return;
        ev.preventDefault();
        const key = th.getAttribute("data-sort-key");
        if (key === "name" || key === "size" || key === "modified" || key === "type") {
          setPaneSort(pane, key);
        }
      });
    }
    updatePaneSortHeaderIndicators(pane);
  }
}

function nameCellHtml(iconClass, icon, name) {
  return `<td class="col-name"><span class="explorer-name-row-inner"><span class="${iconClass}">${icon}</span><span class="explorer-name-text">${escapeHtml(
    name
  )}</span></span></td>`;
}

function hideNameTooltip() {
  if (nameTooltipTimer != null) {
    clearTimeout(nameTooltipTimer);
    nameTooltipTimer = null;
  }
  const tip = document.getElementById("explorer-name-tooltip");
  if (tip) {
    tip.hidden = true;
    tip.textContent = "";
    tip.style.visibility = "";
  }
}

const MODAL_TITLE_DEFAULT = "Xfer64";

function isExplorerModalOpen() {
  const root = document.getElementById("explorer-modal-root");
  const ow = document.getElementById("explorer-overwrite-modal");
  const help = document.getElementById("explorer-help-modal");
  const props = document.getElementById("explorer-properties-modal");
  return (
    (root != null && !root.classList.contains("hidden")) ||
    (ow != null && !ow.classList.contains("hidden")) ||
    (help != null && !help.classList.contains("hidden")) ||
    (props != null && !props.classList.contains("hidden"))
  );
}

function isExplorerContextMenuVisible() {
  const menu = document.getElementById("explorer-context-menu");
  return menu != null && !menu.classList.contains("hidden");
}

function hideExplorerContextMenu() {
  const menu = document.getElementById("explorer-context-menu");
  if (!menu) return;
  menu.classList.add("hidden");
  menu.setAttribute("hidden", "");
  menu.setAttribute("aria-hidden", "true");
}

/**
 * @param {"cart" | "pc"} pane
 * @param {number} clientX
 * @param {number} clientY
 * @param {boolean} [blankArea] Right-click on empty list / header (not on a file row)
 */
function showExplorerContextMenu(pane, clientX, clientY, blankArea = false) {
  const menu = document.getElementById("explorer-context-menu");
  if (!menu) return;
  const fileGroup = document.getElementById("explorer-context-menu-group-file");
  const blankGroup = document.getElementById("explorer-context-menu-group-blank");
  if (fileGroup && blankGroup) {
    if (blankArea) {
      fileGroup.classList.add("hidden");
      fileGroup.setAttribute("hidden", "");
      fileGroup.setAttribute("aria-hidden", "true");
      blankGroup.classList.remove("hidden");
      blankGroup.removeAttribute("hidden");
      blankGroup.setAttribute("aria-hidden", "false");
      updateBlankContextMenuItems(pane);
    } else {
      blankGroup.classList.add("hidden");
      blankGroup.setAttribute("hidden", "");
      blankGroup.setAttribute("aria-hidden", "true");
      fileGroup.classList.remove("hidden");
      fileGroup.removeAttribute("hidden");
      fileGroup.setAttribute("aria-hidden", "false");
      updateExplorerContextMenuItems(menu, pane);
    }
  }
  menu.dataset.pane = pane;
  const copyBtn = menu.querySelector('[data-ctx="copy"]');
  if (copyBtn) {
    copyBtn.textContent = pane === "cart" ? "Export to Windows" : "Import to SD card";
  }
  menu.classList.remove("hidden");
  menu.removeAttribute("hidden");
  menu.setAttribute("aria-hidden", "false");
  const pad = 6;
  menu.style.left = "0";
  menu.style.top = "0";
  const w = menu.offsetWidth;
  const h = menu.offsetHeight;
  let left = clientX;
  let top = clientY;
  if (left + w > window.innerWidth - pad) left = window.innerWidth - w - pad;
  if (top + h > window.innerHeight - pad) top = window.innerHeight - h - pad;
  if (left < pad) left = pad;
  if (top < pad) top = pad;
  menu.style.left = `${left}px`;
  menu.style.top = `${top}px`;
}

/**
 * @param {HTMLElement} menu
 * @param {"cart" | "pc"} pane
 */
function updateExplorerContextMenuItems(menu, pane) {
  const sel = state[pane].selected;
  const n = sel.size;
  const single = n === 1;
  let onlyIsDir = false;
  if (single) {
    const onlyPath = [...sel][0];
    const entry = state[pane].listEntries.find((e) => e.path === onlyPath);
    onlyIsDir = entry ? entry.isDir : false;
  }
  const copyBtn = menu.querySelector('[data-ctx="copy"]');
  const delBtn = menu.querySelector('[data-ctx="delete"]');
  const renBtn = menu.querySelector('[data-ctx="rename"]');
  const openBtn = menu.querySelector('[data-ctx="open"]');
  const propBtn = menu.querySelector('[data-ctx="properties"]');
  if (copyBtn) copyBtn.disabled = n === 0;
  if (delBtn) delBtn.disabled = n === 0;
  if (renBtn) renBtn.disabled = !single;
  if (openBtn) openBtn.disabled = !single || !onlyIsDir;
  if (propBtn) propBtn.disabled = !single;
}

/**
 * @param {"cart" | "pc"} pane
 */
function updateBlankContextMenuItems(pane) {
  const menu = document.getElementById("explorer-context-menu");
  const nf = menu?.querySelector('[data-ctx="new-folder"]');
  const fp = menu?.querySelector('[data-ctx="folder-properties"]');
  const noPcFolder = pane === "pc" && !normalizePath(state.pc.path || "");
  if (nf) nf.disabled = noPcFolder;
  if (fp) fp.disabled = noPcFolder;
}

/**
 * @param {"cart" | "pc"} pane
 * @param {string} path
 */
function ensureContextMenuSelection(pane, path) {
  focusedPane = pane;
  cancelRenameNameClickArm();
  const prevSel = new Set(state[pane].selected);
  if (state[pane].selected.has(path) && state[pane].selected.size > 0) {
    applySelectionDiffToDom(pane, prevSel);
    return;
  }
  state[pane].selected.clear();
  state[pane].selected.add(path);
  state[pane].anchorPath = path;
  applySelectionDiffToDom(pane, prevSel);
}

/**
 * @param {HTMLElement} dl
 * @param {{ name: string, path: string, isDir: boolean, size: number, modifiedMs?: number | null, hidden?: boolean }} info
 * @param {"cart" | "pc"} pane
 */
function fillExplorerPropertiesDl(dl, info, pane) {
  dl.innerHTML = "";
  const add = (label, value) => {
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    dl.appendChild(dt);
    dl.appendChild(dd);
  };
  add("Name", info.name);
  add("Location", info.path);
  add("Type", fileTypeLabel({ name: info.name, isDir: info.isDir }));
  let sizeStr = "—";
  if (!info.isDir && typeof info.size === "number") {
    sizeStr = formatSize(info.size);
  }
  add("Size", sizeStr);
  if (pane === "cart") {
    add("Modified", "Not available (USB list)");
  } else {
    const mod =
      info.modifiedMs != null && info.modifiedMs !== undefined
        ? formatDate(/** @type {number} */ (info.modifiedMs))
        : "—";
    add("Modified", mod);
  }
  add("Hidden", info.hidden ? "Yes" : "No");
}

/** @param {"cart" | "pc"} pane */
async function openExplorerPropertiesForPane(pane) {
  if (state[pane].selected.size !== 1) return;
  const path = [...state[pane].selected][0];
  hideExplorerContextMenu();
  const root = document.getElementById("explorer-properties-modal");
  const dl = document.getElementById("explorer-properties-dl");
  if (!root || !dl) return;
  dl.innerHTML = "";
  const dt = document.createElement("dt");
  dt.textContent = "Loading…";
  const dd = document.createElement("dd");
  dd.textContent = "";
  dl.appendChild(dt);
  dl.appendChild(dd);
  root.classList.remove("hidden");
  root.setAttribute("aria-hidden", "false");
  document.body.classList.add("explorer-modal-open");
  try {
    if (pane === "cart") {
      const info = await invoke("cart_serial_path_info", { path: normalizeUsbPath(path) });
      fillExplorerPropertiesDl(dl, info, pane);
    } else {
      const info = await invoke("fs_path_info", { path });
      fillExplorerPropertiesDl(dl, info, pane);
    }
  } catch (e) {
    dl.innerHTML = "";
    const errDt = document.createElement("dt");
    errDt.textContent = "Error";
    const errDd = document.createElement("dd");
    errDd.textContent = userFacingErrorMessage(e, { context: "general" });
    dl.appendChild(errDt);
    dl.appendChild(errDd);
  }
  document.getElementById("explorer-properties-close")?.focus();
}

/** Properties for the folder shown in the path bar (not a selected row). */
async function openExplorerPropertiesForCurrentFolder(pane) {
  hideExplorerContextMenu();
  const root = document.getElementById("explorer-properties-modal");
  const dl = document.getElementById("explorer-properties-dl");
  if (!root || !dl) return;
  dl.innerHTML = "";
  const dt = document.createElement("dt");
  dt.textContent = "Loading…";
  const dd = document.createElement("dd");
  dd.textContent = "";
  dl.appendChild(dt);
  dl.appendChild(dd);
  root.classList.remove("hidden");
  root.setAttribute("aria-hidden", "false");
  document.body.classList.add("explorer-modal-open");
  try {
    if (pane === "cart") {
      const path = normalizeUsbPath(state.cart.path);
      if (!path) {
        fillExplorerPropertiesDl(
          dl,
          {
            name: "(SD root)",
            path: "/",
            isDir: true,
            size: 0,
            hidden: false,
          },
          pane,
        );
      } else {
        const info = await invoke("cart_serial_path_info", { path });
        fillExplorerPropertiesDl(dl, info, pane);
      }
    } else {
      const path = state.pc.path;
      if (!path || !String(path).trim()) {
        dl.innerHTML = "";
        const errDt = document.createElement("dt");
        errDt.textContent = "Error";
        const errDd = document.createElement("dd");
        errDd.textContent =
          "Choose a Windows folder first (Browse … next to the path).";
        dl.appendChild(errDt);
        dl.appendChild(errDd);
        document.getElementById("explorer-properties-close")?.focus();
        return;
      }
      const info = await invoke("fs_path_info", { path });
      fillExplorerPropertiesDl(dl, info, pane);
    }
  } catch (e) {
    dl.innerHTML = "";
    const errDt = document.createElement("dt");
    errDt.textContent = "Error";
    const errDd = document.createElement("dd");
    errDd.textContent = userFacingErrorMessage(e, { context: "general" });
    dl.appendChild(errDt);
    dl.appendChild(errDd);
  }
  document.getElementById("explorer-properties-close")?.focus();
}

function closeExplorerPropertiesModal() {
  const root = document.getElementById("explorer-properties-modal");
  if (!root || root.classList.contains("hidden")) return;
  root.classList.add("hidden");
  root.setAttribute("aria-hidden", "true");
  document.body.classList.remove("explorer-modal-open");
}

/** True when the default browser context menu should stay (text fields, selects, etc.). */
function explorerTargetAllowsNativeContextMenu(target) {
  if (!(target instanceof Element)) return false;
  if (target.closest("input, textarea, select, [contenteditable='true']")) return true;
  const lab = target.closest("label");
  if (lab?.querySelector("input, textarea, select, [contenteditable='true']")) return true;
  return false;
}

let explorerGlobalContextMenuSuppressionInstalled = false;

/** Install early (before async init) so the browser menu is suppressed while panels load. */
function setupExplorerGlobalContextMenuSuppression() {
  if (explorerGlobalContextMenuSuppressionInstalled) return;
  explorerGlobalContextMenuSuppressionInstalled = true;
  document.addEventListener(
    "contextmenu",
    (ev) => {
      if (ev.defaultPrevented) return;
      if (explorerTargetAllowsNativeContextMenu(/** @type {Node} */ (ev.target))) return;
      ev.preventDefault();
    },
    true
  );
}

function setupExplorerContextMenu() {
  const menu = document.getElementById("explorer-context-menu");
  for (const pane of ["cart", "pc"]) {
    document.getElementById(`table-wrap-${pane}`)?.addEventListener("contextmenu", (ev) => {
      onExplorerPaneContextMenu(ev, pane);
    });
  }
  menu?.querySelectorAll("[data-ctx]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const pane = /** @type {"cart" | "pc"} */ (menu.dataset.pane || focusedPane);
      const act = /** @type {HTMLElement} */ (btn).dataset.ctx;
      hideExplorerContextMenu();
      if (act === "open") {
        activateSelectedFolder(pane);
        return;
      }
      if (act === "copy") {
        if (pane === "cart") void copyCartToPcPaths([...state.cart.selected]);
        else void copyPcToCartPaths([...state.pc.selected]);
        return;
      }
      if (act === "delete") {
        if (pane === "cart") void deleteSelectedCart(true);
        else void deleteSelectedPc(true);
        return;
      }
      if (act === "rename") {
        if (pane === "cart") void promptRenameCart();
        else void promptRenamePc();
        return;
      }
      if (act === "properties") void openExplorerPropertiesForPane(pane);
      if (act === "new-folder") {
        if (pane === "cart") void promptMkdirCart();
        else void promptMkdirPc();
        return;
      }
      if (act === "folder-properties") void openExplorerPropertiesForCurrentFolder(pane);
    });
  });
  document.addEventListener(
    "mousedown",
    (ev) => {
      if (!menu || menu.classList.contains("hidden")) return;
      if (menu.contains(/** @type {Node} */ (ev.target))) return;
      hideExplorerContextMenu();
    },
    true
  );
}

/**
 * @param {MouseEvent} ev
 * @param {"cart" | "pc"} pane
 */
function onExplorerPaneContextMenu(ev, pane) {
  if (document.body.classList.contains("explorer-settings-open")) return;
  if (isExplorerModalOpen()) return;
  if (inlineRenameState) return;
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap || !(ev.target instanceof Node) || !wrap.contains(ev.target)) return;

  const el = ev.target instanceof Element ? ev.target : ev.target.parentElement;
  const tr = el?.closest("tbody tr");
  if (tr && isSelectableDataRow(tr) && tr.dataset.path) {
    ev.preventDefault();
    ensureContextMenuSelection(pane, tr.dataset.path);
    showExplorerContextMenu(pane, ev.clientX, ev.clientY, false);
    return;
  }
  ev.preventDefault();
  focusedPane = pane;
  cancelRenameNameClickArm();
  showExplorerContextMenu(pane, ev.clientX, ev.clientY, true);
}

function setupExplorerPropertiesModal() {
  const root = document.getElementById("explorer-properties-modal");
  const backdrop = root?.querySelector(".explorer-modal-backdrop");
  const btnClose = document.getElementById("explorer-properties-close");
  btnClose?.addEventListener("click", () => closeExplorerPropertiesModal());
  backdrop?.addEventListener("click", () => closeExplorerPropertiesModal());
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape" || !root || root.classList.contains("hidden")) return;
      e.preventDefault();
      e.stopPropagation();
      closeExplorerPropertiesModal();
    },
    true
  );
}

/** @param {string} message */
function showExplorerAlert(message) {
  return showExplorerModal({ type: "alert", message });
}

/** @param {string} message */
function showExplorerConfirm(message) {
  return showExplorerModal({ type: "confirm", message });
}

/**
 * @param {string} message
 * @param {{ defaultValue?: string, placeholder?: string, selectFilenameStem?: boolean, disableOkIfEmpty?: boolean }} [opts]
 *   When `selectFilenameStem` is true, only the part before the last "." is selected (Windows-style rename).
 *   When `disableOkIfEmpty` is true, OK stays disabled until the trimmed value is non-empty.
 */
function showExplorerPrompt(message, opts = {}) {
  return showExplorerModal({
    type: "prompt",
    message,
    defaultValue: opts.defaultValue ?? "",
    placeholder: opts.placeholder ?? "",
    selectFilenameStem: opts.selectFilenameStem === true,
    disableOkIfEmpty: opts.disableOkIfEmpty === true,
  });
}

/** Shared options for cart + PC "new folder" prompt (default label, OK disabled when empty). */
const PROMPT_MKDIR_OPTS = { defaultValue: "New Folder", disableOkIfEmpty: true };

function throwUserCopyCancel() {
  const e = new Error("Cancelled");
  e.userCancelledCopy = true;
  throw e;
}

/**
 * @param {{ mode: 'fs'|'export'|'import', step: Record<string, unknown>, totalInPlan: number }} cfg
 * @returns {Promise<'yes'|'skip'|'yesAll'|'skipAll'|'cancel'>}
 */
function showFileReplaceModal(cfg) {
  return new Promise((resolve) => {
    const root = document.getElementById("explorer-overwrite-modal");
    const msgEl = document.getElementById("explorer-overwrite-message");
    const btnYes = document.getElementById("explorer-overwrite-yes");
    const btnSkip = document.getElementById("explorer-overwrite-skip");
    const btnYesAll = document.getElementById("explorer-overwrite-yes-all");
    const btnSkipAll = document.getElementById("explorer-overwrite-skip-all");
    const btnCancel = document.getElementById("explorer-overwrite-cancel");
    const backdrop = root?.querySelector(".explorer-modal-backdrop");
    if (!root || !msgEl || !btnYes || !btnSkip || !btnYesAll || !btnSkipAll || !btnCancel) {
      resolve("cancel");
      return;
    }

    const nPlan = Number(cfg.totalInPlan);
    const singleFile = Number.isFinite(nPlan) && nPlan <= 1;
    btnSkipAll.hidden = singleFile;
    btnYesAll.hidden = singleFile;
    if (singleFile) {
      btnYes.classList.add("primary");
      btnYesAll.classList.remove("primary");
    } else {
      btnYes.classList.remove("primary");
      btnYesAll.classList.add("primary");
    }

    hideNameTooltip();
    const step = cfg.step || {};
    let targetLine = "";
    if (cfg.mode === "import") {
      targetLine = String(step.cartPath || "").trim() || "(unknown)";
    } else {
      targetLine = String(step.destPc || "").trim() || "(unknown)";
    }
    msgEl.textContent = `A file with this name already exists:\n\n${targetLine}\n\nReplace it, skip it, or cancel the copy?`;

    let settled = false;
    const prevFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;

    const cleanup = () => {
      document.removeEventListener("keydown", onKeyDown, true);
      btnYes.removeEventListener("click", onYes);
      btnSkip.removeEventListener("click", onSkip);
      btnYesAll.removeEventListener("click", onYesAll);
      btnSkipAll.removeEventListener("click", onSkipAll);
      btnCancel.removeEventListener("click", onCancel);
      backdrop?.removeEventListener("click", onBackdrop);
    };

    const finish = (v) => {
      if (settled) return;
      settled = true;
      cleanup();
      root.classList.add("hidden");
      root.setAttribute("aria-hidden", "true");
      document.body.classList.remove("explorer-modal-open");
      if (prevFocus) prevFocus.focus();
      resolve(v);
    };

    const onKeyDown = (e) => {
      if (!root || root.classList.contains("hidden")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        finish("cancel");
      }
    };

    const onYes = () => finish("yes");
    const onSkip = () => finish("skip");
    const onYesAll = () => finish("yesAll");
    const onSkipAll = () => finish("skipAll");
    const onCancel = () => finish("cancel");
    const onBackdrop = () => finish("cancel");

    document.addEventListener("keydown", onKeyDown, true);
    btnYes.addEventListener("click", onYes);
    btnSkip.addEventListener("click", onSkip);
    btnYesAll.addEventListener("click", onYesAll);
    btnSkipAll.addEventListener("click", onSkipAll);
    btnCancel.addEventListener("click", onCancel);
    backdrop?.addEventListener("click", onBackdrop);

    root.classList.remove("hidden");
    root.setAttribute("aria-hidden", "false");
    document.body.classList.add("explorer-modal-open");
    requestAnimationFrame(() => btnYes.focus());
  });
}

/**
 * Resolves overwrite prompts, then runs cart ↔ PC copies in **one** backend SD session per direction
 * (`cart_serial_*_copy_batch`) so the COM port opens once for the whole batch.
 * @param {Record<string, unknown>[]} plan
 * @param {'export'|'import'} mode
 */
async function runInteractiveCopyPlan(plan, mode) {
  if (mode !== "export" && mode !== "import") {
    throw new Error("runInteractiveCopyPlan: only export and import are supported");
  }
  const total = plan.reduce((s, st) => s + (Number(st.bytes) || 0), 0) || 1;
  let doneBytes = 0;
  let yesAll = false;
  let skipAll = false;
  const n = plan.length;
  const initialMsg =
    n === 0 ? "" : n === 1 ? formatCopyProgressMessage(plan[0], mode, 0, 1) : `${copyActionLabel(mode)} — ${n} files`;
  await invoke("explorer_emit_progress", { done: 0, total, message: initialMsg });
  const batchPayload = [];
  for (let i = 0; i < plan.length; i++) {
    const step = plan[i];
    let skipThis = false;
    let overwrite = true;
    if (step.conflictIfExists) {
      if (yesAll) {
        overwrite = true;
      } else if (skipAll) {
        skipThis = true;
      } else {
        const choice = await showFileReplaceModal({ mode, step, totalInPlan: n });
        if (choice === "cancel") throwUserCopyCancel();
        if (choice === "yes") overwrite = true;
        if (choice === "skip") skipThis = true;
        if (choice === "yesAll") {
          yesAll = true;
          overwrite = true;
        }
        if (choice === "skipAll") {
          skipAll = true;
          skipThis = true;
        }
      }
    }
    if (skipThis) {
      doneBytes += Number(step.bytes) || 0;
      await invoke("explorer_emit_progress", {
        done: doneBytes,
        total,
        message: formatSkipProgressMessage(step, mode, i, n),
      });
      continue;
    }
    const ow = step.conflictIfExists ? overwrite : true;
    const base = doneBytes;
    const msg = formatCopyProgressMessage(step, mode, i, n);
    const bytes = Number(step.bytes) || 0;
    const common = { overwrite: ow, progressDoneBase: base, progressMessage: msg, bytes };
    batchPayload.push(
      mode === "export"
        ? { ...common, cartPath: step.cartPath, destPcPath: step.destPc }
        : { ...common, srcPcPath: step.srcPc, cartDestPath: step.cartPath }
    );
    doneBytes += bytes;
  }
  if (batchPayload.length === 0) return;
  const batchCmd = mode === "export" ? "cart_serial_export_copy_batch" : "cart_serial_import_copy_batch";
  await invoke(batchCmd, { items: batchPayload, progressTotal: total });
}

/**
 * @param {{ type: 'alert'|'confirm'|'prompt', message: string, title?: string, defaultValue?: string, placeholder?: string, selectFilenameStem?: boolean, disableOkIfEmpty?: boolean }} cfg
 */
function showExplorerModal(cfg) {
  return new Promise((resolve) => {
    const root = document.getElementById("explorer-modal-root");
    const titleEl = document.getElementById("explorer-modal-title");
    const msgEl = document.getElementById("explorer-modal-message");
    const inputEl = document.getElementById("explorer-modal-input");
    const actionsEl = document.getElementById("explorer-modal-actions");
    const btnCancel = document.getElementById("explorer-modal-cancel");
    const btnOk = document.getElementById("explorer-modal-ok");
    const backdrop = root?.querySelector(".explorer-modal-backdrop");
    if (!root || !titleEl || !msgEl || !inputEl || !actionsEl || !btnCancel || !btnOk) {
      if (cfg.type === "confirm") resolve(false);
      else if (cfg.type === "prompt") resolve(null);
      else resolve(undefined);
      return;
    }

    hideNameTooltip();

    titleEl.textContent = cfg.title || MODAL_TITLE_DEFAULT;
    msgEl.textContent = cfg.message;

    const isPrompt = cfg.type === "prompt";
    const isAlert = cfg.type === "alert";
    inputEl.classList.toggle("hidden", !isPrompt);
    actionsEl.classList.toggle("explorer-modal-actions--alert", isAlert);
    btnCancel.classList.toggle("hidden", isAlert);

    const syncOkButtonForEmptyField = () => {
      if (cfg.disableOkIfEmpty !== true) return;
      const empty = inputEl.value.trim() === "";
      btnOk.disabled = empty;
      if (empty) {
        btnOk.setAttribute("tabindex", "-1");
      } else {
        btnOk.removeAttribute("tabindex");
      }
    };

    if (isPrompt) {
      inputEl.value = cfg.defaultValue ?? "";
      inputEl.placeholder = cfg.placeholder ?? "";
      if (cfg.disableOkIfEmpty === true) {
        syncOkButtonForEmptyField();
      } else {
        btnOk.disabled = false;
        btnOk.removeAttribute("tabindex");
      }
    } else {
      inputEl.value = "";
      inputEl.placeholder = "";
      btnOk.disabled = false;
      btnOk.removeAttribute("tabindex");
    }

    let settled = false;
    /** @type {HTMLElement | null} */
    const prevFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;

    /** @type {() => void} */
    let cleanup = () => {};

    const finish = (value) => {
      if (settled) return;
      settled = true;
      cleanup();
      root.classList.add("hidden");
      root.setAttribute("aria-hidden", "true");
      document.body.classList.remove("explorer-modal-open");
      if (prevFocus) prevFocus.focus();
      resolve(value);
    };

    const onKeyDown = (e) => {
      if (!root || root.classList.contains("hidden")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (cfg.type === "alert") finish(undefined);
        else if (cfg.type === "confirm") finish(false);
        else finish(null);
      } else if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        if (cfg.type === "alert") finish(undefined);
        else if (cfg.type === "confirm") finish(e.target !== btnCancel);
        else if (e.target === btnCancel) finish(null);
        else if (cfg.type === "prompt") {
          if (cfg.disableOkIfEmpty === true && !inputEl.value.trim()) return;
          finish(inputEl.value);
        }
      }
    };

    const onOk = () => {
      if (cfg.type === "prompt") {
        if (cfg.disableOkIfEmpty === true && !inputEl.value.trim()) return;
        finish(inputEl.value);
      } else if (cfg.type === "confirm") finish(true);
      else finish(undefined);
    };

    const onCancel = () => {
      if (cfg.type === "prompt") finish(null);
      else if (cfg.type === "confirm") finish(false);
      else finish(undefined);
    };

    const onBackdrop = () => {
      if (cfg.type === "alert") finish(undefined);
      else if (cfg.type === "confirm") finish(false);
      else finish(null);
    };

    cleanup = () => {
      document.removeEventListener("keydown", onKeyDown, true);
      btnOk.removeEventListener("click", onOk);
      btnCancel.removeEventListener("click", onCancel);
      backdrop?.removeEventListener("click", onBackdrop);
      if (isPrompt && cfg.disableOkIfEmpty === true) {
        inputEl.removeEventListener("input", syncOkButtonForEmptyField);
      }
      btnOk.removeAttribute("tabindex");
    };

    document.addEventListener("keydown", onKeyDown, true);
    if (isPrompt && cfg.disableOkIfEmpty === true) {
      inputEl.addEventListener("input", syncOkButtonForEmptyField);
    }
    btnOk.addEventListener("click", onOk);
    btnCancel.addEventListener("click", onCancel);
    backdrop?.addEventListener("click", onBackdrop);

    root.classList.remove("hidden");
    root.setAttribute("aria-hidden", "false");
    document.body.classList.add("explorer-modal-open");

    requestAnimationFrame(() => {
      if (isPrompt) {
        inputEl.focus();
        if (cfg.selectFilenameStem === true) selectFilenameStemInInput(inputEl);
        else inputEl.select();
      } else {
        btnOk.focus();
      }
    });
  });
}

function showNameTooltip(fullName, anchorRect) {
  const tip = document.getElementById("explorer-name-tooltip");
  if (!tip) return;
  tip.textContent = fullName;
  tip.hidden = false;
  tip.style.visibility = "hidden";
  tip.style.left = "0";
  tip.style.top = "0";
  tip.style.maxWidth = `${Math.min(560, window.innerWidth - 16)}px`;
  const tw = tip.offsetWidth;
  const th = tip.offsetHeight;
  const margin = 8;
  let left = anchorRect.left + anchorRect.width / 2 - tw / 2;
  left = Math.max(margin, Math.min(left, window.innerWidth - tw - margin));
  let top = anchorRect.bottom + 4;
  if (top + th > window.innerHeight - margin) {
    top = anchorRect.top - th - 4;
  }
  if (top < margin) top = margin;
  tip.style.left = `${left}px`;
  tip.style.top = `${top}px`;
  tip.style.visibility = "visible";
}

function setupNameTooltipForRow(tr, fullName) {
  const td = tr.querySelector("td.col-name");
  const textEl = tr.querySelector(".explorer-name-text");
  if (!td || !textEl) return;

  td.addEventListener("pointerenter", () => {
    hideNameTooltip();
    nameTooltipTimer = setTimeout(() => {
      nameTooltipTimer = null;
      if (textEl.scrollWidth <= textEl.clientWidth + 1) return;
      showNameTooltip(fullName, td.getBoundingClientRect());
    }, NAME_TOOLTIP_DELAY_MS);
  });
  td.addEventListener("pointerleave", hideNameTooltip);
  td.addEventListener("pointercancel", hideNameTooltip);
}

/** Sniff test: error text that asks the user to configure / scan the EverDrive SD linear base (see `cart_serial_sd.rs`). */
const ED64_SD_BASE_HELP_SNIPPET = "Scan for SD base";

const ED64_SD_BASE_SCAN_LABEL = "Scanning for SD base";
/** Cart footer + progress strip while `runEd64LinearBaseScan` runs. */
const ED64_SD_BASE_SCAN_STATUS_MSG = `${ED64_SD_BASE_SCAN_LABEL} (may take a minute)…`;

function isCartSdBaseScanProgressBarActive() {
  const root = document.getElementById("explorer-operation-cart");
  if (!root || root.classList.contains("hidden")) return false;
  const t = (document.getElementById("explorer-operation-text-cart")?.textContent || "").trim();
  return t.includes(ED64_SD_BASE_SCAN_LABEL);
}

/**
 * @param {{ preserveSelection?: boolean, forceRefresh?: boolean }} [opts]
 * When `preserveSelection` is true, selection is restored for paths that still exist (e.g. during long operations).
 * `forceRefresh` — bypass Rust cart list cache on the first page (e.g. F5).
 */
async function loadCartPane(opts = {}) {
  beginPaneLoading("cart", "Reading SD card…");
  try {
    const preserveSelection = opts.preserveSelection === true;
    const forceRefresh = opts.forceRefresh === true;
    const savedSel = preserveSelection ? new Set(state.cart.selected) : null;
    const savedAnchor = preserveSelection ? state.cart.anchorPath : null;

    const pathSel = document.getElementById("addr-cart-select");
    const tbody = document.getElementById(paneTbodyId("cart"));
    const statusMeta = document.getElementById("status-cart-meta");

    fillCartPathSelect(pathSel);
    const cancelled = await withCartDaemonYield(async () => {
      try {
        const listPath = normalizeUsbPath(state.cart.path);
        const { entries, exfat } = await listCartDirPaged(listPath, forceRefresh);
        const visible = sortEntriesForPane("cart", filterHiddenEntries(entries, showHiddenForPane("cart")));
        if (!preserveSelection) {
          state.cart.selected.clear();
          state.cart.anchorPath = null;
          const wrapCart = document.getElementById("table-wrap-cart");
          if (wrapCart) wrapCart.scrollTop = 0;
        }
        abandonInlineRenameIfPane("cart");
        lastVirtualRange.cart = null;
        state.cart.listEntries = visible;
        if (preserveSelection && savedSel) {
          restorePaneSelectionAfterLoad("cart", visible, savedSel, savedAnchor, false);
        }
        renderExplorerPane("cart");
        const vol = exfat ? "exFAT" : "FAT";
        if (statusMeta) statusMeta.textContent = `${visible.length} item(s) · ${vol}`;
      } catch (err) {
        abandonInlineRenameIfPane("cart");
        state.cart.listEntries = [];
        lastVirtualRange.cart = null;
        tbody.replaceChildren();
        if (statusMeta) {
          const raw = userFacingErrorMessage(err, { context: "cart" });
          const longEd64NoBase =
            typeof raw === "string" && raw.includes(ED64_SD_BASE_HELP_SNIPPET);
          if (
            ed64LinearScanRunning ||
            (longEd64NoBase && isCartSdBaseScanProgressBarActive())
          ) {
            statusMeta.textContent = ED64_SD_BASE_SCAN_STATUS_MSG;
          } else {
            statusMeta.textContent = raw;
          }
        }
      }
    });
    if (cancelled) return;
  } finally {
    endPaneLoading("cart");
    updateExplorerDeleteToolbarButtons();
  }
}

/**
 * @param {{ preserveSelection?: boolean, forceRefresh?: boolean }} [opts]
 * `forceRefresh` — bypass Rust PC list cache on the first page (e.g. F5).
 */
async function loadPcPane(opts = {}) {
  beginPaneLoading("pc", "Reading folder…");
  try {
    const preserveSelection = opts.preserveSelection === true;
    const forceRefresh = opts.forceRefresh === true;
    const savedSel = preserveSelection ? new Set(state.pc.selected) : null;
    const savedAnchor = preserveSelection ? state.pc.anchorPath : null;

    const p = state.pc.path;
    const pathSel = document.getElementById("addr-pc-select");
    const tbody = document.getElementById(paneTbodyId("pc"));
    const statusMeta = document.getElementById("status-pc-meta");

    fillPcPathSelect(pathSel);
    if (!p) {
      abandonInlineRenameIfPane("pc");
      state.pc.listEntries = [];
      lastVirtualRange.pc = null;
      tbody.replaceChildren();
      if (statusMeta) {
        statusMeta.textContent = "Choose a Windows folder (use Browse … next to the path).";
      }
      return;
    }
    try {
      const entries = await listPcDirPaged(p, forceRefresh);
      const visible = sortEntriesForPane("pc", filterHiddenEntries(entries, showHiddenForPane("pc")));
      if (!preserveSelection) {
        state.pc.selected.clear();
        state.pc.anchorPath = null;
        const wrapPc = document.getElementById("table-wrap-pc");
        if (wrapPc) wrapPc.scrollTop = 0;
      }
      abandonInlineRenameIfPane("pc");
      lastVirtualRange.pc = null;
      state.pc.listEntries = visible;
      if (preserveSelection && savedSel) {
        restorePaneSelectionAfterLoad("pc", visible, savedSel, savedAnchor, false);
      }
      renderExplorerPane("pc");
      if (statusMeta) statusMeta.textContent = `${visible.length} item(s)`;
      localStorage.setItem(LS_PC, p);
    } catch (err) {
      abandonInlineRenameIfPane("pc");
      state.pc.listEntries = [];
      lastVirtualRange.pc = null;
      tbody.replaceChildren();
      if (statusMeta) statusMeta.textContent = userFacingErrorMessage(err, { context: "pc" });
    }
  } finally {
    endPaneLoading("pc");
    updateExplorerDeleteToolbarButtons();
  }
}

/**
 * Refresh cart and Windows panes concurrently so neither blocks the other.
 * @param {{ preserveSelection?: boolean, forceRefresh?: boolean }} [opts]
 * `forceRefresh` is passed through to both panes (bypass Rust list caches on the first page).
 */
async function loadBothPanes(opts = {}) {
  await Promise.all([loadCartPane(opts), loadPcPane(opts)]);
}

/** Restore last SD folder from settings (same key as quick upload). */
async function applySavedCartFolderFromSettings() {
  let settings;
  try {
    settings = await invoke("explorer_get_settings");
  } catch {
    return;
  }
  const path = await probeSavedCartFolderReachable(
    invoke,
    normalizeUsbPath(String(settings?.quickUploadCartPath ?? "")),
  );
  if (!path) return;
  state.cart.path = path;
  state.cart.history = [path];
  state.cart.histIndex = 0;
}

function onRowClick(pane, path, ev) {
  focusedPane = pane;
  if (ev.target.closest(".explorer-name-input")) return;

  const sel = state[pane].selected;
  const prevSel = new Set(sel);

  if (ev.detail >= 2) {
    cancelRenameNameClickArm();
  }

  const paths = getOrderedPaths(pane);
  const idx = paths.indexOf(path);
  const anchor = state[pane].anchorPath;

  if (ev.shiftKey && anchor != null && anchor !== "") {
    cancelRenameNameClickArm();
    const anchorIdx = paths.indexOf(anchor);
    if (anchorIdx >= 0 && idx >= 0) {
      const [lo, hi] = anchorIdx < idx ? [anchorIdx, idx] : [idx, anchorIdx];
      sel.clear();
      for (let i = lo; i <= hi; i++) sel.add(paths[i]);
      applySelectionDiffToDom(pane, prevSel);
      return;
    }
  }

  if (ev.ctrlKey || ev.metaKey) {
    cancelRenameNameClickArm();
    if (sel.has(path)) sel.delete(path);
    else sel.add(path);
    state[pane].anchorPath = path;
  } else {
    if (
      ev.button === 0 &&
      ev.detail === 1 &&
      ev.target.closest(".explorer-name-text") &&
      sel.size === 1 &&
      sel.has(path)
    ) {
      const now = Date.now();
      const arm = renameNameClickArm;
      if (!arm || arm.pane !== pane || arm.path !== path) {
        renameNameClickArm = { pane, path, t: now };
        return;
      }
      if (now - arm.t <= INLINE_RENAME_MIN_GAP_MS) return;
      cancelRenameNameClickArm();
      startInlineRename(pane, path);
      return;
    }
    cancelRenameNameClickArm();
    sel.clear();
    sel.add(path);
    state[pane].anchorPath = path;
    renameNameClickArm = { pane, path, t: Date.now() };
  }
  applySelectionDiffToDom(pane, prevSel);
}

function pathsForDrag(pane, path) {
  const sel = state[pane].selected;
  if (sel.size && sel.has(path)) return [...sel];
  return [path];
}

function bindRowDrag(pane, tr, path) {
  tr.addEventListener("dragstart", (ev) => {
    const paths = pathsForDrag(pane, path);
    const payload = JSON.stringify({ sourcePane: pane, paths });
    ev.dataTransfer.setData("text/plain", `${DRAG_INTERNAL_PREFIX}${payload}`);
    ev.dataTransfer.effectAllowed = "copy";
  });
}

function bindRowDragOver(tr) {
  tr.addEventListener("dragover", (ev) => {
    if (isExternalFileDrag(ev.dataTransfer)) {
      ev.dataTransfer.dropEffect = "none";
      return;
    }
    ev.preventDefault();
    ev.dataTransfer.dropEffect = "copy";
  });
}

/** Highlight a folder row when dragging over it so drops can target that path (not only the current directory). */
function bindDirectoryDropTargetRow(tr) {
  tr.addEventListener("dragenter", (ev) => {
    if (isExternalFileDrag(ev.dataTransfer)) return;
    ev.preventDefault();
    tr.classList.add("drag-over-drop-target");
  });
  tr.addEventListener("dragover", (ev) => {
    if (isExternalFileDrag(ev.dataTransfer)) {
      ev.dataTransfer.dropEffect = "none";
      return;
    }
    ev.preventDefault();
    ev.dataTransfer.dropEffect = "copy";
  });
  tr.addEventListener("dragleave", (ev) => {
    if (!tr.contains(ev.relatedTarget)) tr.classList.remove("drag-over-drop-target");
  });
}

/** @param {"cart" | "pc"} pane */
function paneTbodyId(pane) {
  return pane === "cart" ? "tbody-cart" : "tbody-pc";
}

function getOrderedPaths(pane) {
  return state[pane].listEntries.map((e) => e.path);
}

function findRowInPaneDom(pane, path) {
  const tbody = document.getElementById(paneTbodyId(pane));
  if (!tbody) return null;
  for (const tr of tbody.querySelectorAll("tr[data-path]")) {
    if (tr.dataset.path === path) return /** @type {HTMLTableRowElement} */ (tr);
  }
  return null;
}

/**
 * @param {"cart" | "pc"} pane
 * @param {number} index
 */
function scrollVirtualRowIntoView(pane, index) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;
  const thead = wrap.querySelector("thead");
  if (!thead) return;
  const theadH = thead.offsetHeight;
  const rowH = getExplorerRowHeightPx();
  const n = state[pane].listEntries.length;
  if (!n) return;
  const targetScroll = theadH + index * rowH - wrap.clientHeight / 2 + rowH / 2;
  const maxScroll = Math.max(0, wrap.scrollHeight - wrap.clientHeight);
  wrap.scrollTop = Math.max(0, Math.min(targetScroll, maxScroll));
}

/** Scroll so `path` is in the virtual window, then re-render rows for that scroll position. */
function ensurePathVisibleInPane(pane, path) {
  const entries = state[pane].listEntries;
  const idx = entries.findIndex((e) => e.path === path);
  if (idx < 0) return;
  lastVirtualRange[pane] = null;
  scrollVirtualRowIntoView(pane, idx);
  renderExplorerPane(pane);
}

/**
 * @param {"cart" | "pc"} pane
 * @returns {{ start: number, end: number }}
 */
function computeVisibleRange(pane) {
  const entries = state[pane].listEntries;
  const n = entries.length;
  if (!n) return { start: 0, end: -1 };
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return { start: 0, end: n - 1 };
  const thead = wrap.querySelector("thead");
  if (!thead) return { start: 0, end: n - 1 };
  const theadH = thead.offsetHeight;
  const rowH = getExplorerRowHeightPx();
  const scrollTop = wrap.scrollTop;
  const clientH = wrap.clientHeight;
  const first = Math.max(0, Math.floor((scrollTop - theadH) / rowH));
  const last = Math.min(n - 1, Math.max(0, Math.floor((scrollTop + clientH - theadH - 1) / rowH)));
  if (last < first) {
    return { start: 0, end: Math.min(n - 1, EXPLORER_VIRTUAL_OVERSCAN * 2) };
  }
  const start = Math.max(0, first - EXPLORER_VIRTUAL_OVERSCAN);
  const end = Math.min(n - 1, last + EXPLORER_VIRTUAL_OVERSCAN);
  return { start, end };
}

/**
 * @param {number} heightPx
 * @returns {HTMLTableRowElement}
 */
function createSpacerRow(heightPx) {
  const tr = document.createElement("tr");
  tr.className = "explorer-list-spacer";
  tr.setAttribute("aria-hidden", "true");
  const td = document.createElement("td");
  td.colSpan = 4;
  td.className = "explorer-list-spacer-cell";
  td.style.height = `${heightPx}px`;
  tr.appendChild(td);
  return tr;
}

/**
 * @param {"cart" | "pc"} pane
 * @param {{ path: string, name: string, isDir: boolean, size: number, modifiedMs?: number, hidden?: boolean }} e
 * @returns {HTMLTableRowElement}
 */
function createEntryRowElement(pane, e) {
  const tr = document.createElement("tr");
  tr.dataset.path = e.path;
  tr.dataset.isDir = e.isDir ? "1" : "0";
  const icon = e.isDir ? "📁" : "📄";
  const iconClass = e.hidden ? "explorer-icon explorer-icon--hidden" : "explorer-icon";
  tr.innerHTML = `
        ${nameCellHtml(iconClass, icon, e.name)}
        <td class="col-size">${e.isDir ? "" : formatSize(e.size)}</td>
        <td class="col-date">${formatDate(e.modifiedMs)}</td>
        <td class="col-type">${fileTypeLabel(e)}</td>`;
  if (state[pane].selected.has(e.path)) tr.classList.add("selected");
  setupNameTooltipForRow(tr, e.name);
  tr.draggable = true;
  tr.addEventListener("click", (ev) => onRowClick(pane, e.path, ev));
  tr.addEventListener("dblclick", (ev) => {
    ev.preventDefault();
    cancelRenameNameClickArm();
    if (inlineRenameState) {
      cancelInlineRenameRestoreDOMOnly();
    }
    if (e.isDir) navigate(pane, e.path, true);
  });
  bindRowDrag(pane, tr, e.path);
  bindRowDragOver(tr);
  if (e.isDir) bindDirectoryDropTargetRow(tr);
  return tr;
}

/**
 * Mount only the visible slice of `state[pane].listEntries` plus spacer rows so scroll height matches a full list.
 * @param {"cart" | "pc"} pane
 * @param {number} [depth] internal: limit remeasure recursion
 */
function renderExplorerPane(pane, depth = 0) {
  const tbody = document.getElementById(paneTbodyId(pane));
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!tbody || !wrap) return;

  const entries = state[pane].listEntries;
  if (!entries.length) {
    tbody.replaceChildren();
    if (pane === "pc" && !state.pc.path) {
      lastVirtualRange[pane] = null;
      return;
    }
    appendEmptyFolderHintRow(pane);
    lastVirtualRange[pane] = null;
    return;
  }

  let { start, end } = computeVisibleRange(pane);
  if (start > end) {
    start = 0;
    end = Math.min(entries.length - 1, EXPLORER_VIRTUAL_OVERSCAN * 2);
  }
  if (
    lastVirtualRange[pane] &&
    lastVirtualRange[pane].start === start &&
    lastVirtualRange[pane].end === end
  ) {
    return;
  }

  const rowH = getExplorerRowHeightPx();
  const frag = document.createDocumentFragment();
  if (start > 0) {
    frag.appendChild(createSpacerRow(start * rowH));
  }
  for (let i = start; i <= end; i++) {
    frag.appendChild(createEntryRowElement(pane, entries[i]));
  }
  if (end < entries.length - 1) {
    frag.appendChild(createSpacerRow((entries.length - 1 - end) * rowH));
  }
  tbody.replaceChildren(frag);

  const prevMeasured = explorerRowHeightPx;
  const firstData = tbody.querySelector("tr[data-path]");
  if (firstData && firstData.offsetHeight > 0) {
    explorerRowHeightPx = firstData.offsetHeight;
  }
  if (Math.abs(explorerRowHeightPx - prevMeasured) > 1 && depth < 2) {
    lastVirtualRange[pane] = null;
    renderExplorerPane(pane, depth + 1);
    return;
  }

  lastVirtualRange[pane] = { start, end };
  updateExplorerDeleteToolbarButtons();
}

/**
 * @param {"cart" | "pc"} pane
 */
function setupExplorerVirtualScroll(pane) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;
  let raf = null;
  const onScroll = () => {
    if (raf != null) return;
    raf = requestAnimationFrame(() => {
      raf = null;
      lastVirtualRange[pane] = null;
      renderExplorerPane(pane);
    });
  };
  wrap.addEventListener("scroll", onScroll, { passive: true });
}

/** Single placeholder row when a directory lists zero entries (not an error). */
function appendEmptyFolderHintRow(pane) {
  const tbody = document.getElementById(paneTbodyId(pane));
  if (!tbody) return;
  const tr = document.createElement("tr");
  tr.classList.add("explorer-empty-hint");
  tr.setAttribute("aria-hidden", "true");
  tr.innerHTML = `<td colspan="4" class="explorer-empty-hint-cell">No files or folders here</td>`;
  tbody.appendChild(tr);
}

/**
 * @param {"cart" | "pc"} pane
 * @param {Array<{ path: string }>} visible
 * @param {Set<string> | null} savedSel
 * @param {string | null} savedAnchor
 * @param {boolean} [syncDom] When true (default), sync row classes. When false, only update state — call
 *   before `renderExplorerPane` so rows get `.selected` from `createEntryRowElement`.
 */
function restorePaneSelectionAfterLoad(pane, visible, savedSel, savedAnchor, syncDom = true) {
  if (!savedSel) return;
  const pathSet = new Set(visible.map((row) => row.path));
  const st = state[pane];
  const prevSel = new Set(st.selected);
  st.selected.clear();
  for (const p of savedSel) {
    if (pathSet.has(p)) st.selected.add(p);
  }
  if (savedAnchor && st.selected.has(savedAnchor)) {
    st.anchorPath = savedAnchor;
  } else {
    st.anchorPath = st.selected.size ? [...st.selected][0] : null;
  }
  if (syncDom) applySelectionDiffToDom(pane, prevSel);
}

function parseInternalDragPayload(textPlain) {
  if (!textPlain || typeof textPlain !== "string") return null;
  const s = textPlain.trim();
  if (!s.startsWith(DRAG_INTERNAL_PREFIX)) return null;
  try {
    const parsed = JSON.parse(s.slice(DRAG_INTERNAL_PREFIX.length));
    if (parsed && (parsed.sourcePane === "cart" || parsed.sourcePane === "pc") && Array.isArray(parsed.paths)) {
      return parsed;
    }
  } catch {
    return null;
  }
  return null;
}

function rectsIntersect(a, b) {
  return !(a.right < b.left || a.left > b.right || a.bottom < b.top || a.top > b.bottom);
}

/**
 * Rubber-band selection: intersect marquee with file rows (virtual list: uses scroll + fixed row height).
 * Selection is applied once per animation frame (see `setupTableWrapMarquee`); DOM updates only rows whose
 * `selected` state changed vs the previous frame (`applySelectionDiffToDom`), not a full tbody scan.
 * @param {{
 *   pane: "cart" | "pc";
 *   wrap: HTMLElement;
 *   startClientX: number;
 *   startClientY: number;
 *   ctrlKey: boolean;
 * }} m
 */
function applyMarqueeSelection(m, clientX, clientY) {
  const entries = state[m.pane].listEntries;
  const sel = state[m.pane].selected;
  const prevSel = new Set(sel);

  // No rows to intersect; selection state is unchanged (early return before any sel mutation).
  if (!entries.length) {
    return;
  }
  const x1 = Math.min(m.startClientX, clientX);
  const y1 = Math.min(m.startClientY, clientY);
  const x2 = Math.max(m.startClientX, clientX);
  const y2 = Math.max(m.startClientY, clientY);
  const marqueeRect = { left: x1, top: y1, right: x2, bottom: y2 };
  if (!m.ctrlKey) sel.clear();
  const wrap = m.wrap;
  const wr = wrap.getBoundingClientRect();
  if (marqueeRect.right < wr.left || marqueeRect.left > wr.right) {
    applySelectionDiffToDom(m.pane, prevSel);
    return;
  }
  const thead = wrap.querySelector("thead");
  if (!thead) {
    applySelectionDiffToDom(m.pane, prevSel);
    return;
  }
  const theadH = thead.offsetHeight;
  const rowH = getExplorerRowHeightPx();
  const scrollTop = wrap.scrollTop;
  const n = entries.length;
  const cy1 = scrollTop + (Math.min(m.startClientY, clientY) - wr.top);
  const cy2 = scrollTop + (Math.max(m.startClientY, clientY) - wr.top);
  const yLow = Math.min(cy1, cy2);
  const yHigh = Math.max(cy1, cy2);
  const iMin = Math.max(0, Math.floor((yLow - theadH) / rowH));
  const iMax = Math.min(n - 1, Math.floor((yHigh - theadH) / rowH));
  for (let i = iMin; i <= iMax; i++) {
    const rowTop = wr.top + theadH + i * rowH - scrollTop;
    const rowRect = { left: wr.left, right: wr.right, top: rowTop, bottom: rowTop + rowH };
    if (rectsIntersect(marqueeRect, rowRect)) {
      sel.add(entries[i].path);
    }
  }
  applySelectionDiffToDom(m.pane, prevSel);
}

async function copyCartToPcPaths(paths, destOverride = null) {
  const dest =
    destOverride != null && String(destOverride).trim() !== ""
      ? normalizePath(destOverride)
      : state.pc.path;
  if (!dest) {
    finishOperationProgress("Choose a Windows folder first (Browse … next to the path).", true, "pc");
    return;
  }
  if (paths.length === 0) return;
  const action = copyActionLabel("export");
  const label =
    paths.length === 1
      ? `${action} — "${basenameForMessage(paths[0])}"`
      : `${action} — ${paths.length} files`;
  beginPaneActionLoading("cart", "Preparing…");
  try {
    let performed = false;
    const cancelled = await withCartDaemonYield(
      async () => {
        endPaneActionLoading("cart");
        showOperationProgress("Preparing copy…", "cart", false);
        try {
          await invoke("explorer_reset_cancel");
          const plan = await invoke("build_cart_export_plan", {
            cartPaths: paths,
            toPcParent: dest,
          });
          if (!plan.length) {
            hideOperationProgressPane("cart");
            return;
          }
          performed = true;
          await runWithProgress("cart", `${label}…`, () => runInteractiveCopyPlan(plan, "export"));
          await loadBothPanes();
        } catch (e) {
          hideOperationProgressPane("cart");
          throw e;
        }
      },
      { confirm: true, actionPane: "cart" }
    );
    if (cancelled) return;
    if (!performed) return;
    finishOperationProgress(
      paths.length === 1
        ? `Copied ${basenameForMessage(paths[0])}.`
        : `Copied ${paths.length} items.`,
      false,
      "cart"
    );
  } catch (e) {
    if (e && e.userCancelledCopy) {
      await loadBothPanes({ forceRefresh: true });
      finishOperationProgress("Cancelled.", false, "cart");
      return;
    }
    if (isCancelledBackendError(e)) {
      await loadBothPanes({ forceRefresh: true });
      finishOperationProgress("Cancelled.", false, "cart");
    } else finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  } finally {
    endPaneActionLoading("cart");
  }
}

async function copyPcToCartPaths(paths, cartParentOverride = null) {
  const cartParent =
    cartParentOverride != null && String(cartParentOverride).trim() !== ""
      ? normalizeUsbPath(cartParentOverride)
      : normalizeUsbPath(state.cart.path);
  if (paths.length === 0) return;
  const action = copyActionLabel("import");
  const label =
    paths.length === 1
      ? `${action} — "${basenameForMessage(paths[0])}"`
      : `${action} — ${paths.length} files`;
  beginPaneActionLoading("pc", "Preparing…");
  try {
    let performed = false;
    const cancelled = await withCartDaemonYield(
      async () => {
        endPaneActionLoading("pc");
        showOperationProgress("Preparing copy…", "pc", false);
        try {
          await invoke("explorer_reset_cancel");
          const plan = await invoke("build_cart_import_plan", {
            cartParent,
            fromPcPaths: paths,
          });
          if (!plan.length) {
            hideOperationProgressPane("pc");
            return;
          }
          performed = true;
          await runWithProgress("pc", `${label}…`, () => runInteractiveCopyPlan(plan, "import"));
          await loadBothPanes();
        } catch (e) {
          hideOperationProgressPane("pc");
          throw e;
        }
      },
      { confirm: true, actionPane: "pc" }
    );
    if (cancelled) return;
    if (!performed) return;
    finishOperationProgress(
      paths.length === 1
        ? `Copied ${basenameForMessage(paths[0])}.`
        : `Copied ${paths.length} items.`,
      false,
      "pc"
    );
  } catch (e) {
    if (e && e.userCancelledCopy) {
      await loadBothPanes({ forceRefresh: true });
      finishOperationProgress("Cancelled.", false, "pc");
      return;
    }
    if (isCancelledBackendError(e)) {
      await loadBothPanes({ forceRefresh: true });
      finishOperationProgress("Cancelled.", false, "pc");
    } else finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  } finally {
    endPaneActionLoading("pc");
  }
}

function setupTableWrapMarquee(pane) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;

  wrap.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    if (ev.target.closest("thead")) return;
    if (ev.target.closest("tbody tr")) return;

    const el = document.createElement("div");
    el.className = "explorer-marquee";
    const r = wrap.getBoundingClientRect();
    const left = ev.clientX - r.left + wrap.scrollLeft;
    const top = ev.clientY - r.top + wrap.scrollTop;
    el.style.left = `${left}px`;
    el.style.top = `${top}px`;
    el.style.width = "0px";
    el.style.height = "0px";
    wrap.appendChild(el);

    marquee = {
      pane,
      wrap,
      startClientX: ev.clientX,
      startClientY: ev.clientY,
      ctrlKey: ev.ctrlKey,
      el,
      selRafId: null,
      pendingClientX: ev.clientX,
      pendingClientY: ev.clientY,
    };

    const runMarqueeSelectionRaf = () => {
      if (!marquee) return;
      marquee.selRafId = null;
      applyMarqueeSelection(marquee, marquee.pendingClientX, marquee.pendingClientY);
    };

    const onMove = (e) => {
      if (!marquee) return;
      const wr = marquee.wrap.getBoundingClientRect();
      const x1 = Math.min(marquee.startClientX, e.clientX);
      const y1 = Math.min(marquee.startClientY, e.clientY);
      const x2 = Math.max(marquee.startClientX, e.clientX);
      const y2 = Math.max(marquee.startClientY, e.clientY);
      marquee.el.style.left = `${x1 - wr.left + marquee.wrap.scrollLeft}px`;
      marquee.el.style.top = `${y1 - wr.top + marquee.wrap.scrollTop}px`;
      marquee.el.style.width = `${x2 - x1}px`;
      marquee.el.style.height = `${y2 - y1}px`;

      marquee.pendingClientX = e.clientX;
      marquee.pendingClientY = e.clientY;
      if (marquee.selRafId != null) return;
      marquee.selRafId = requestAnimationFrame(runMarqueeSelectionRaf);
    };

    const onUp = (e) => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (!marquee) return;
      const m = marquee;
      if (m.selRafId != null) {
        cancelAnimationFrame(m.selRafId);
        m.selRafId = null;
      }
      m.el.remove();
      const dist = Math.hypot(e.clientX - m.startClientX, e.clientY - m.startClientY);
      if (dist < 5 && !m.ctrlKey) {
        const prevSel = new Set(state[m.pane].selected);
        state[m.pane].selected.clear();
        state[m.pane].anchorPath = null;
        applySelectionDiffToDom(m.pane, prevSel);
      } else if (dist < 5 && m.ctrlKey) {
        const tb = m.wrap.querySelector("tbody");
        const first = tb?.querySelector("tr.selected");
        state[m.pane].anchorPath = first ? first.dataset.path : null;
      } else {
        applyMarqueeSelection(m, e.clientX, e.clientY);
        const tb = m.wrap.querySelector("tbody");
        const first = tb?.querySelector("tr.selected");
        state[m.pane].anchorPath = first ? first.dataset.path : null;
      }
      marquee = null;
    };

    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    ev.preventDefault();
  });
}

function setupTableWrapDrop(pane) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;

  const onDragEnter = (ev) => {
    if (isExternalFileDrag(ev.dataTransfer)) {
      ev.dataTransfer.dropEffect = "none";
      return;
    }
    ev.preventDefault();
    ev.dataTransfer.dropEffect = "copy";
  };

  const onDragOver = (ev) => {
    if (isExternalFileDrag(ev.dataTransfer)) {
      ev.preventDefault();
      ev.dataTransfer.dropEffect = "none";
      wrap.classList.remove("drag-over-target");
      return;
    }
    ev.preventDefault();
    ev.dataTransfer.dropEffect = "copy";
    const dirTr = ev.target.closest("tbody tr[data-is-dir='1']");
    if (dirTr) wrap.classList.remove("drag-over-target");
    else wrap.classList.add("drag-over-target");
  };

  const onDragLeave = (ev) => {
    if (!wrap.contains(ev.relatedTarget)) wrap.classList.remove("drag-over-target");
  };

  wrap.addEventListener("dragenter", onDragEnter, true);
  wrap.addEventListener("dragover", onDragOver, true);
  wrap.addEventListener("dragleave", onDragLeave, true);

  wrap.addEventListener(
    "drop",
    async (ev) => {
      ev.preventDefault();
      ev.stopPropagation();
      wrap.classList.remove("drag-over-target");
      const dirTr = ev.target.closest("tbody tr[data-is-dir='1']");
      dirTr?.classList.remove("drag-over-drop-target");
      const destPath = dirTr?.dataset?.path ?? null;

      const dt = ev.dataTransfer;
      const plain = dt.getData("text/plain");
      const internal = parseInternalDragPayload(plain);
      if (!internal) return;
      const { sourcePane, paths } = internal;
      if (!paths.length || sourcePane === pane) return;
      if (sourcePane === "cart" && pane === "pc") await copyCartToPcPaths(paths, destPath);
      else if (sourcePane === "pc" && pane === "cart") await copyPcToCartPaths(paths, destPath);
    },
    true
  );
}

/**
 * After `state[pane].selected` mutates, update only rows whose membership changed vs `prevSel`.
 * Uses one pass over mounted `tr[data-path]` (virtual list: viewport-sized).
 * Full pane rebuilds go through `renderExplorerPane` → `createEntryRowElement`, which applies `.selected` from state.
 * @param {"cart" | "pc"} pane
 * @param {Set<string>} prevSel snapshot before the mutation
 */
function applySelectionDiffToDom(pane, prevSel) {
  const sel = state[pane].selected;
  const tbody = document.getElementById(paneTbodyId(pane));
  if (!tbody) return;
  const trMap = new Map();
  for (const tr of tbody.querySelectorAll("tr[data-path]")) {
    const p = tr.dataset.path;
    if (p) trMap.set(p, tr);
  }
  for (const p of prevSel) {
    if (!sel.has(p)) {
      const tr = trMap.get(p);
      if (tr) tr.classList.remove("selected");
    }
  }
  for (const p of sel) {
    if (!prevSel.has(p)) {
      const tr = trMap.get(p);
      if (tr) tr.classList.add("selected");
    }
  }
  updateExplorerDeleteToolbarButtons();
}

/** Toolbar delete buttons reflect each pane’s selection (not the focused pane). */
function updateExplorerDeleteToolbarButtons() {
  const btnCart = document.getElementById("btn-delete-cart");
  const btnPc = document.getElementById("btn-delete-pc");
  if (btnCart) btnCart.disabled = state.cart.selected.size === 0;
  if (btnPc) btnPc.disabled = state.pc.selected.size === 0;
}

function isKeyboardBypassTarget(el) {
  if (!el || !(el instanceof Element)) return false;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || tag === "OPTION") return true;
  if (el.isContentEditable) return true;
  if (el.closest("[contenteditable]")) return true;
  return false;
}

function isSelectableDataRow(tr) {
  return Boolean(tr.dataset.path) && !tr.classList.contains("explorer-empty-hint");
}

function selectAllInPane(pane) {
  const entries = state[pane].listEntries;
  if (!entries.length) return;
  const sel = state[pane].selected;
  const prevSel = new Set(sel);
  sel.clear();
  for (const e of entries) sel.add(e.path);
  state[pane].anchorPath = entries[entries.length - 1].path;
  applySelectionDiffToDom(pane, prevSel);
}

function moveSelectionArrow(pane, delta, extend) {
  const paths = getOrderedPaths(pane);
  if (!paths.length) return;
  let focusIdx = 0;
  if (state[pane].selected.size === 1) {
    focusIdx = paths.indexOf([...state[pane].selected][0]);
  } else if (state[pane].anchorPath) {
    focusIdx = paths.indexOf(state[pane].anchorPath);
  }
  if (focusIdx < 0) focusIdx = 0;
  const newIdx = Math.max(0, Math.min(paths.length - 1, focusIdx + delta));
  if (extend) {
    let anchorIdx = state[pane].anchorPath ? paths.indexOf(state[pane].anchorPath) : -1;
    if (anchorIdx < 0) {
      anchorIdx = focusIdx;
      state[pane].anchorPath = paths[anchorIdx];
    }
    const a = Math.min(anchorIdx, newIdx);
    const b = Math.max(anchorIdx, newIdx);
    state[pane].selected.clear();
    for (let i = a; i <= b; i++) state[pane].selected.add(paths[i]);
  } else {
    state[pane].selected.clear();
    state[pane].selected.add(paths[newIdx]);
    state[pane].anchorPath = paths[newIdx];
  }
  lastVirtualRange[pane] = null;
  scrollVirtualRowIntoView(pane, newIdx);
  renderExplorerPane(pane);
}

function moveSelectionEdge(pane, toEnd) {
  const paths = getOrderedPaths(pane);
  if (!paths.length) return;
  const newIdx = toEnd ? paths.length - 1 : 0;
  state[pane].selected.clear();
  state[pane].selected.add(paths[newIdx]);
  state[pane].anchorPath = paths[newIdx];
  lastVirtualRange[pane] = null;
  scrollVirtualRowIntoView(pane, newIdx);
  renderExplorerPane(pane);
}

function activateSelectedFolder(pane) {
  if (state[pane].selected.size !== 1) return;
  const path = [...state[pane].selected][0];
  const entry = state[pane].listEntries.find((e) => e.path === path);
  if (!entry || !entry.isDir) return;
  navigate(pane, path, true);
}

async function deleteSelectedCart(alertIfEmpty) {
  const paths = [...state.cart.selected];
  if (paths.length === 0) {
    if (alertIfEmpty) await showExplorerAlert("Select one or more items on the SD card to delete.");
    return;
  }
  beginPaneActionLoading("cart", "Preparing…");
  let multi64dRunning = false;
  try {
    const p = await invoke("explorer_daemon_probe", { listen: daemonListenUrl() });
    multi64dRunning = p.up === true;
  } catch {
    multi64dRunning = false;
  }
  const deleteMsg = multi64dRunning
    ? `Delete ${paths.length} ${paths.length === 1 ? "item" : "items"} from the SD card?\n\nThe Multi64 bridge is using this cart—it will pause during the delete, then resume.`
    : `Delete ${paths.length} ${paths.length === 1 ? "item" : "items"} from the SD card?`;
  endPaneActionLoading("cart");
  if (!(await showExplorerConfirm(deleteMsg))) return;
  beginPaneActionLoading("cart", "Preparing…");
  try {
    const cancelled = await withCartDaemonYield(async () => {
      endPaneActionLoading("cart");
      showOperationProgress("Preparing…", "cart", false);
      await runWithProgress(
        "cart",
        paths.length === 1
          ? `Deleting from SD card — "${basenameForMessage(paths[0])}"…`
          : `Deleting from SD card — ${paths.length} items…`,
        () => invoke("cart_serial_remove_cart", { paths })
      );
      state.cart.selected.clear();
      await loadCartPane();
    });
    if (cancelled) return;
    finishOperationProgress(
      paths.length === 1
        ? `Deleted "${basenameForMessage(paths[0])}".`
        : `Deleted ${paths.length} items.`,
      false,
      "cart"
    );
  } catch (e) {
    if (isCancelledBackendError(e)) {
      state.cart.selected.clear();
      await loadCartPane();
      finishOperationProgress("Cancelled.", false, "cart");
    } else finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  } finally {
    endPaneActionLoading("cart");
  }
}

async function deleteSelectedPc(alertIfEmpty) {
  const paths = [...state.pc.selected];
  if (paths.length === 0) {
    if (alertIfEmpty) await showExplorerAlert("Select one or more items in the Windows folder to delete.");
    return;
  }
  beginPaneActionLoading("pc", "Preparing…");
  await new Promise((r) => requestAnimationFrame(r));
  endPaneActionLoading("pc");
  if (
    !(await showExplorerConfirm(
      `Delete ${paths.length} ${paths.length === 1 ? "item" : "items"} from this Windows folder?`
    ))
  )
    return;
  beginPaneActionLoading("pc", "Preparing…");
  try {
    resetProgressCancel();
    endPaneActionLoading("pc");
    showOperationProgress(
      paths.length === 1
        ? `Deleting from Windows — "${basenameForMessage(paths[0])}"…`
        : `Deleting from Windows — ${paths.length} items…`,
      "pc",
      true
    );
    const fill = document.getElementById("explorer-operation-fill-pc");
    const textOp = document.getElementById("explorer-operation-text-pc");
    const n = paths.length;
    for (let i = 0; i < n; i++) {
      if (progressCancelRequested) {
        state.pc.selected.clear();
        await loadPcPane();
        finishOperationProgress("Cancelled.", false, "pc");
        return;
      }
      await invoke("fs_remove", { path: paths[i] });
      const done = i + 1;
      const rem = n - done;
      const itemName = basenameForMessage(paths[i]);
      if (textOp)
        textOp.textContent =
          n === 1
            ? `Deleting from Windows — "${itemName}"…`
            : `Deleting from Windows — "${itemName}" (${done} of ${n}, ${rem} left)`;
      if (fill) {
        fill.classList.remove("indeterminate");
        fill.style.width = `${(done / n) * 100}%`;
      }
    }
    state.pc.selected.clear();
    state.pc.anchorPath = null;
    await loadPcPane();
    finishOperationProgress(
      paths.length === 1
        ? `Deleted "${basenameForMessage(paths[0])}".`
        : `Deleted ${paths.length} items.`,
      false,
      "pc"
    );
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  } finally {
    endPaneActionLoading("pc");
  }
}

async function promptMkdirCart() {
  const name = await showExplorerPrompt("Name the new folder:", PROMPT_MKDIR_OPTS);
  if (!name || !name.trim()) return;
  const path = cartRelPathForMkdir(name);
  if (!path) return;
  const display = name.trim();
  beginPaneActionLoading("cart", "Preparing…");
  try {
    const cancelled = await withCartDaemonYield(
      async () => {
        endPaneActionLoading("cart");
        showOperationProgress(`Creating folder on SD card — "${display}"…`, "cart");
        await invoke("cart_serial_mkdir_cart", { path });
        await loadCartPane();
      },
      { confirm: true, actionPane: "cart" }
    );
    if (cancelled) return;
    finishOperationProgress(`Created "${display}".`, false, "cart");
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  } finally {
    endPaneActionLoading("cart");
  }
}

async function promptMkdirPc() {
  const parent = state.pc.path;
  if (!parent) {
    finishOperationProgress("Choose a Windows folder first (Browse … next to the path).", true, "pc");
    return;
  }
  const name = await showExplorerPrompt("Name the new folder:", PROMPT_MKDIR_OPTS);
  if (!name || !name.trim()) return;
  const path = `${parent.replace(/[/\\]+$/, "")}\\${name.trim()}`;
  const display = name.trim();
  showOperationProgress(`Creating folder on Windows — "${display}"…`, "pc");
  try {
    await invoke("fs_mkdir", { path });
    await loadPcPane();
    finishOperationProgress(`Created "${display}".`, false, "pc");
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  }
}

async function promptRenameCart() {
  if (inlineRenameState) cancelInlineRenameRestoreDOMOnly();
  if (state.cart.selected.size !== 1) {
    finishOperationProgress("Select a single item to rename.", true, "cart");
    return;
  }
  const fromPath = normalizeUsbPath([...state.cart.selected][0]);
  const baseName = basenameForMessage(fromPath);
  const name = await showExplorerPrompt("New name:", {
    defaultValue: baseName,
    selectFilenameStem: true,
  });
  if (!name || !name.trim()) return;
  try {
    await runRenameCartFromPaths(fromPath, name.trim());
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  }
}

async function promptRenamePc() {
  if (inlineRenameState) cancelInlineRenameRestoreDOMOnly();
  if (state.pc.selected.size !== 1) {
    finishOperationProgress("Select a single item to rename.", true, "pc");
    return;
  }
  const fromPath = [...state.pc.selected][0];
  const baseName = basenameForMessage(fromPath);
  const name = await showExplorerPrompt("New name:", {
    defaultValue: baseName,
    selectFilenameStem: true,
  });
  if (!name || !name.trim()) return;
  try {
    await runRenamePcFromPaths(fromPath, name.trim());
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  }
}

function setupPaneFocusTracking() {
  document.getElementById("pane-cart")?.addEventListener("mousedown", () => {
    focusedPane = "cart";
  });
  document.getElementById("pane-pc")?.addEventListener("mousedown", () => {
    focusedPane = "pc";
  });
}

function setupTableWrapKeyboardFocus(pane) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;
  wrap.tabIndex = -1;
  wrap.addEventListener("mousedown", () => {
    focusedPane = pane;
  });
}

function setupExplorerKeyboard() {
  document.addEventListener(
    "keydown",
    (ev) => {
      if (isExplorerModalOpen()) return;
      if (isExplorerContextMenuVisible()) {
        if (ev.key === "Escape") {
          hideExplorerContextMenu();
          ev.preventDefault();
        }
        return;
      }
      if (isKeyboardBypassTarget(ev.target)) return;
      const pane = focusedPane;
      const key = ev.key;
      if (key === "Delete") {
        ev.preventDefault();
        if (pane === "cart") void deleteSelectedCart(false);
        else void deleteSelectedPc(false);
        return;
      }
      if (key === "Backspace") {
        ev.preventDefault();
        void goUp(pane);
        return;
      }
      if (key === "F5") {
        ev.preventDefault();
        if (pane === "cart") {
          void refreshUsbComPorts();
          void loadCartPane({ forceRefresh: true });
        } else void loadPcPane({ forceRefresh: true });
        return;
      }
      if (key === "F2") {
        ev.preventDefault();
        if (inlineRenameState) {
          inlineRenameState.input.focus();
          selectFilenameStemInInput(inlineRenameState.input);
          return;
        }
        if (pane === "cart") void promptRenameCart();
        else void promptRenamePc();
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && key === "a" && !ev.shiftKey && !ev.altKey) {
        ev.preventDefault();
        selectAllInPane(pane);
        return;
      }
      if (key === "ArrowDown") {
        ev.preventDefault();
        moveSelectionArrow(pane, 1, ev.shiftKey);
        return;
      }
      if (key === "ArrowUp") {
        ev.preventDefault();
        moveSelectionArrow(pane, -1, ev.shiftKey);
        return;
      }
      if (key === "Home") {
        ev.preventDefault();
        moveSelectionEdge(pane, false);
        return;
      }
      if (key === "End") {
        ev.preventDefault();
        moveSelectionEdge(pane, true);
        return;
      }
      if (key === "Enter") {
        ev.preventDefault();
        activateSelectedFolder(pane);
        return;
      }
      if (key === "Escape") {
        hideNameTooltip();
        const prevSel = new Set(state[pane].selected);
        state[pane].selected.clear();
        state[pane].anchorPath = null;
        applySelectionDiffToDom(pane, prevSel);
        ev.preventDefault();
        return;
      }
      if (ev.altKey && key === "ArrowLeft") {
        ev.preventDefault();
        goBack(pane);
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && key.toLowerCase() === "n") {
        ev.preventDefault();
        if (pane === "cart") void promptMkdirCart();
        else void promptMkdirPc();
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && key === "ArrowRight") {
        ev.preventDefault();
        if (pane === "cart") void copyCartToPcPaths([...state.cart.selected]);
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && key === "ArrowLeft") {
        ev.preventDefault();
        if (pane === "pc") void copyPcToCartPaths([...state.pc.selected]);
        return;
      }
    },
    true
  );
}

function navigate(pane, path, pushHist) {
  let p;
  if (pane === "cart") {
    p = normalizeUsbPath(path);
  } else {
    p = normalizePath(path);
    if (!p) return;
  }
  const s = state[pane];
  if (s.path === p) return;
  if (pushHist !== false) {
    if (s.history.length === 0 || s.histIndex < 0) {
      s.history = [p];
      s.histIndex = 0;
    } else {
      s.history = s.history.slice(0, s.histIndex + 1);
      if (s.history[s.history.length - 1] !== p) {
        s.history.push(p);
        s.histIndex = s.history.length - 1;
      }
    }
  }
  s.path = p;
  if (pane === "cart") {
    void invoke("explorer_set_quick_upload_cart_path", { path: p }).catch(() => {});
    void loadCartPane();
  } else void loadPcPane();
}

function goBack(pane) {
  const s = state[pane];
  if (s.histIndex <= 0) return;
  s.histIndex--;
  s.path = s.history[s.histIndex];
  if (pane === "cart") void loadCartPane();
  else void loadPcPane();
}

async function goUp(pane) {
  const s = state[pane];
  if (!s.path) return;
  if (pane === "cart") {
    const parent = usbParentPath(s.path);
    navigate("cart", parent, true);
    return;
  }
  const parent = await invoke("fs_parent", { path: s.path });
  navigate("pc", parent, true);
}

/** Sorted join of serial port names — detect plug/unplug without native USB events. */
function serialPortsSnapshot(ports) {
  return Array.isArray(ports) ? [...ports].sort().join("\0") : "";
}

/** Last seen port list from `refreshUsbComPorts` / hotplug poll (for change detection). */
let usbSerialPortsSnapshot = "";

/** Prevents overlapping hotplug polls (slow `loadCartPane` / hint refresh vs 2.5s interval). */
let usbSerialPollInFlight = false;

/**
 * Repopulate the COM dropdown. Pass `ports` when the list was already fetched (e.g. hotplug poll).
 * @param {string[] | undefined} [portsOpt]
 */
async function refreshUsbComPorts(portsOpt) {
  const sel = document.getElementById("select-usb-com");
  if (!sel) return;
  const needFetch = portsOpt === undefined;
  if (needFetch) beginUsbLoading("Listing serial ports…");
  try {
    const ports = portsOpt ?? (await invoke("cart_serial_list_ports"));
    usbSerialPortsSnapshot = serialPortsSnapshot(ports);
    const saved = localStorage.getItem(LS_USB_COM) || "";
    sel.innerHTML = "";
    const opt0 = document.createElement("option");
    opt0.value = "";
    opt0.textContent = ports.length ? "Auto-detect" : "No serial ports";
    sel.appendChild(opt0);
    for (const p of ports) {
      const o = document.createElement("option");
      o.value = p;
      o.textContent = p;
      sel.appendChild(o);
    }
    if (saved && [...sel.options].some((o) => o.value === saved)) sel.value = saved;
  } finally {
    if (needFetch) endUsbLoading();
  }
}

const USB_SERIAL_POLL_MS = 2500;
/** After the tab becomes visible, COM devices can enumerate a few hundred ms late (Windows). Extra polls, single-flight coalesced. */
const USB_SERIAL_VISIBILITY_BURST_DELAYS_MS = [500, 1500];

/**
 * When serial devices appear or disappear (cart plugged / unplugged), refresh COM UI and cart pane.
 * Uses polling — OS-specific device notifications are not wired in this build.
 */
async function pollUsbSerialPortsOnChange() {
  if (document.visibilityState !== "visible") return;
  if (isExplorerModalOpen()) return;
  if (document.documentElement.classList.contains("explorer-settings-open")) return;
  if (usbSerialPollInFlight) return;
  usbSerialPollInFlight = true;
  try {
    let ports;
    try {
      ports = await invoke("cart_serial_list_ports");
    } catch {
      return;
    }
    if (serialPortsSnapshot(ports) === usbSerialPortsSnapshot) return;

    await refreshUsbComPorts(ports);
    await syncPreferredComToBackend();
    await refreshUsbDetectHint();
    await loadCartPane({ forceRefresh: true });
  } finally {
    usbSerialPollInFlight = false;
  }
}

function scheduleUsbSerialVisibilityBurst() {
  for (const delay of USB_SERIAL_VISIBILITY_BURST_DELAYS_MS) {
    setTimeout(() => void pollUsbSerialPortsOnChange(), delay);
  }
}

function setupUsbSerialHotplug() {
  setInterval(() => void pollUsbSerialPortsOnChange(), USB_SERIAL_POLL_MS);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") return;
    void pollUsbSerialPortsOnChange();
    scheduleUsbSerialVisibilityBurst();
  });
}

async function syncPreferredComToBackend() {
  const v = (document.getElementById("select-usb-com")?.value || "").trim();
  try {
    await invoke("cart_serial_set_preferred_com", { port: v });
  } catch {
  /* ignore */
  }
}

function updateExplorerDevShellButton() {
  const on = document.getElementById("explorer-developer-mode")?.checked === true;
  const row = document.getElementById("explorer-dev-shell-row");
  const btn = document.getElementById("btn-open-dev-shell");
  if (row) {
    row.hidden = !on;
    row.setAttribute("aria-hidden", on ? "false" : "true");
  }
  if (btn) btn.disabled = !on;
}

/** @param {unknown} base */
function formatEd64LinearBaseForInput(base) {
  if (base == null || base === "") return "";
  const n = Number(base);
  if (!Number.isFinite(n)) return "";
  return `0x${(n >>> 0).toString(16)}`;
}

/**
 * @param {string} raw
 * @returns {number | null | undefined} `undefined` = empty field (clear), `null` = invalid
 */
function parseEd64LinearBaseInput(raw) {
  const s = String(raw ?? "").trim();
  if (s === "") return undefined;
  let n;
  if (/^0x/i.test(s)) {
    n = parseInt(s, 16);
  } else {
    n = parseInt(s, 10);
  }
  if (!Number.isFinite(n) || n < 0 || n > 0xffffffff) return null;
  return n >>> 0;
}

function updateEd64AdvancedSectionVisibility() {
  const sec = document.getElementById("explorer-ed64-advanced-section");
  const cd = document.getElementById("explorer-cart-device");
  const v = cd?.value || "auto";
  if (!sec) return;
  const show = v === "ed64_beta";
  sec.hidden = !show;
  sec.setAttribute("aria-hidden", show ? "false" : "true");
}

/** Fills “addresses tried first” from the backend (curated hint list + scan note). */
async function refreshEd64LinearHintBases() {
  const el = document.getElementById("explorer-ed64-hint-addresses");
  if (!el) return;
  try {
    const hints = await invoke("cart_serial_ed64_linear_hint_bases");
    el.textContent = `Scan tries these first: ${hints.join(", ")}, then a wider grid over the cart ROM range. If several matches appear, pick the one that lists your SD correctly.`;
  } catch {
    el.textContent = "";
  }
}

/** EverDrive auto-scan offer state (Settings cart = Auto). */
let lastAutoUsbCartKind = "unset";
let ed64LinearScanRunning = false;
let ed64AutoOfferModalOpen = false;

async function hasEd64RomLinearBaseConfigured() {
  try {
    const s = await invoke("explorer_get_settings");
    const b = s.ed64RomLinearBase;
    return b != null && b !== "";
  } catch {
    return false;
  }
}

/** Settings “Scan for SD base” / auto-offer: probe, fill field, cart-pane progress + Cancel. @returns {Promise<boolean>} false if cancelled mid-scan. */
async function runEd64LinearBaseScan() {
  if (ed64LinearScanRunning) return false;
  ed64LinearScanRunning = true;
  const status = document.getElementById("explorer-ed64-probe-status");
  const input = document.getElementById("explorer-ed64-linear-base");
  const btn = document.getElementById("btn-ed64-probe-linear-base");
  const cartMeta = document.getElementById("status-cart-meta");
  if (btn) btn.disabled = true;
  if (status) status.textContent = "Scanning…";
  if (cartMeta) cartMeta.textContent = ED64_SD_BASE_SCAN_STATUS_MSG;
  let scanCancelled = false;
  try {
    let r;
    await runWithProgress("cart", ED64_SD_BASE_SCAN_STATUS_MSG, async () => {
      r = await invoke("cart_serial_probe_ed64_linear_base");
    });
    const candidates = r.candidates || [];
    const checked = r.basesChecked ?? 0;
    if (candidates.length === 0) {
      await showExplorerAlert(
        `No automatic match (${checked} addresses tried). Try another USB port, close other apps using the cart, or enter a base address manually.`,
      );
      finishOperationProgress(`No automatic match (${checked} addresses tried).`, true, "cart");
    } else if (candidates.length === 1) {
      if (input) input.value = formatEd64LinearBaseForInput(candidates[0]);
      finishOperationProgress("SD base address filled in.", false, "cart");
    } else {
      if (input) input.value = formatEd64LinearBaseForInput(candidates[0]);
      const list = candidates.map((x) => formatEd64LinearBaseForInput(x)).join(", ");
      await showExplorerAlert(
        `Several possible bases: ${list}. The first is filled in—save Settings and try the SD pane; if listing fails, try the next value.`,
      );
      finishOperationProgress("Several possible bases — see the alert.", false, "cart");
    }
    return true;
  } catch (e) {
    if (isCancelledBackendError(e)) {
      scanCancelled = true;
      if (status) status.textContent = "Cancelled.";
      finishOperationProgress("Scan cancelled.", false, "cart");
      return false;
    }
    if (status) status.textContent = "";
    const msg = userFacingErrorMessage(e, { context: "general" });
    await showExplorerAlert(msg);
    finishOperationProgress(msg, true, "cart");
    return true;
  } finally {
    if (btn) btn.disabled = false;
    if (status && !scanCancelled) status.textContent = "";
    ed64LinearScanRunning = false;
    void loadCartPane({ preserveSelection: true, forceRefresh: true });
  }
}

/** Confirm + scan when no linear base; returns false if dismissed, cancelled mid-scan, or already running. */
async function maybeOfferEd64AutoScan() {
  if (ed64LinearScanRunning) return false;
  if (await hasEd64RomLinearBaseConfigured()) return false;

  const ok = await showExplorerModal({
    type: "confirm",
    title: "EverDrive: find SD base",
    message:
      "Xfer64 can scan the USB link for a valid SD card start address. This sends many read commands and may take about a minute or longer.\n\nContinue with the scan now?\n\nYou can cancel and use “Scan for SD base” in Settings → EverDrive (advanced) later.",
  });
  if (!ok) return false;

  return runEd64LinearBaseScan();
}

/** USB auto-detect saw EverDrive without a saved base: one modal + scan (guarded; no stacked prompts). */
async function maybeStartEd64AutoUsbOffer() {
  if (ed64AutoOfferModalOpen) return;
  ed64AutoOfferModalOpen = true;
  try {
    const accepted = await maybeOfferEd64AutoScan();
    if (accepted) lastAutoUsbCartKind = "ed64";
  } finally {
    ed64AutoOfferModalOpen = false;
  }
}

/** Updates the cart device hint (EverDrive depends on linear ROM address in Settings). */
async function updateCartDeviceSettingsHint() {
  const cd = document.getElementById("explorer-cart-device");
  const v = cd?.value || "auto";
  const hintEl = document.getElementById("explorer-cart-device-hint");
  if (!hintEl) return;
  if (v === "ed64_beta") {
    try {
      const s = await invoke("explorer_get_settings");
      const base = s.ed64RomLinearBase;
      if (base != null && base !== "") {
        hintEl.textContent =
          "SD browsing from this PC is enabled. Adjust the address under EverDrive (advanced) if needed.";
      } else {
        hintEl.textContent =
          "Use EverDrive (advanced) below to enable SD browsing from this PC, or choose SummerCart64 above for plug-and-play USB SD access.";
      }
    } catch {
      hintEl.textContent =
        "Use EverDrive (advanced) below for SD browsing, or SummerCart64 for plug-and-play USB SD access.";
    }
  } else if (v === "sc64") {
    hintEl.textContent = "Full USB SD file access over serial (FAT or exFAT) for SummerCart64.";
  } else {
    hintEl.textContent =
      "Probes candidate COM ports and picks the first SC64/EverDrive signature match. Non-cart serial devices are ignored; override manually if needed.";
  }
}

/** Updates COM row + pane subtitle from backend probe (Settings mode + last USB probe). */
async function refreshUsbDetectHint() {
  beginUsbLoading("Detecting cart…");
  try {
    try {
      const s = await invoke("explorer_get_settings");
      const mode =
        s.cartDevice === "ed64_beta" ? "ed64_beta" : s.cartDevice === "sc64" ? "sc64" : "auto";
      const hint = document.getElementById("explorer-pane-cart-hint");
      const usbHint = document.getElementById("usb-hint");
      if (mode !== "auto") {
        lastAutoUsbCartKind = "unset";
        if (hint) {
          if (mode === "ed64_beta") hint.textContent = "EverDrive (beta)";
          else hint.textContent = "SC64";
        }
        if (usbHint) {
          usbHint.textContent = mode === "ed64_beta" ? "Manual: EverDrive" : "Manual: SC64";
        }
        return;
      }
      const hasEd64Base = s.ed64RomLinearBase != null && s.ed64RomLinearBase !== "";
      await withCartDaemonYield(async () => {
        const st = await invoke("cart_serial_probe_status");
        if (hint) {
          if (st.detectedKind === "sc64") hint.textContent = "SC64";
          else if (st.detectedKind === "ed64") hint.textContent = "EverDrive (beta)";
          else if (st.detectedKind === "unknown") hint.textContent = "Unknown";
          else hint.textContent = "Auto";
        }
        if (usbHint) {
          const p = st.resolvedPort || "";
          if (st.detectedKind === "sc64") usbHint.textContent = `Auto · SC64 · ${p}`;
          else if (st.detectedKind === "ed64") usbHint.textContent = `Auto · EverDrive (beta) · ${p}`;
          else if (st.detectedKind === "unknown") usbHint.textContent = `Auto · not detected · ${p}`;
          else usbHint.textContent = p ? `Auto · ${p}` : "Auto-detect";
        }
        const kind = st.detectedKind || "unknown";
        if (kind === "ed64" && !hasEd64Base && lastAutoUsbCartKind !== "ed64") {
          void maybeStartEd64AutoUsbOffer();
        } else {
          lastAutoUsbCartKind = kind;
        }
      });
    } catch {
      const usbHint = document.getElementById("usb-hint");
      if (usbHint) usbHint.textContent = "Auto-detect";
    }
  } finally {
    endUsbLoading();
  }
}

/**
 * @param {{ skipUsbRefresh?: boolean }} [opts]
 *   When `skipUsbRefresh` is true, only updates labels from the cart device control (no COM probe / daemon yield).
 */
function applyCartDeviceUi(opts = {}) {
  const skipUsbRefresh = opts.skipUsbRefresh === true;
  const cd = document.getElementById("explorer-cart-device");
  const v = cd?.value || "auto";
  const hint = document.getElementById("explorer-pane-cart-hint");
  if (hint) {
    if (v === "ed64_beta") hint.textContent = "EverDrive (beta)";
    else if (v === "sc64") hint.textContent = "SC64";
    else hint.textContent = "Auto";
  }
  updateEd64AdvancedSectionVisibility();
  void updateCartDeviceSettingsHint();
  if (!skipUsbRefresh) {
    void refreshUsbDetectHint();
  }
}

function setExplorerSettingsOpen(open) {
  const panel = document.getElementById("explorer-settings-panel");
  const backdrop = document.getElementById("explorer-settings-backdrop");
  const opener = document.getElementById("btn-open-settings");
  if (!panel || !backdrop) return;
  panel.hidden = !open;
  backdrop.hidden = !open;
  opener?.setAttribute("aria-expanded", open ? "true" : "false");
  backdrop.setAttribute("aria-hidden", open ? "false" : "true");
  document.documentElement.classList.toggle("explorer-settings-open", open);
  document.body.classList.toggle("explorer-settings-open", open);
  if (open) {
    document.getElementById("btn-close-settings")?.focus();
  } else {
    void loadExplorerSettings().then(() => opener?.focus());
  }
}

/** True while Add/Remove Send to is running — do not clear busy from overlapping refresh() calls. */
let explorerSendToUploadOpInFlight = false;

/** @param {boolean} busy */
function setExplorerSendToUploadBusy(busy) {
  const btn = document.getElementById("btn-sendto-upload");
  const status = document.getElementById("explorer-sendto-status");
  if (!btn) return;
  explorerSendToUploadOpInFlight = busy;
  btn.disabled = busy;
  btn.setAttribute("aria-disabled", busy ? "true" : "false");
  btn.setAttribute("aria-busy", busy ? "true" : "false");
  btn.classList.toggle("explorer-sendto-upload--busy", busy);
  if (status) {
    status.hidden = !busy;
    status.setAttribute("aria-hidden", busy ? "false" : "true");
  }
}

async function refreshExplorerSendToUploadButton() {
  const section = document.getElementById("explorer-settings-sendto-section");
  const btn = document.getElementById("btn-sendto-upload");
  if (!section || !btn) return;
  try {
    const supported = await invoke("explorer_send_to_upload_supported");
    if (!supported) {
      section.hidden = true;
      section.setAttribute("aria-hidden", "true");
      return;
    }
    section.hidden = false;
    section.setAttribute("aria-hidden", "false");
    const installed = await invoke("explorer_send_to_upload_is_installed");
    btn.textContent = installed ? "Remove from Send to" : "Add to Send to";
    btn.dataset.installed = installed ? "1" : "0";
  } catch {
    section.hidden = true;
    section.setAttribute("aria-hidden", "true");
  } finally {
    if (!explorerSendToUploadOpInFlight) {
      setExplorerSendToUploadBusy(false);
    }
  }
}

async function loadExplorerSettings() {
  try {
    const s = await invoke("explorer_get_settings");
    const nextCommit = buildExplorerSettingsCommit(s);
    const skipUsbRefresh = explorerSettingsCommitUsbEquivalent(lastExplorerSettingsCommit, nextCommit);
    lastExplorerSettingsCommit = nextCommit;

    const el = document.getElementById("explorer-developer-mode");
    if (el) el.checked = nextCommit.developerMode;
    const cd = document.getElementById("explorer-cart-device");
    explorerSettingsCartDeviceAtLoad = nextCommit.cartDevice;
    if (cd) {
      cd.value = nextCommit.cartDevice;
    }
    const baseEl = document.getElementById("explorer-ed64-linear-base");
    if (baseEl) {
      baseEl.value = formatEd64LinearBaseForInput(s.ed64RomLinearBase);
    }
    updateExplorerDevShellButton();
    applyCartDeviceUi({ skipUsbRefresh });
    await refreshExplorerSendToUploadButton();
  } catch (e) {
    console.error("explorer_get_settings", e);
  }
}

/** @type {boolean} */
let helpAboutVersionLoaded = false;

async function ensureHelpAboutVersion() {
  const el = document.getElementById("explorer-about-version");
  if (!el || helpAboutVersionLoaded) return;
  try {
    const v = await invoke("xfer64_app_version");
    el.textContent = `Version ${v}`;
    helpAboutVersionLoaded = true;
  } catch {
    el.textContent = "";
  }
}

/** @param {"setup" | "about"} tabId */
function setHelpTab(tabId) {
  const root = document.getElementById("explorer-help-modal");
  if (!root) return;
  const tabs = root.querySelectorAll('.explorer-help-tab[role="tab"]');
  const panels = root.querySelectorAll('.explorer-help-panel[role="tabpanel"]');
  for (const t of tabs) {
    const id = /** @type {HTMLElement} */ (t).dataset.helpTab;
    const sel = id === tabId;
    t.setAttribute("aria-selected", sel ? "true" : "false");
    t.tabIndex = sel ? 0 : -1;
  }
  for (const p of panels) {
    const id = /** @type {HTMLElement} */ (p).dataset.helpPanel;
    const show = id === tabId;
    /** @type {HTMLElement} */ (p).hidden = !show;
  }
  if (tabId === "about") void ensureHelpAboutVersion();
}

function setExplorerHelpOpen(open) {
  const root = document.getElementById("explorer-help-modal");
  const opener = document.getElementById("btn-explorer-help");
  if (!root) return;
  root.classList.toggle("hidden", !open);
  root.setAttribute("aria-hidden", open ? "false" : "true");
  document.body.classList.toggle("explorer-modal-open", open);
  if (open) {
    setHelpTab("setup");
    void ensureHelpAboutVersion();
    document.getElementById("explorer-help-tab-setup")?.focus();
  } else {
    opener?.focus();
  }
}

function setupExplorerHelpModal() {
  const root = document.getElementById("explorer-help-modal");
  const backdrop = root?.querySelector(".explorer-modal-backdrop");
  const btnClose = document.getElementById("explorer-help-close");
  document.getElementById("btn-explorer-help")?.addEventListener("click", () => {
    setExplorerHelpOpen(true);
  });
  btnClose?.addEventListener("click", () => setExplorerHelpOpen(false));
  backdrop?.addEventListener("click", () => setExplorerHelpOpen(false));

  root?.querySelector(".explorer-help-tablist")?.addEventListener("click", (e) => {
    const btn = e.target.closest(".explorer-help-tab[data-help-tab]");
    if (!btn) return;
    const id = /** @type {HTMLElement} */ (btn).dataset.helpTab;
    if (id === "setup" || id === "about") setHelpTab(id);
  });

  root?.querySelector(".explorer-help-tablist")?.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const tabs = [...root.querySelectorAll('.explorer-help-tab[role="tab"]')];
    const i = tabs.indexOf(document.activeElement);
    if (i < 0) return;
    e.preventDefault();
    const next = e.key === "ArrowRight" ? Math.min(i + 1, tabs.length - 1) : Math.max(i - 1, 0);
    const id = /** @type {HTMLElement} */ (tabs[next]).dataset.helpTab;
    if (id === "setup" || id === "about") {
      setHelpTab(id);
      tabs[next].focus();
    }
  });

  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape" || !root || root.classList.contains("hidden")) return;
      e.preventDefault();
      e.stopPropagation();
      setExplorerHelpOpen(false);
    },
    true
  );
}

function setupExplorerSettings() {
  document.getElementById("btn-open-settings")?.addEventListener("click", () => {
    void loadExplorerSettings().then(() => setExplorerSettingsOpen(true));
  });
  document.getElementById("btn-close-settings")?.addEventListener("click", () => {
    setExplorerSettingsOpen(false);
  });
  document.getElementById("explorer-settings-backdrop")?.addEventListener("click", () => {
    setExplorerSettingsOpen(false);
  });
  document.getElementById("explorer-developer-mode")?.addEventListener("change", updateExplorerDevShellButton);
  document.getElementById("explorer-cart-device")?.addEventListener("change", () => {
    updateEd64AdvancedSectionVisibility();
    void updateCartDeviceSettingsHint();
  });
  document.getElementById("btn-ed64-probe-linear-base")?.addEventListener("click", async () => {
    await runEd64LinearBaseScan();
  });
  document.getElementById("btn-sendto-upload")?.addEventListener("click", async () => {
    if (explorerSendToUploadOpInFlight) return;
    const btn = document.getElementById("btn-sendto-upload");
    const msg = document.getElementById("explorer-sendto-status-msg");
    const installed = btn?.dataset.installed === "1";
    if (msg) msg.textContent = installed ? "Removing…" : "Adding…";
    setExplorerSendToUploadBusy(true);
    try {
      if (installed) {
        await invoke("explorer_send_to_upload_remove");
      } else {
        await invoke("explorer_send_to_upload_install");
      }
      await refreshExplorerSendToUploadButton();
    } catch (e) {
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
    } finally {
      setExplorerSendToUploadBusy(false);
    }
  });
  document.getElementById("btn-save-explorer-settings")?.addEventListener("click", async () => {
    const developerMode = document.getElementById("explorer-developer-mode")?.checked ?? false;
    const cartDevice = document.getElementById("explorer-cart-device")?.value || "auto";
    const cartDeviceChanged = cartDevice !== explorerSettingsCartDeviceAtLoad;
    /** True when EverDrive is saved with no linear base — offer scan after Settings closes (not behind the panel). */
    let offerEd64ScanAfterSave = false;
    try {
      const prev = await invoke("explorer_get_settings");
      /** @type {Record<string, unknown>} */
      const settings = { ...prev, developerMode, cartDevice };
      if (cartDevice === "ed64_beta") {
        const rawBase = document.getElementById("explorer-ed64-linear-base")?.value ?? "";
        const parsed = parseEd64LinearBaseInput(rawBase);
        if (parsed === null) {
          await showExplorerAlert(
            "Enter a valid linear ROM address (for example hex 0x10000000 or a decimal number), or leave the field blank to disable SD browsing from this app.",
          );
          return;
        }
        settings.ed64RomLinearBase = parsed === undefined ? null : parsed;
        offerEd64ScanAfterSave = parsed === undefined;
      }
      await invoke("explorer_set_settings", { settings });
      lastExplorerSettingsCommit = buildExplorerSettingsCommit(settings);
      if (cartDeviceChanged) {
        await invoke("cart_serial_invalidate_probe_cache");
      }
    } catch (e) {
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
      return;
    }
    explorerSettingsCartDeviceAtLoad = cartDevice;
    setExplorerSettingsOpen(false);
    try {
      if (developerMode) {
        await invoke("explorer_open_dev_shell");
      }
      applyCartDeviceUi({ skipUsbRefresh: true });
      if (cartDeviceChanged) {
        await refreshUsbDetectHint();
        await loadCartPane({ forceRefresh: true });
      }
    } catch (e) {
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
    }
    if (offerEd64ScanAfterSave) {
      setTimeout(() => void maybeOfferEd64AutoScan(), 0);
    }
  });
  document.getElementById("btn-open-dev-shell")?.addEventListener("click", async () => {
    try {
      await invoke("explorer_open_dev_shell");
    } catch (e) {
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
    }
  });
  document.addEventListener("keydown", (e) => {
    const panel = document.getElementById("explorer-settings-panel");
    if (e.key === "Escape" && panel && !panel.hidden) {
      setExplorerSettingsOpen(false);
    }
  });
}

async function init() {
  const dirs = await invoke("fs_user_dirs");
  const defPc = dirs.documents || dirs.home || "";
  const savedPc = localStorage.getItem(LS_PC) || defPc;
  state.cart.path = "";
  state.cart.history = [""];
  state.cart.histIndex = 0;
  if (savedPc) {
    state.pc.path = normalizePath(savedPc);
    state.pc.history = [state.pc.path];
    state.pc.histIndex = 0;
  }

  await loadExplorerSettings();
  void refreshEd64LinearHintBases();

  await refreshUsbComPorts();
  const usbCom = localStorage.getItem(LS_USB_COM);
  if (usbCom && document.getElementById("select-usb-com")) {
    document.getElementById("select-usb-com").value = usbCom;
  }
  await syncPreferredComToBackend();
  await refreshUsbDetectHint();

  await applySavedCartFolderFromSettings();

  const btnHiddenCart = document.getElementById("btn-show-hidden-cart");
  if (btnHiddenCart) {
    const on = localStorage.getItem(LS_SHOW_HIDDEN_CART) === "1";
    btnHiddenCart.setAttribute("aria-pressed", on ? "true" : "false");
    btnHiddenCart.addEventListener("click", () => {
      const next = btnHiddenCart.getAttribute("aria-pressed") !== "true";
      btnHiddenCart.setAttribute("aria-pressed", next ? "true" : "false");
      localStorage.setItem(LS_SHOW_HIDDEN_CART, next ? "1" : "0");
      void loadCartPane();
    });
  }
  const btnHiddenPc = document.getElementById("btn-show-hidden-pc");
  if (btnHiddenPc) {
    const on = localStorage.getItem(LS_SHOW_HIDDEN_PC) === "1";
    btnHiddenPc.setAttribute("aria-pressed", on ? "true" : "false");
    btnHiddenPc.addEventListener("click", () => {
      const next = btnHiddenPc.getAttribute("aria-pressed") !== "true";
      btnHiddenPc.setAttribute("aria-pressed", next ? "true" : "false");
      localStorage.setItem(LS_SHOW_HIDDEN_PC, next ? "1" : "0");
      void loadPcPane();
    });
  }

  setupExplorerSortHeaders();
  await loadBothPanes({ forceRefresh: true });

  setupExplorerVirtualScroll("cart");
  setupExplorerVirtualScroll("pc");

  setupExplorerHelpModal();
  setupExplorerSettings();
  setupExplorerContextMenu();
  setupExplorerPropertiesModal();

  setupPaneFocusTracking();
  setupTableWrapKeyboardFocus("cart");
  setupTableWrapKeyboardFocus("pc");
  setupExplorerKeyboard();

  for (const id of ["table-wrap-cart", "table-wrap-pc"]) {
    document.getElementById(id)?.addEventListener(
      "scroll",
      () => {
        hideNameTooltip();
        hideExplorerContextMenu();
      },
      { passive: true }
    );
  }
  window.addEventListener(
    "resize",
    () => {
      hideNameTooltip();
      hideExplorerContextMenu();
      if (state.cart.listEntries.length > 0) {
        lastVirtualRange.cart = null;
        renderExplorerPane("cart");
      }
      if (state.pc.listEntries.length > 0) {
        lastVirtualRange.pc = null;
        renderExplorerPane("pc");
      }
    },
    { passive: true }
  );

  document.getElementById("addr-cart-select")?.addEventListener("change", () => {
    const v = document.getElementById("addr-cart-select").value;
    navigate("cart", v, true);
  });
  document.getElementById("addr-pc-select")?.addEventListener("change", () => {
    const v = document.getElementById("addr-pc-select").value.trim();
    if (!v) return;
    navigate("pc", v, true);
  });

  document.getElementById("select-usb-com")?.addEventListener("change", async () => {
    const v = document.getElementById("select-usb-com").value;
    localStorage.setItem(LS_USB_COM, v);
    await syncPreferredComToBackend();
    await refreshUsbDetectHint();
    await loadCartPane({ forceRefresh: true });
  });

  document.querySelectorAll("[data-action]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const action = btn.dataset.action;
      const pane = btn.dataset.pane;
      if (action === "back") goBack(pane);
      else if (action === "up") await goUp(pane);
      else if (action === "refresh") {
        if (pane === "cart") {
          await refreshUsbComPorts();
          await refreshUsbDetectHint();
          await loadCartPane({ forceRefresh: true });
        } else await loadPcPane({ forceRefresh: true });
      } else if (action === "pick") {
        const picked = await invoke("pick_folder");
        if (picked) navigate(pane, picked, true);
      }
    });
  });

  document.getElementById("btn-copy-to-pc")?.addEventListener("click", async () => {
    const paths = [...state.cart.selected];
    if (paths.length === 0) {
      await showExplorerAlert(
        "Select one or more items on the SD card to export.\n\nTip: Ctrl+click, Shift+click, or drag to select multiple items."
      );
      return;
    }
    await copyCartToPcPaths(paths);
  });

  document.getElementById("btn-copy-to-cart")?.addEventListener("click", async () => {
    const paths = [...state.pc.selected];
    if (paths.length === 0) {
      await showExplorerAlert(
        "Select one or more items in the Windows folder to import.\n\nTip: Ctrl+click, Shift+click, or drag to select multiple items."
      );
      return;
    }
    await copyPcToCartPaths(paths);
  });

  document.getElementById("btn-delete-cart")?.addEventListener("click", async () => {
    await deleteSelectedCart(true);
  });

  document.getElementById("btn-delete-pc")?.addEventListener("click", async () => {
    await deleteSelectedPc(true);
  });

  document.getElementById("btn-mkdir-cart")?.addEventListener("click", async () => {
    await promptMkdirCart();
  });

  document.getElementById("btn-mkdir-pc")?.addEventListener("click", async () => {
    await promptMkdirPc();
  });

  document.getElementById("btn-rename-cart")?.addEventListener("click", async () => {
    await promptRenameCart();
  });

  document.getElementById("btn-rename-pc")?.addEventListener("click", async () => {
    await promptRenamePc();
  });

  setupTableWrapMarquee("cart");
  setupTableWrapMarquee("pc");
  setupTableWrapDrop("cart");
  setupTableWrapDrop("pc");

  for (const pane of ["cart", "pc"]) {
    document.getElementById(`explorer-operation-cancel-${pane}`)?.addEventListener("click", () => {
      requestProgressCancel();
    });
  }

  document.addEventListener("dragend", () => {
    document.querySelectorAll(".explorer-table-wrap.drag-over-target").forEach((w) => w.classList.remove("drag-over-target"));
    document.querySelectorAll("tbody tr.drag-over-drop-target").forEach((r) => r.classList.remove("drag-over-drop-target"));
  });

  setupUsbSerialHotplug();
}

window.addEventListener("DOMContentLoaded", () => {
  setupExplorerGlobalContextMenuSuppression();
  void init();
});
