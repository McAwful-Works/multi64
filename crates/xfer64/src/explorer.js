import { countNoun, userFacingErrorMessage } from "./user-error.js";
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
      (next.cartDevice === "ed64_beta" && lastAutoUsbCartKind === "ed64") ||
      (next.cartDevice === "ed64_pro" && lastAutoUsbCartKind === "ed64pro"))
  );
}

/** @param {string | undefined} raw */
function normalizeCartDeviceSetting(raw) {
  const r = (raw || "").trim();
  if (r === "ed64_beta") return "ed64_beta";
  if (r === "ed64_pro") return "ed64_pro";
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
 * @param {{ confirm?: boolean }} [opts]
 *   - `confirm: false` (default) — no dialog before release/resume.
 *   - `confirm: true` — ask before releasing the daemon (copy/mkdir/rename).
 * @returns {Promise<boolean>} `true` if the user cancelled the preflight dialog (only when `confirm` is true).
 */
async function withCartDaemonYield(fn, opts = {}) {
  const confirm = opts.confirm === true;
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
    const ok = await showExplorerConfirm(
      "The Multi64 bridge is using this cart on the same serial port.\n\nIt will pause while this finishes, then resume. Continue?",
      { title: "Pause the Multi64 bridge?", okLabel: "Continue" }
    );
    if (!ok) return true;
  }
  await invoke("explorer_daemon_release", { listen });
  // Released and resumed separately so a resume failure can be reported without masking
  // the operation's own error. If resume fails, multi64d keeps ignoring WebSocket writes
  // and the bridge is silently dead -- the user has to be told.
  let fnError = null;
  try {
    await fn();
  } catch (e) {
    fnError = e;
  }
  let resumeError = null;
  try {
    await invoke("explorer_daemon_resume", { listen });
  } catch (e) {
    resumeError = e;
  }
  if (resumeError) {
    await showExplorerAlert(
      `The Multi64 bridge was paused for this operation and could not be resumed:

${String(
        resumeError
      )}

The bridge is not using the cart until it resumes — use Restart bridge in Multi64.`,
      { title: "Multi64 bridge not resumed" }
    );
  }
  if (fnError) throw fnError;
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

/* ---------------------------------------------------------------------------
   Drag and drop
   ---------------------------------------------------------------------------
   The main window sets `dragDropEnabled: true` (`tauri.conf.json`), so the OS hands us absolute
   paths when files are dropped from Explorer — WebView2's own HTML5 drop reports no path at all,
   and the copy planners need one. The price is that HTML5 drag-and-drop stops working inside the
   webview on Windows, which is why nothing here uses it. Three directions, three mechanisms:

   - **In** (Explorer → Xfer64): `tauri://drag-*` events — `setupOsFileDrops`.
   - **Between panes**: pointer events — `bindRowPointerDrag`.
   - **Out** (Xfer64 → Explorer): `plugin:drag|start_drag`. Windows-pane rows carry real paths and
     go straight out; cart rows do not exist on disk, so the selection is exported to a staging
     directory first and the drag starts on the *next* gesture — `startCartDragOut`.

   A drop lands in the folder row under the pointer, or in the pane's current folder when there is
   no row there. Same rule in every direction.
   --------------------------------------------------------------------------- */

/** Pointer travel that turns a press into a drag rather than a click. */
const DRAG_START_THRESHOLD_PX = 5;

/**
 * How far past the window edge the pointer must go before a drag is handed to the shell.
 * Brushing the edge on the way between panes must not start a drag-out — from the cart pane that
 * would kick off a staging export measured in tens of seconds.
 */
const DRAG_OUT_MARGIN_PX = 12;

/** Band at a list's top/bottom edge where a drag scrolls it. */
const DRAG_AUTOSCROLL_EDGE_PX = 28;
const DRAG_AUTOSCROLL_STEP_PX = 20;
const DRAG_AUTOSCROLL_INTERVAL_MS = 50;

/** `plugin:drag|start_drag` requires a drag image: a 32×32 card in the accent colour. */
const OS_DRAG_IMAGE_PNG = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAbUlEQVR42mNgGAVDAXRt+fGaHDwgllLFMTBNll7Z76iBSXIEtS0n2RG0sBzZEYPbAbQKfqKjYdA4QFnD4AUt8KgDRh0w6oBRB4w6YPA7AOYIWjqAqPbAgDuAFo4gq104IJYPimb5oOiYjAJ6AQCeK5lnXii7XgAAAABJRU5ErkJggg==";

/** The plugin's own callback clears the in-flight flag; this only bounds a callback that never comes. */
const OS_DRAG_WATCHDOG_MS = 60000;

/** The live pane-to-pane drag, or null. */
let pointerDrag = null;

/** The highlighted drop target, so it can be cleared without sweeping the DOM. */
let dropHighlight = null;

/** True while a drag we started belongs to the OS: its events over our own window are not drops. */
let osDragInFlight = false;
let osDragWatchdogTimer = null;

/** Swallows the click that ends a drag, which would otherwise reshuffle the selection. */
let suppressNextRowClick = false;

/** The cart selection most recently exported for a drag-out; staging a new one replaces it. */
let cartDragStaged = null;

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

/**
 * Message to show for a backend-reported cancellation.
 *
 * Cancellation is detected by substring, so the error often carries more than the bare word --
 * an interrupted overwrite reports that the file on the cart is now incomplete. Replacing every
 * such message with a flat "Cancelled." hid exactly the part the user needed to see.
 *
 * Every cancelled operation finishes as "<Operation> cancelled." (e.g. "Export cancelled."); a
 * backend detail that starts with "Cancelled" keeps its detail under that same lead.
 * @param {unknown} e
 * @param {string} operation noun for the operation, e.g. "Export", "Import", "Copy", "Delete"
 */
function cancelMessageFor(e, operation) {
  const raw = String(e && e.message ? e.message : e).trim();
  const stripped = raw.replace(/^Error:\s*/i, "").trim();
  if (!stripped || /^cancelled[.]?$/i.test(stripped)) return `${operation} cancelled.`;
  if (/^cancelled\b/i.test(stripped)) return stripped.replace(/^cancelled\b/i, `${operation} cancelled`);
  return stripped;
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
 * Human-readable copy action for the progress line. Names the destination the same way everywhere.
 * @param {"fs" | "export" | "import"} mode
 */
function copyActionLabel(mode) {
  if (mode === "import") return "Importing to cart";
  if (mode === "export") return "Exporting to This PC";
  return "Copying";
}

/**
 * Operation noun for a copy's cancel line ("Export cancelled.").
 * @param {"fs" | "export" | "import"} mode
 */
function copyOperationNoun(mode) {
  if (mode === "import") return "Import";
  if (mode === "export") return "Export";
  return "Copy";
}

/**
 * How a finished message names what it acted on: the quoted name for one path, otherwise a count —
 * "files" when every path is a known file in `pane`, "items" when folders are or may be mixed in.
 * @param {"cart" | "pc"} pane
 * @param {string[]} paths
 */
function selectionLabel(pane, paths) {
  if (paths.length === 1) return `"${basenameForMessage(paths[0])}"`;
  const byPath = new Map(state[pane].listEntries.map((e) => [e.path, e]));
  const allFiles = paths.every((p) => byPath.get(p)?.isDir === false);
  return countNoun(paths.length, allFiles ? "file" : "item");
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
 * One progress format for a whole copy, first step to last: `Exporting to This PC — "a.z64" (2 of 5)…`.
 * @param {Record<string, unknown>} step
 * @param {"fs" | "export" | "import"} mode
 * @param {string} [action] progress lead; defaults to `copyActionLabel(mode)`
 */
function formatCopyProgressMessage(step, mode, i, n, action = copyActionLabel(mode)) {
  const name = currentItemLabelForCopyStep(step, mode);
  if (n <= 1) return `${action} — "${name}"…`;
  return `${action} — "${name}" (${i + 1} of ${n})…`;
}

/**
 * Same shape as `formatCopyProgressMessage`, for a step the user chose to skip.
 * @param {Record<string, unknown>} step
 * @param {"fs" | "export" | "import"} mode
 * @param {string} [action]
 */
function formatSkipProgressMessage(step, mode, i, n, action = copyActionLabel(mode)) {
  const name = currentItemLabelForCopyStep(step, mode);
  if (n <= 1) return `${action} — skipping "${name}"…`;
  return `${action} — skipping "${name}" (${i + 1} of ${n})…`;
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
  otherRoot.hidden = true;
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
  root.hidden = false;
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

/** Avoid flashing the list overlay when directory listing returns almost immediately. */
const PANE_SHADE_DELAY_MS = 120;

/** An operation's overlay stays undrawn this long, so a quick rename or delete does not flash it. */
const PANE_BUSY_SHOW_DELAY_MS = 300;

/** Whether a pane's folder load has run long enough for its shade to be drawn. */
const paneLoadingShown = { cart: false, pc: false };

/**
 * Operations running per pane (`beginBusy`), and whether their overlay is drawn yet.
 * @type {Record<"cart" | "pc", { depth: number, shown: boolean, timer: ReturnType<typeof setTimeout> | null, text: string }>}
 */
const paneBusy = {
  cart: { depth: 0, shown: false, timer: null, text: "" },
  pc: { depth: 0, shown: false, timer: null, text: "" },
};

/** Whether the cart's last listing succeeded: Import is offered only for a cart that answered. */
let cartReady = false;

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
  let fsLabel = "";
  for (;;) {
    const page = await invoke("cart_serial_list_dir_page", {
      path: listPath,
      offset,
      limit: lim,
      fresh: forceRefresh && offset === 0,
    });
    exfat = page.exfat;
    fsLabel = page.fsLabel || "";
    all.push(...page.entries);
    offset += page.entries.length;
    if (offset >= page.total || page.entries.length === 0) break;
  }
  return { entries: all, exfat, fsLabel };
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
function isPaneBusy(pane) {
  return paneBusy[pane].depth > 0;
}

/**
 * Draw a pane's list shade from its folder-load and busy state.
 *
 * A busy pane always has the shade in place, so input to the list is blocked from the first
 * moment; it is drawn only once the busy delay has passed (until then it carries
 * `explorer-table-shade--pending`). While busy, a folder load inside the operation does not draw
 * its own shade — the operation's delay decides.
 * @param {"cart" | "pc"} pane
 */
function syncPaneShade(pane) {
  const shade = document.getElementById(`table-shade-${pane}`);
  const textEl = document.getElementById(`table-shade-text-${pane}`);
  const wrapEl = document.getElementById(`table-wrap-${pane}`);
  if (!shade) return;
  const busy = paneBusy[pane];
  const loading = paneLoadingDepth[pane] > 0 && paneLoadingShown[pane];
  const visible = busy.depth > 0 ? busy.shown : loading;
  if (textEl && visible) textEl.textContent = busy.depth > 0 ? busy.text : paneShadePendingText[pane];
  shade.hidden = !visible && busy.depth === 0;
  shade.classList.toggle("explorer-table-shade--pending", !visible);
  shade.setAttribute("aria-hidden", visible ? "false" : "true");
  if (wrapEl) wrapEl.setAttribute("aria-busy", busy.depth > 0 || loading ? "true" : "false");
}

/** @param {"cart" | "pc"} pane */
function beginPaneLoading(pane, text = "Reading folder…") {
  paneLoadingDepth[pane]++;
  paneShadePendingText[pane] = text;
  if (paneLoadingDepth[pane] === 1) {
    clearPaneShadeDelayTimer(pane);
    paneLoadingShown[pane] = false;
    paneShadeDelayTimer[pane] = setTimeout(() => {
      paneShadeDelayTimer[pane] = null;
      if (paneLoadingDepth[pane] > 0) {
        paneLoadingShown[pane] = true;
        syncPaneShade(pane);
      }
    }, PANE_SHADE_DELAY_MS);
  }
}

/** @param {"cart" | "pc"} pane */
function endPaneLoading(pane) {
  paneLoadingDepth[pane] = Math.max(0, paneLoadingDepth[pane] - 1);
  if (paneLoadingDepth[pane] === 0) {
    clearPaneShadeDelayTimer(pane);
    paneLoadingShown[pane] = false;
  }
  syncPaneShade(pane);
}

/**
 * Mark an operation as running on `pane`, and return the function that ends it.
 *
 * Input is blocked at once: the list shade goes in place, and row drags, double-clicks, drops,
 * shortcuts and the pane's buttons refuse while `isPaneBusy(pane)`. The overlay itself is drawn
 * only if the operation is still running after `PANE_BUSY_SHOW_DELAY_MS`, so a quick one shows
 * nothing. Overlapping operations are counted; calling the returned `end()` again does nothing.
 * @param {"cart" | "pc"} pane
 * @param {string} [text] caption shown on the overlay
 * @returns {() => void}
 */
function beginBusy(pane, text = "Working…") {
  const busy = paneBusy[pane];
  busy.depth++;
  busy.text = text;
  if (busy.depth === 1) {
    busy.shown = false;
    busy.timer = setTimeout(() => {
      busy.timer = null;
      if (busy.depth > 0) {
        busy.shown = true;
        syncPaneShade(pane);
      }
    }, PANE_BUSY_SHOW_DELAY_MS);
  }
  syncPaneShade(pane);
  updateExplorerControls();
  let ended = false;
  return () => {
    if (ended) return;
    ended = true;
    busy.depth = Math.max(0, busy.depth - 1);
    if (busy.depth === 0) {
      if (busy.timer != null) clearTimeout(busy.timer);
      busy.timer = null;
      busy.shown = false;
    }
    syncPaneShade(pane);
    updateExplorerControls();
  };
}

/** Hide the copy/delete progress strip without a completion toast (e.g. empty copy plan). */
function hideOperationProgressPane(pane) {
  clearOperationHideTimer();
  const root = document.getElementById(`explorer-operation-${pane}`);
  const fill = document.getElementById(`explorer-operation-fill-${pane}`);
  if (!root || !fill) return;
  root.hidden = true;
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
/** Pane reloads requested by progress events, deferred until the operation releases the port. */
const pendingPaneRefresh = { cart: false, pc: false };

async function flushPendingPaneRefresh() {
  const wantCart = pendingPaneRefresh.cart;
  const wantPc = pendingPaneRefresh.pc;
  pendingPaneRefresh.cart = false;
  pendingPaneRefresh.pc = false;
  try {
    if (wantCart) await loadCartPane({ preserveSelection: true, forceRefresh: true });
    if (wantPc) await loadPcPane({ preserveSelection: true, forceRefresh: true });
  } catch {
    /* the caller reports the operation's own error; a refresh failure must not mask it */
  }
}

async function runWithProgress(pane, message, fn) {
  resetProgressCancel();
  pendingPaneRefresh.cart = false;
  pendingPaneRefresh.pc = false;
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
        // Do NOT reload here. These events arrive while the backend still holds the cart's
        // SD session, so a reload would try to open the same exclusive COM port and fail,
        // blanking the pane. Record the request and run it once, after the session closes.
        if (payload.refreshCart) pendingPaneRefresh.cart = true;
        if (payload.refreshPc) pendingPaneRefresh.pc = true;
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
    // The backend session is closed by now, so the port is free for a reload.
    await flushPendingPaneRefresh();
  }
}

/** @param {"cart" | "pc"} pane */
function finishOperationProgress(message, isError = false, pane) {
  clearOperationHideTimer();
  const root = document.getElementById(`explorer-operation-${pane}`);
  const text = document.getElementById(`explorer-operation-text-${pane}`);
  const fill = document.getElementById(`explorer-operation-fill-${pane}`);
  if (!root || !text || !fill) return;
  root.hidden = false;
  root.setAttribute("aria-hidden", "false");
  root.classList.toggle("explorer-operation--done", !isError);
  root.classList.toggle("explorer-operation--error", !!isError);
  root.classList.remove("explorer-operation--cancelled");
  text.textContent = message;
  fill.classList.remove("indeterminate");
  fill.style.width = "100%";
  operationHideTimer = setTimeout(() => {
    root.hidden = true;
    root.setAttribute("aria-hidden", "true");
    fill.style.width = "0";
    fill.classList.remove("indeterminate");
    root.classList.remove(
      "explorer-operation--done",
      "explorer-operation--error",
      "explorer-operation--cancelled",
    );
    operationHideTimer = null;
  }, OP_HIDE_MS);
}

/**
 * For the reload a cancel branch runs before `finishOperationCancelled`: a reload that fails must
 * not leave the strip stuck on the operation's progress.
 * @param {unknown} e
 */
function logReloadAfterCancelError(e) {
  console.error("Reload after a cancelled operation failed:", e);
}

/** Finish the operation strip for a cancellation: shown in amber, since nothing finished. */
function finishOperationCancelled(message, pane) {
  finishOperationProgress(message, false, pane);
  const root = document.getElementById(`explorer-operation-${pane}`);
  if (!root) return;
  root.classList.remove("explorer-operation--done");
  root.classList.add("explorer-operation--cancelled");
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
  const endBusy = beginBusy("cart", "Renaming…");
  try {
    const cancelled = await withCartDaemonYield(
      async () => {
        showOperationProgress(`Renaming on cart — "${newNameTrimmed}"…`, "cart");
        await invokeCartWrite("cart_serial_rename_cart", { from: normalizeUsbPath(fromPath), to: toPath });
        await loadCartPane();
      },
      { confirm: true }
    );
    if (cancelled) return;
    finishOperationProgress(`Renamed to "${newNameTrimmed}".`, false, "cart");
  } finally {
    endBusy();
  }
}

/**
 * @param {string} fromPath
 * @param {string} newNameTrimmed
 */
async function runRenamePcFromPaths(fromPath, newNameTrimmed) {
  const parent = dirnameWin(fromPath);
  const toPath = `${parent}${newNameTrimmed}`;
  const endBusy = beginBusy("pc", "Renaming…");
  try {
    showOperationProgress(`Renaming on This PC — "${newNameTrimmed}"…`, "pc");
    await invoke("fs_rename", { from: fromPath, to: toPath });
    await loadPcPane();
    finishOperationProgress(`Renamed to "${newNameTrimmed}".`, false, "pc");
  } finally {
    endBusy();
  }
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
  optRoot.title = "Cart root";
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
    (th.querySelector(".explorer-sort-btn") || th).setAttribute(
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
    // The `th` keeps its column-header role, which is what makes its `aria-sort` valid; the button
    // inside it is what a click, Enter or Space presses.
    for (const th of table.querySelectorAll("thead th[data-sort-key]")) {
      th.querySelector(".explorer-sort-btn")?.addEventListener("click", () => {
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
const MODAL_OK_LABEL_DEFAULT = "OK";

const FOCUSABLE_SELECTOR =
  'a[href], button, input:not([type="hidden"]), select, textarea, summary, [tabindex]';

/**
 * The elements Tab can reach inside `root`, in document order: rendered, not disabled, not inside a
 * `[hidden]` ancestor, and `tabindex` not negative.
 * @param {Element} root
 * @returns {HTMLElement[]}
 */
function focusableIn(root) {
  return /** @type {HTMLElement[]} */ ([...root.querySelectorAll(FOCUSABLE_SELECTOR)]).filter(
    (el) =>
      !el.matches(":disabled") &&
      el.tabIndex >= 0 &&
      !el.closest("[hidden], [inert]") &&
      el.getClientRects().length > 0 &&
      getComputedStyle(el).visibility !== "hidden"
  );
}

/**
 * Open dialogs, bottom first. Only the last one takes Tab, Escape and Enter, so a key pressed in a
 * dialog never reaches one underneath it.
 * @type {{ panel: HTMLElement, onEscape: () => void, onEnter: ((e: KeyboardEvent) => void) | null, prevFocus: HTMLElement | null, initialFocus: () => HTMLElement | null, fallbackFocus: () => HTMLElement | null }[]}
 */
const dialogFocusStack = [];

/** @param {HTMLElement | null | (() => HTMLElement | null) | undefined} target */
function resolveFocusTarget(target) {
  return typeof target === "function" ? target() : target ?? null;
}

/** Focus `el` and report whether it took focus (it may be hidden, disabled or detached). */
function tryFocus(el) {
  if (!el || !el.isConnected) return false;
  el.focus();
  return document.activeElement === el;
}

/** @param {KeyboardEvent} e */
function onDialogStackKeyDown(e) {
  const top = dialogFocusStack[dialogFocusStack.length - 1];
  if (!top) return;
  if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    top.onEscape();
    return;
  }
  if (e.key === "Enter") {
    top.onEnter?.(e);
    return;
  }
  if (e.key !== "Tab"|| e.ctrlKey || e.altKey || e.metaKey) return;
  const items = focusableIn(top.panel);
  const active = document.activeElement;
  if (items.length === 0) {
    e.preventDefault();
    tryFocus(top.panel);
    return;
  }
  const first = items[0];
  const last = items[items.length - 1];
  const inside = active instanceof Node && top.panel.contains(active);
  if (e.shiftKey) {
    // Leave the browser's own order alone unless it would carry focus out of the dialog.
    const hasEarlier = inside && active !== first && items.some((el) => el.compareDocumentPosition(active) & Node.DOCUMENT_POSITION_FOLLOWING);
    if (!hasEarlier) {
      e.preventDefault();
      last.focus();
    }
  } else {
    const hasLater = inside && active !== last && items.some((el) => el.compareDocumentPosition(active) & Node.DOCUMENT_POSITION_PRECEDING);
    if (!hasLater) {
      e.preventDefault();
      first.focus();
    }
  }
}

let dialogStackListenerInstalled = false;

/**
 * Make `panel` the topmost dialog: focus moves into it, Tab and Shift+Tab stay inside it, and Escape
 * runs `onEscape` (what its Cancel or close does). `onEnter`, when given, gets Enter while this is
 * the topmost dialog, and only then; it calls `preventDefault` itself if it acts. Call the returned
 * `release` when it closes: focus goes back to what had it before, or to `fallbackFocus`, or into
 * the dialog underneath.
 * @param {HTMLElement} panel the `role="dialog"` element
 * @param {{ initialFocus?: HTMLElement | null | (() => HTMLElement | null), onEscape: () => void, onEnter?: (e: KeyboardEvent) => void, fallbackFocus?: HTMLElement | null | (() => HTMLElement | null) }} opts
 * @returns {(opts?: { restoreFocus?: boolean }) => void} release
 */
function trapDialogFocus(panel, opts) {
  if (!dialogStackListenerInstalled) {
    dialogStackListenerInstalled = true;
    document.addEventListener("keydown", onDialogStackKeyDown, true);
  }
  const active = document.activeElement;
  const entry = {
    panel,
    onEscape: opts.onEscape,
    onEnter: opts.onEnter ?? null,
    prevFocus: active instanceof HTMLElement && active !== document.body && !panel.contains(active) ? active : null,
    initialFocus: () => resolveFocusTarget(opts.initialFocus) || focusableIn(panel)[0] || panel,
    fallbackFocus: () => resolveFocusTarget(opts.fallbackFocus),
  };
  dialogFocusStack.push(entry);
  tryFocus(entry.initialFocus());
  let released = false;
  return ({ restoreFocus = true } = {}) => {
    if (released) return;
    released = true;
    const i = dialogFocusStack.indexOf(entry);
    if (i >= 0) dialogFocusStack.splice(i, 1);
    if (!restoreFocus || i !== dialogFocusStack.length) return;
    const under = dialogFocusStack[dialogFocusStack.length - 1];
    const allowed = (el) => el != null && (!under || under.panel.contains(el));
    const prev = entry.prevFocus;
    if (allowed(prev) && tryFocus(prev)) return;
    const fallback = entry.fallbackFocus();
    if (allowed(fallback) && tryFocus(fallback)) return;
    if (under) tryFocus(under.initialFocus());
  };
}

/** True while any dialog (Settings included) holds focus. */
function isAnyExplorerDialogOpen() {
  return dialogFocusStack.length > 0;
}

function isExplorerModalOpen() {
  const root = document.getElementById("explorer-modal-root");
  const ow = document.getElementById("explorer-overwrite-modal");
  const help = document.getElementById("explorer-help-modal");
  const props = document.getElementById("explorer-properties-modal");
  return (
    (root != null && !root.hidden) ||
    (ow != null && !ow.hidden) ||
    (help != null && !help.hidden) ||
    (props != null && !props.hidden)
  );
}

function isExplorerContextMenuVisible() {
  const menu = document.getElementById("explorer-context-menu");
  return menu != null && !menu.hidden;
}

/** What had focus when the context menu opened (the list, usually); it gets focus back. */
let contextMenuReturnFocus = /** @type {HTMLElement | null} */ (null);

/**
 * Where the menu's own list was scrolled when the menu opened. Scrolling a list closes the menu, but
 * opening it from the keyboard can first scroll its row into view, and that scroll's event arrives
 * once the menu is already up. A scroll event that leaves the list at this position is that one.
 * @type {{ wrap: HTMLElement, top: number, left: number } | null}
 */
let contextMenuOpenScroll = null;

/**
 * @param {{ restoreFocus?: boolean }} [opts] `restoreFocus`: return focus to where the menu opened
 *   from. A click elsewhere closes it without, so the click's own target keeps focus.
 */
function hideExplorerContextMenu({ restoreFocus = false } = {}) {
  const menu = document.getElementById("explorer-context-menu");
  if (!menu) return;
  const wasOpen = !menu.hidden;
  menu.hidden = true;
  menu.setAttribute("aria-hidden", "true");
  const target = contextMenuReturnFocus;
  contextMenuReturnFocus = null;
  if (restoreFocus && wasOpen && !tryFocus(target)) {
    tryFocus(document.getElementById(`table-wrap-${menu.dataset.pane || focusedPane}`));
  }
}

/** @param {HTMLElement} menu @returns {HTMLButtonElement[]} the items that can run, in order */
function contextMenuItems(menu) {
  return /** @type {HTMLButtonElement[]} */ ([...menu.querySelectorAll('[role="menuitem"]')]).filter(
    (el) => !el.disabled && !el.closest("[hidden]")
  );
}

/**
 * Keys while the context menu is open: arrows, Home and End move between enabled items (wrapping),
 * Enter and Space run one, Escape and Tab close it. Nothing else reaches the lists behind it.
 * @param {KeyboardEvent} ev
 */
function onExplorerContextMenuKeyDown(ev) {
  const menu = document.getElementById("explorer-context-menu");
  if (!menu) return;
  const items = contextMenuItems(menu);
  const i = items.indexOf(/** @type {HTMLButtonElement} */ (document.activeElement));
  const key = ev.key;
  if (key === "Escape" || key === "Tab") {
    ev.preventDefault();
    hideExplorerContextMenu({ restoreFocus: true });
    return;
  }
  if (key === "ArrowDown" || key === "ArrowUp" || key === "Home" || key === "End") {
    ev.preventDefault();
    if (!items.length) return;
    let next;
    if (key === "Home") next = 0;
    else if (key === "End") next = items.length - 1;
    else if (key === "ArrowDown") next = i < 0 ? 0 : (i + 1) % items.length;
    else next = i < 0 ? items.length - 1 : (i - 1 + items.length) % items.length;
    items[next].focus();
    return;
  }
  if (key === "Enter" || key === " ") {
    ev.preventDefault();
    if (i >= 0) items[i].click();
    return;
  }
  if (key === "ContextMenu" || key === "F10") ev.preventDefault();
}

/**
 * Shift+F10 or the ContextMenu key: the menu for the selected row of the active list, or the list's
 * own menu when nothing is selected, placed by the row instead of a pointer.
 * @param {"cart" | "pc"} pane
 */
function openExplorerContextMenuFromKeyboard(pane) {
  if (isExplorerSettingsOpen() || isExplorerModalOpen() || inlineRenameState || isPaneBusy(pane)) return;
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;
  focusedPane = pane;
  cancelRenameNameClickArm();
  const sel = state[pane].selected;
  if (sel.size > 0) {
    const anchor = state[pane].anchorPath;
    const path = anchor && sel.has(anchor) ? anchor : [...sel][0];
    ensurePathVisibleInPane(pane, path, { onlyIfHidden: true });
    const r = (findRowInPaneDom(pane, path) || wrap).getBoundingClientRect();
    showExplorerContextMenu(pane, r.left + 24, r.bottom, false);
    return;
  }
  const r = wrap.getBoundingClientRect();
  showExplorerContextMenu(pane, r.left + 24, r.top + 24, true);
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
  if (menu.hidden) {
    const active = document.activeElement;
    contextMenuReturnFocus =
      active instanceof HTMLElement && active !== document.body && !menu.contains(active)
        ? active
        : document.getElementById(`table-wrap-${pane}`);
  }
  const fileGroup = document.getElementById("explorer-context-menu-group-file");
  const blankGroup = document.getElementById("explorer-context-menu-group-blank");
  if (fileGroup && blankGroup) {
    fileGroup.hidden = blankArea;
    fileGroup.setAttribute("aria-hidden", blankArea ? "true" : "false");
    blankGroup.hidden = !blankArea;
    blankGroup.setAttribute("aria-hidden", blankArea ? "false" : "true");
    if (blankArea) updateBlankContextMenuItems(pane);
    else updateExplorerContextMenuItems(menu, pane);
  }
  menu.dataset.pane = pane;
  const paneWrap = document.getElementById(`table-wrap-${pane}`);
  contextMenuOpenScroll = paneWrap ? { wrap: paneWrap, top: paneWrap.scrollTop, left: paneWrap.scrollLeft } : null;
  const copyBtn = menu.querySelector('[data-ctx="copy"]');
  if (copyBtn) {
    copyBtn.textContent = pane === "cart" ? "Export to This PC" : "Import to cart";
  }
  menu.hidden = false;
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
  // The first item that can run takes focus; the menu itself when none can.
  (contextMenuItems(menu)[0] || menu).focus();
}

/**
 * @param {HTMLElement} menu
 * @param {"cart" | "pc"} pane
 */
function updateExplorerContextMenuItems(menu, pane) {
  const sel = state[pane].selected;
  const single = sel.size === 1;
  let onlyIsDir = false;
  if (single) {
    const onlyPath = [...sel][0];
    const entry = state[pane].listEntries.find((e) => e.path === onlyPath);
    onlyIsDir = entry ? entry.isDir : false;
  }
  setMenuItemEnabled(menu.querySelector('[data-ctx="copy"]'), actionBlockedReason("transfer", pane));
  setMenuItemEnabled(menu.querySelector('[data-ctx="delete"]'), actionBlockedReason("delete", pane));
  setMenuItemEnabled(menu.querySelector('[data-ctx="rename"]'), actionBlockedReason("rename", pane));
  setMenuItemEnabled(menu.querySelector('[data-ctx="open"]'), single && onlyIsDir ? "" : "select one folder");
  setMenuItemEnabled(menu.querySelector('[data-ctx="properties"]'), single ? "" : "select one item");
}

/**
 * @param {"cart" | "pc"} pane
 */
function updateBlankContextMenuItems(pane) {
  const menu = document.getElementById("explorer-context-menu");
  const noPcFolder = pane === "pc" && !normalizePath(state.pc.path || "");
  setMenuItemEnabled(menu?.querySelector('[data-ctx="new-folder"]'), actionBlockedReason("mkdir", pane));
  setMenuItemEnabled(
    menu?.querySelector('[data-ctx="folder-properties"]'),
    noPcFolder ? "choose a folder on This PC first" : ""
  );
}

/**
 * Enable or disable a context-menu item; the reason it is off, if any, is its tooltip.
 * @param {Element | null | undefined} btn
 * @param {string} reason from `actionBlockedReason`, or "" when the item can run
 */
function setMenuItemEnabled(btn, reason) {
  if (!(btn instanceof HTMLButtonElement)) return;
  btn.disabled = reason !== "";
  btn.setAttribute("aria-disabled", reason ? "true" : "false");
  btn.title = reason ? reason.charAt(0).toUpperCase() + reason.slice(1) : "";
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
  showExplorerPropertiesModal(pane);
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
}

/** @type {((opts?: { restoreFocus?: boolean }) => void) | null} */
let releasePropertiesFocus = null;

/**
 * Show the Properties dialog (its contents are filled in by the caller) and move focus to Close.
 * @param {"cart" | "pc"} pane the pane it describes; focus falls back to that list when it closes
 */
function showExplorerPropertiesModal(pane) {
  const root = document.getElementById("explorer-properties-modal");
  const panel = root?.querySelector('[role="dialog"]');
  if (!root || !(panel instanceof HTMLElement)) return;
  root.hidden = false;
  root.setAttribute("aria-hidden", "false");
  document.body.classList.add("explorer-modal-open");
  if (releasePropertiesFocus) return;
  releasePropertiesFocus = trapDialogFocus(panel, {
    initialFocus: document.getElementById("explorer-properties-close"),
    onEscape: () => closeExplorerPropertiesModal(),
    fallbackFocus: document.getElementById(`table-wrap-${pane}`),
  });
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
  showExplorerPropertiesModal(pane);
  try {
    if (pane === "cart") {
      const path = normalizeUsbPath(state.cart.path);
      if (!path) {
        fillExplorerPropertiesDl(
          dl,
          {
            name: "(Cart root)",
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
        errDd.textContent = BROWSE_FIRST_MSG;
        dl.appendChild(errDt);
        dl.appendChild(errDd);
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
}

function closeExplorerPropertiesModal() {
  const root = document.getElementById("explorer-properties-modal");
  if (!root || root.hidden) return;
  root.hidden = true;
  root.setAttribute("aria-hidden", "true");
  document.body.classList.remove("explorer-modal-open");
  const release = releasePropertiesFocus;
  releasePropertiesFocus = null;
  release?.();
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
      // Focus goes back to the list first, so a dialog the item opens returns focus there too.
      hideExplorerContextMenu({ restoreFocus: true });
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
        if (pane === "cart") void deleteSelectedCart();
        else void deleteSelectedPc();
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
      if (!menu || menu.hidden) return;
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
  if (isExplorerSettingsOpen()) return;
  if (isExplorerModalOpen()) return;
  if (inlineRenameState) return;
  if (isPaneBusy(pane)) return;
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
  // Escape is handled by trapDialogFocus while the dialog is open.
}

/**
 * @param {string} message
 * @param {{ title?: string }} [opts] `title` defaults to "Xfer64".
 */
function showExplorerAlert(message, opts = {}) {
  return showExplorerModal({ type: "alert", message, title: opts.title });
}

/**
 * @param {string} message
 * @param {{ title?: string, okLabel?: string }} [opts]
 *   `title` defaults to "Xfer64" and `okLabel` to "OK". Destructive confirms should pass both: a
 *   title that says what the dialog is about, and a button that names the action ("Delete").
 */
function showExplorerConfirm(message, opts = {}) {
  return showExplorerModal({ type: "confirm", message, title: opts.title, okLabel: opts.okLabel });
}

/** Start of the backend's refusal to write to an EverDrive-64 PRO before consent (`ED64PRO_WRITE_CONSENT_MARKER`). */
const ED64PRO_WRITE_CONSENT_MARKER = "ED64PRO_WRITE_CONSENT_REQUIRED";

const ED64PRO_WRITE_WARNING =
  "Writing to an EverDrive-64 PRO is experimental. Xfer64's support for it is ported from Krikzz's published sources and has never been tested on a real cart, so a write could fail partway or damage files on the SD card.\n\nBack up anything important on the SD card first.\n\nAllow writes to this cart until Xfer64 closes?";

/**
 * Invoke a command that writes to the cart. An EverDrive-64 PRO refuses until the user accepts, once per
 * app run, that writing to it is experimental: this asks, records the answer, and retries.
 * @param {string} cmd
 * @param {Record<string, unknown>} args
 */
async function invokeCartWrite(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    if (!String(e).includes(ED64PRO_WRITE_CONSENT_MARKER)) throw e;
    const ok = await showExplorerConfirm(ED64PRO_WRITE_WARNING, {
      title: "Allow writes to the EverDrive-64 PRO?",
      okLabel: "Allow writes",
    });
    if (!ok) throw new Error("Cancelled");
    await invoke("cart_serial_allow_ed64pro_writes");
    return invoke(cmd, args);
  }
}

/**
 * @param {string} message
 * @param {{ title?: string, okLabel?: string, defaultValue?: string, placeholder?: string, selectFilenameStem?: boolean, disableOkIfEmpty?: boolean }} [opts]
 *   `title` defaults to "Xfer64" and `okLabel` to "OK".
 *   When `selectFilenameStem` is true, only the part before the last "." is selected (Windows-style rename).
 *   When `disableOkIfEmpty` is true, OK stays disabled until the trimmed value is non-empty.
 */
function showExplorerPrompt(message, opts = {}) {
  return showExplorerModal({
    type: "prompt",
    message,
    title: opts.title,
    okLabel: opts.okLabel,
    defaultValue: opts.defaultValue ?? "",
    placeholder: opts.placeholder ?? "",
    selectFilenameStem: opts.selectFilenameStem === true,
    disableOkIfEmpty: opts.disableOkIfEmpty === true,
  });
}

/** Shared options for cart + PC "new folder" prompt (default label, OK disabled when empty). */
const PROMPT_MKDIR_OPTS = {
  title: "New folder",
  okLabel: "Create",
  defaultValue: "New folder",
  disableOkIfEmpty: true,
};

/** Shared options for cart + PC rename prompt; `defaultValue` is added per call. */
const PROMPT_RENAME_OPTS = { title: "Rename", okLabel: "Rename", selectFilenameStem: true };

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
  return fileReplaceModalQueue(() => openFileReplaceModal(cfg));
}

/**
 * One dialog root holds one dialog at a time. The returned function runs `open` once every dialog it
 * was given before has settled, and returns that dialog's own promise, so a message that arrives
 * while its root is in use waits its turn rather than overwriting the one on screen.
 * @returns {<T>(open: () => Promise<T>) => Promise<T>}
 */
function createModalQueue() {
  let tail = Promise.resolve();
  return (open) => {
    const run = tail.then(open);
    tail = run.then(
      () => undefined,
      () => undefined
    );
    return run;
  };
}

const fileReplaceModalQueue = createModalQueue();
const explorerModalQueue = createModalQueue();

/** @param {{ mode: 'fs'|'export'|'import', step: Record<string, unknown>, totalInPlan: number }} cfg */
function openFileReplaceModal(cfg) {
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

    // Replace — this one file — is always the primary action: it is focused on open and Enter
    // activates it. Replace all and Skip all appear only when more files may follow.
    const nPlan = Number(cfg.totalInPlan);
    const singleFile = Number.isFinite(nPlan) && nPlan <= 1;
    btnSkipAll.hidden = singleFile;
    btnYesAll.hidden = singleFile;

    hideNameTooltip();
    const step = cfg.step || {};
    let targetLine = "";
    if (cfg.mode === "import") {
      targetLine = String(step.cartPath || "").trim() || "(unknown)";
    } else {
      targetLine = String(step.destPc || "").trim() || "(unknown)";
    }
    const operation = copyOperationNoun(cfg.mode).toLowerCase();
    msgEl.textContent = `A file with this name already exists:\n\n${targetLine}\n\nReplace it, skip it, or cancel the ${operation}?`;

    let settled = false;
    /** @type {((opts?: { restoreFocus?: boolean }) => void) | null} */
    let releaseFocus = null;

    const cleanup = () => {
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
      root.hidden = true;
      root.setAttribute("aria-hidden", "true");
      document.body.classList.remove("explorer-modal-open");
      releaseFocus?.();
      resolve(v);
    };

    // trapDialogFocus hands this Enter only while this dialog is the topmost one.
    /** @param {KeyboardEvent} e */
    const onEnter = (e) => {
      // Enter on a focused button presses that button; anywhere else it is Replace.
      if (e.target instanceof HTMLButtonElement && root.contains(e.target)) return;
      e.preventDefault();
      e.stopPropagation();
      finish("yes");
    };

    const onYes = () => finish("yes");
    const onSkip = () => finish("skip");
    const onYesAll = () => finish("yesAll");
    const onSkipAll = () => finish("skipAll");
    const onCancel = () => finish("cancel");
    const onBackdrop = () => finish("cancel");

    btnYes.addEventListener("click", onYes);
    btnSkip.addEventListener("click", onSkip);
    btnYesAll.addEventListener("click", onYesAll);
    btnSkipAll.addEventListener("click", onSkipAll);
    btnCancel.addEventListener("click", onCancel);
    backdrop?.addEventListener("click", onBackdrop);

    root.hidden = false;
    root.setAttribute("aria-hidden", "false");
    document.body.classList.add("explorer-modal-open");
    const panel = root.querySelector('[role="dialog"]');
    releaseFocus = trapDialogFocus(panel instanceof HTMLElement ? panel : root, {
      initialFocus: btnYes,
      onEscape: onCancel,
      onEnter,
    });
  });
}

/**
 * Resolves overwrite prompts, then runs cart ↔ PC copies in **one** backend SD session per direction
 * (`cart_serial_*_copy_batch`) so the COM port opens once for the whole batch.
 * @param {Record<string, unknown>[]} plan
 * @param {'export'|'import'} mode
 * @param {string} [action] progress lead for every step; defaults to `copyActionLabel(mode)`
 */
async function runInteractiveCopyPlan(plan, mode, action = copyActionLabel(mode)) {
  if (mode !== "export" && mode !== "import") {
    throw new Error("runInteractiveCopyPlan: only export and import are supported");
  }
  const total = plan.reduce((s, st) => s + (Number(st.bytes) || 0), 0) || 1;
  let doneBytes = 0;
  let yesAll = false;
  let skipAll = false;
  const n = plan.length;
  const initialMsg = n === 0 ? "" : formatCopyProgressMessage(plan[0], mode, 0, n, action);
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
        message: formatSkipProgressMessage(step, mode, i, n, action),
      });
      continue;
    }
    const ow = step.conflictIfExists ? overwrite : true;
    const base = doneBytes;
    const msg = formatCopyProgressMessage(step, mode, i, n, action);
    const bytes = Number(step.bytes) || 0;
    // isDir marks a directory-creation step (empty folders); the backend mkdirs instead of copying.
    const common = { overwrite: ow, progressDoneBase: base, progressMessage: msg, bytes, isDir: step.isDir === true };
    batchPayload.push(
      mode === "export"
        ? { ...common, cartPath: step.cartPath, destPcPath: step.destPc }
        : { ...common, srcPcPath: step.srcPc, cartDestPath: step.cartPath }
    );
    doneBytes += bytes;
  }
  if (batchPayload.length === 0) return;
  const batchCmd = mode === "export" ? "cart_serial_export_copy_batch" : "cart_serial_import_copy_batch";
  await invokeCartWrite(batchCmd, { items: batchPayload, progressTotal: total });
}

/**
 * @param {{ type: 'alert'|'confirm'|'prompt', message: string, title?: string, okLabel?: string, defaultValue?: string, placeholder?: string, selectFilenameStem?: boolean, disableOkIfEmpty?: boolean }} cfg
 *   `title` defaults to "Xfer64" and `okLabel` to "OK"; both are reset on every open.
 *   Alert, confirm and prompt share one dialog root: one asked for while another is open waits
 *   until that one is answered, then opens and resolves on its own.
 */
function showExplorerModal(cfg) {
  return explorerModalQueue(() => openExplorerModal(cfg));
}

/** @param {Parameters<typeof showExplorerModal>[0]} cfg */
function openExplorerModal(cfg) {
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
    const okLabel = cfg.okLabel || MODAL_OK_LABEL_DEFAULT;
    btnOk.textContent = okLabel;
    btnOk.title = `${okLabel} (Enter)`;

    const isPrompt = cfg.type === "prompt";
    const isAlert = cfg.type === "alert";
    inputEl.hidden = !isPrompt;
    btnCancel.hidden = isAlert;

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
    /** @type {((opts?: { restoreFocus?: boolean }) => void) | null} */
    let releaseFocus = null;

    /** @type {() => void} */
    let cleanup = () => {};

    const finish = (value) => {
      if (settled) return;
      settled = true;
      cleanup();
      root.hidden = true;
      root.setAttribute("aria-hidden", "true");
      document.body.classList.remove("explorer-modal-open");
      releaseFocus?.();
      resolve(value);
    };

    // trapDialogFocus hands this Enter only while this dialog is the topmost one.
    /** @param {KeyboardEvent} e */
    const onEnter = (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (cfg.type === "alert") finish(undefined);
      else if (cfg.type === "confirm") finish(e.target !== btnCancel);
      else if (e.target === btnCancel) finish(null);
      else if (cfg.type === "prompt") {
        if (cfg.disableOkIfEmpty === true && !inputEl.value.trim()) return;
        finish(inputEl.value);
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
      btnOk.removeEventListener("click", onOk);
      btnCancel.removeEventListener("click", onCancel);
      backdrop?.removeEventListener("click", onBackdrop);
      if (isPrompt && cfg.disableOkIfEmpty === true) {
        inputEl.removeEventListener("input", syncOkButtonForEmptyField);
      }
      btnOk.removeAttribute("tabindex");
    };

    if (isPrompt && cfg.disableOkIfEmpty === true) {
      inputEl.addEventListener("input", syncOkButtonForEmptyField);
    }
    btnOk.addEventListener("click", onOk);
    btnCancel.addEventListener("click", onCancel);
    backdrop?.addEventListener("click", onBackdrop);

    root.hidden = false;
    root.setAttribute("aria-hidden", "false");
    document.body.classList.add("explorer-modal-open");

    const panel = root.querySelector('[role="dialog"]');
    releaseFocus = trapDialogFocus(panel instanceof HTMLElement ? panel : root, {
      initialFocus: isPrompt ? inputEl : btnOk,
      onEscape: onCancel,
      onEnter,
    });
    if (isPrompt) {
      if (cfg.selectFilenameStem === true) selectFilenameStemInInput(inputEl);
      else inputEl.select();
    }
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

/** Substring of `ED64_BETA_SD_MSG` in `cart_serial_sd.rs` — sniff test for the long “configure linear base” footer text. */
const ED64_NO_BASE_ERR_PREFIX = "EverDrive-64 X7 needs a linear ROM address";

const ED64_SD_BASE_SCAN_LABEL = "Scanning for SD base";
/** Cart footer + progress strip while `runEd64LinearBaseScan` runs. */
const ED64_SD_BASE_SCAN_STATUS_MSG = `${ED64_SD_BASE_SCAN_LABEL} (may take a minute)…`;

function isCartSdBaseScanProgressBarActive() {
  const root = document.getElementById("explorer-operation-cart");
  if (!root || root.hidden) return false;
  const t = (document.getElementById("explorer-operation-text-cart")?.textContent || "").trim();
  return t.includes(ED64_SD_BASE_SCAN_LABEL);
}

/**
 * @param {{ preserveSelection?: boolean, forceRefresh?: boolean }} [opts]
 * When `preserveSelection` is true, selection is restored for paths that still exist (e.g. during long operations).
 * `forceRefresh` — bypass Rust cart list cache on the first page (e.g. F5).
 */
async function loadCartPane(opts = {}) {
  beginPaneLoading("cart", "Reading cart…");
  updateExplorerControls();
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
        const { entries, exfat, fsLabel } = await listCartDirPaged(listPath, forceRefresh);
        const visible = sortEntriesForPane("cart", filterHiddenEntries(entries, showHiddenForPane("cart")));
        if (!preserveSelection) {
          state.cart.selected.clear();
          state.cart.anchorPath = null;
          const wrapCart = document.getElementById("table-wrap-cart");
          if (wrapCart) wrapCart.scrollTop = 0;
        }
        abandonInlineRenameIfPane("cart");
        lastVirtualRange.cart = null;
        cartReady = true;
        state.cart.listEntries = visible;
        if (preserveSelection && savedSel) {
          restorePaneSelectionAfterLoad("cart", visible, savedSel, savedAnchor, false);
        }
        renderExplorerPane("cart");
        const vol = fsLabel || (exfat ? "exFAT" : "FAT");
        if (statusMeta) statusMeta.textContent = `${countNoun(visible.length, "item")} · ${vol}`;
      } catch (err) {
        abandonInlineRenameIfPane("cart");
        cartReady = false;
        state.cart.listEntries = [];
        lastVirtualRange.cart = null;
        tbody.replaceChildren();
        if (statusMeta) {
          const raw = userFacingErrorMessage(err, { context: "cart" });
          const longEd64NoBase =
            typeof raw === "string" && raw.includes(ED64_NO_BASE_ERR_PREFIX);
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
    updateExplorerControls();
  }
}

/**
 * @param {{ preserveSelection?: boolean, forceRefresh?: boolean }} [opts]
 * `forceRefresh` — bypass Rust PC list cache on the first page (e.g. F5).
 */
async function loadPcPane(opts = {}) {
  beginPaneLoading("pc", "Reading folder…");
  updateExplorerControls();
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
        statusMeta.textContent = BROWSE_FIRST_MSG;
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
      if (statusMeta) statusMeta.textContent = countNoun(visible.length, "item");
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
    updateExplorerControls();
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
  // The click that ends a drag is not a selection click.
  if (suppressNextRowClick) {
    suppressNextRowClick = false;
    return;
  }
  if (isPaneBusy(pane)) return;

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

/**
 * Arm a pane-to-pane drag on a row press. The drag only begins once the pointer has travelled
 * `DRAG_START_THRESHOLD_PX`, so a plain click still selects and a slow double-click still renames.
 * @param {"cart" | "pc"} pane
 * @param {HTMLTableRowElement} tr
 * @param {string} path
 */
function bindRowPointerDrag(pane, tr, path) {
  tr.addEventListener("pointerdown", (ev) => {
    if (ev.button !== 0 || ev.pointerType === "touch") return;
    if (ev.target.closest(".explorer-name-input")) return;
    if (isExplorerModalOpen() || isPaneBusy(pane)) return;
    const wrap = document.getElementById(`table-wrap-${pane}`);
    if (!wrap) return;
    cancelPointerDrag();
    // Any click from the previous gesture has already been delivered by now.
    suppressNextRowClick = false;
    pointerDrag = {
      pane,
      path,
      wrap,
      pointerId: ev.pointerId,
      startX: ev.clientX,
      startY: ev.clientY,
      lastX: ev.clientX,
      lastY: ev.clientY,
      paths: [],
      started: false,
      ghost: null,
      autoScrollTimer: null,
    };
    window.addEventListener("pointermove", onPointerDragMove, true);
    window.addEventListener("pointerup", onPointerDragEnd, true);
    window.addEventListener("pointercancel", onPointerDragCancel, true);
    window.addEventListener("keydown", onPointerDragKeyDown, true);
  });
}

/** Promote the armed press to a real drag: ghost, cursor, and pointer capture. */
function beginPointerDrag() {
  const d = pointerDrag;
  if (!d || d.started) return;
  d.paths = pathsForDrag(d.pane, d.path);
  if (!d.paths.length) {
    cancelPointerDrag();
    return;
  }
  d.started = true;
  suppressNextRowClick = true;
  hideNameTooltip();
  hideExplorerContextMenu();
  cancelRenameNameClickArm();
  document.body.classList.add("explorer-dragging");
  d.ghost = createDragGhost(d.paths);
  // Capture keeps the drag alive past the window edge — which is exactly where a drag-out starts.
  try {
    d.wrap.setPointerCapture(d.pointerId);
  } catch {
    // Capture is a nicety; without it the drag still works inside the window.
  }
}

function onPointerDragMove(ev) {
  const d = pointerDrag;
  if (!d || ev.pointerId !== d.pointerId) return;
  d.lastX = ev.clientX;
  d.lastY = ev.clientY;
  if (!d.started) {
    if (Math.hypot(ev.clientX - d.startX, ev.clientY - d.startY) < DRAG_START_THRESHOLD_PX) return;
    beginPointerDrag();
    if (!pointerDrag) return;
  }
  if (isPointOutsideWindow(ev.clientX, ev.clientY)) {
    void handOffDragToOs();
    return;
  }
  moveDragGhost(d.ghost, ev.clientX, ev.clientY);
  const target = dropTargetAt(ev.clientX, ev.clientY);
  highlightDropTarget(target, d.pane);
  updateDragAutoScroll(target);
}

function onPointerDragEnd(ev) {
  const d = pointerDrag;
  if (!d || ev.pointerId !== d.pointerId) return;
  const { pane, paths, started } = d;
  const x = ev.clientX;
  const y = ev.clientY;
  cancelPointerDrag();
  if (!started) return;
  const target = dropTargetAt(x, y);
  if (!target || target.pane === pane) return;
  void runPaneDrop(pane, paths, target);
}

function onPointerDragCancel(ev) {
  if (pointerDrag && ev.pointerId === pointerDrag.pointerId) cancelPointerDrag();
}

function onPointerDragKeyDown(ev) {
  if (ev.key === "Escape") cancelPointerDrag();
}

/** Tear the drag down — listeners, ghost, capture, highlight. A no-op when nothing is dragging. */
function cancelPointerDrag() {
  const d = pointerDrag;
  pointerDrag = null;
  window.removeEventListener("pointermove", onPointerDragMove, true);
  window.removeEventListener("pointerup", onPointerDragEnd, true);
  window.removeEventListener("pointercancel", onPointerDragCancel, true);
  window.removeEventListener("keydown", onPointerDragKeyDown, true);
  if (!d) return;
  if (d.autoScrollTimer != null) window.clearInterval(d.autoScrollTimer);
  d.ghost?.remove();
  try {
    if (d.wrap.hasPointerCapture?.(d.pointerId)) d.wrap.releasePointerCapture(d.pointerId);
  } catch {
    // The pointer is already gone; nothing to release.
  }
  document.body.classList.remove("explorer-dragging");
  clearDropHighlight();
}

/** @param {string[]} paths */
function createDragGhost(paths) {
  const el = document.createElement("div");
  el.className = "explorer-drag-ghost";
  const count = document.createElement("span");
  count.className = "explorer-drag-ghost-count";
  count.textContent = String(paths.length);
  const label = document.createElement("span");
  label.className = "explorer-drag-ghost-label";
  label.textContent = basenameForMessage(paths[0]);
  el.append(count, label);
  if (paths.length > 1) {
    const more = document.createElement("span");
    more.className = "explorer-drag-ghost-more";
    more.textContent = `+ ${paths.length - 1} more`;
    el.append(more);
  }
  document.body.appendChild(el);
  return el;
}

function moveDragGhost(el, clientX, clientY) {
  if (el) el.style.transform = `translate(${clientX + 14}px, ${clientY + 12}px)`;
}

/** True once the pointer is clear of the window — the cue to hand the drag to the shell. */
function isPointOutsideWindow(x, y) {
  const m = DRAG_OUT_MARGIN_PX;
  return x < -m || y < -m || x > window.innerWidth - 1 + m || y > window.innerHeight - 1 + m;
}

/**
 * The pane and destination folder under a point, or null when that point is not over a file list.
 * @returns {{ pane: "cart" | "pc", wrap: HTMLElement, dirRow: HTMLTableRowElement | null, destPath: string | null } | null}
 */
function dropTargetAt(clientX, clientY) {
  if (isExplorerModalOpen()) return null;
  const el = document.elementFromPoint(clientX, clientY);
  if (!(el instanceof Element)) return null;
  const wrap = el.closest(".explorer-table-wrap[data-drop-pane]");
  if (!wrap) return null;
  const pane = wrap.dataset.dropPane;
  if (pane !== "cart" && pane !== "pc") return null;
  // A pane with an operation running takes no drops.
  if (isPaneBusy(pane)) return null;
  const dirRow = el.closest("tbody tr[data-is-dir='1']");
  return { pane, wrap, dirRow: dirRow || null, destPath: dirRow?.dataset?.path || null };
}

/**
 * Light up the folder row under the pointer, or the whole pane when the drop would land in its
 * current folder. `sourcePane` suppresses the highlight for a drop that would do nothing.
 */
function highlightDropTarget(target, sourcePane = null) {
  const effective = target && (!sourcePane || target.pane !== sourcePane) ? target : null;
  const row = effective?.dirRow ?? null;
  const wrap = effective?.wrap ?? null;
  if (dropHighlight && dropHighlight.wrap === wrap && dropHighlight.row === row) return;
  clearDropHighlight();
  if (!wrap) return;
  if (row) row.classList.add("drag-over-drop-target");
  else wrap.classList.add("drag-over-target");
  dropHighlight = { wrap, row };
}

function clearDropHighlight() {
  if (!dropHighlight) return;
  dropHighlight.wrap?.classList.remove("drag-over-target");
  dropHighlight.row?.classList.remove("drag-over-drop-target");
  dropHighlight = null;
}

/** Scroll a list that is dragged over near its edge, and keep the highlight on what rolls under. */
function updateDragAutoScroll(target) {
  const d = pointerDrag;
  if (!d) return;
  if (d.autoScrollTimer != null) {
    window.clearInterval(d.autoScrollTimer);
    d.autoScrollTimer = null;
  }
  if (!target) return;
  const rect = target.wrap.getBoundingClientRect();
  let dir = 0;
  if (d.lastY < rect.top + DRAG_AUTOSCROLL_EDGE_PX) dir = -1;
  else if (d.lastY > rect.bottom - DRAG_AUTOSCROLL_EDGE_PX) dir = 1;
  if (!dir) return;
  const wrap = target.wrap;
  d.autoScrollTimer = window.setInterval(() => {
    const live = pointerDrag;
    if (!live) return;
    wrap.scrollTop += dir * DRAG_AUTOSCROLL_STEP_PX;
    // The virtual list re-mounts rows as it scrolls, so re-resolve what is under the pointer.
    highlightDropTarget(dropTargetAt(live.lastX, live.lastY), live.pane);
  }, DRAG_AUTOSCROLL_INTERVAL_MS);
}

/** @param {"cart" | "pc"} sourcePane */
async function runPaneDrop(sourcePane, paths, target) {
  if (!paths.length) return;
  if (sourcePane === "cart" && target.pane === "pc") await copyCartToPcPaths(paths, target.destPath);
  else if (sourcePane === "pc" && target.pane === "cart") await copyPcToCartPaths(paths, target.destPath);
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

/**
 * Whether row `index` sits wholly inside its list's viewport, below the header.
 * @param {"cart" | "pc"} pane
 * @param {number} index
 */
function isVirtualRowInView(pane, index) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  const thead = wrap?.querySelector("thead");
  if (!wrap || !thead) return false;
  const theadH = thead.offsetHeight;
  const rowH = getExplorerRowHeightPx();
  const rowTop = theadH + index * rowH;
  return rowTop >= wrap.scrollTop + theadH && rowTop + rowH <= wrap.scrollTop + wrap.clientHeight;
}

/**
 * Scroll so `path` is in the virtual window, then re-render rows for that scroll position.
 * @param {"cart" | "pc"} pane
 * @param {string} path
 * @param {{ onlyIfHidden?: boolean }} [opts] `onlyIfHidden`: leave the list where it is when the row
 *   is already in view, rather than centring it.
 */
function ensurePathVisibleInPane(pane, path, { onlyIfHidden = false } = {}) {
  const entries = state[pane].listEntries;
  const idx = entries.findIndex((e) => e.path === path);
  if (idx < 0) return;
  if (onlyIfHidden && isVirtualRowInView(pane, idx) && findRowInPaneDom(pane, path)) return;
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
  tr.addEventListener("click", (ev) => onRowClick(pane, e.path, ev));
  tr.addEventListener("dblclick", (ev) => {
    ev.preventDefault();
    if (isPaneBusy(pane)) return;
    cancelRenameNameClickArm();
    if (inlineRenameState) {
      cancelInlineRenameRestoreDOMOnly();
    }
    if (e.isDir) navigate(pane, e.path, true);
  });
  bindRowPointerDrag(pane, tr, e.path);
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
  updateExplorerControls();
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
    finishOperationProgress(BROWSE_FIRST_MSG, true, "pc");
    return;
  }
  if (paths.length === 0) return;
  const action = copyActionLabel("export");
  const doneLabel = selectionLabel("cart", paths);
  // Both panes: the cart's port is held for the whole copy, and the This PC folder is being written.
  const endCartBusy = beginBusy("cart", `${action}…`);
  const endPcBusy = beginBusy("pc", `${action}…`);
  try {
    let performed = false;
    const cancelled = await withCartDaemonYield(
      async () => {
        showOperationProgress(`${action}…`, "cart", false);
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
          await runWithProgress("cart", formatCopyProgressMessage(plan[0], "export", 0, plan.length), () =>
            runInteractiveCopyPlan(plan, "export")
          );
          await loadBothPanes();
        } catch (e) {
          hideOperationProgressPane("cart");
          throw e;
        }
      },
      { confirm: true }
    );
    if (cancelled) return;
    if (!performed) return;
    finishOperationProgress(`Exported ${doneLabel} to This PC.`, false, "cart");
  } catch (e) {
    if (e && e.userCancelledCopy) {
      await loadBothPanes({ forceRefresh: true }).catch(logReloadAfterCancelError);
      finishOperationCancelled("Export cancelled.", "cart");
      return;
    }
    if (isCancelledBackendError(e)) {
      await loadBothPanes({ forceRefresh: true }).catch(logReloadAfterCancelError);
      finishOperationCancelled(cancelMessageFor(e, "Export"), "cart");
    } else {
      // A failure partway through still copied earlier files; refresh so the panes match disk.
      await loadBothPanes({ forceRefresh: true }).catch(() => {});
      finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
    }
  } finally {
    endCartBusy();
    endPcBusy();
  }
}

async function copyPcToCartPaths(paths, cartParentOverride = null) {
  const cartParent =
    cartParentOverride != null && String(cartParentOverride).trim() !== ""
      ? normalizeUsbPath(cartParentOverride)
      : normalizeUsbPath(state.cart.path);
  if (paths.length === 0) return;
  const action = copyActionLabel("import");
  const doneLabel = selectionLabel("pc", paths);
  // Both panes: the files are read from This PC, and the cart's port is held for the whole copy.
  const endPcBusy = beginBusy("pc", `${action}…`);
  const endCartBusy = beginBusy("cart", `${action}…`);
  try {
    let performed = false;
    const cancelled = await withCartDaemonYield(
      async () => {
        showOperationProgress(`${action}…`, "pc", false);
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
          await runWithProgress("pc", formatCopyProgressMessage(plan[0], "import", 0, plan.length), () =>
            runInteractiveCopyPlan(plan, "import")
          );
          await loadBothPanes();
        } catch (e) {
          hideOperationProgressPane("pc");
          throw e;
        }
      },
      { confirm: true }
    );
    if (cancelled) return;
    if (!performed) return;
    finishOperationProgress(`Imported ${doneLabel} to cart.`, false, "pc");
  } catch (e) {
    if (e && e.userCancelledCopy) {
      await loadBothPanes({ forceRefresh: true }).catch(logReloadAfterCancelError);
      finishOperationCancelled("Import cancelled.", "pc");
      return;
    }
    if (isCancelledBackendError(e)) {
      await loadBothPanes({ forceRefresh: true }).catch(logReloadAfterCancelError);
      finishOperationCancelled(cancelMessageFor(e, "Import"), "pc");
    } else {
      await loadBothPanes({ forceRefresh: true }).catch(() => {});
      finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
    }
  } finally {
    endPcBusy();
    endCartBusy();
  }
}

/**
 * Windows → Windows copy: the destination side of a drop from Explorer into the PC pane. No cart
 * is involved, so there is no daemon handshake and no COM port to hold.
 * @param {string[]} paths
 * @param {string | null} destOverride folder row the drop landed on, if any
 */
async function copyPcToPcPaths(paths, destOverride = null) {
  const dest =
    destOverride != null && String(destOverride).trim() !== ""
      ? normalizePath(destOverride)
      : state.pc.path;
  if (!dest) {
    finishOperationProgress(BROWSE_FIRST_MSG, true, "pc");
    return;
  }
  // Dropping a file back into the folder it already sits in is a no-op, not a copy over itself.
  const srcs = [];
  let intoItself = false;
  for (const p of paths) {
    if (isSamePcDir(dirnameWin(p), dest)) continue;
    if (isPcPathInside(p, dest)) {
      intoItself = true;
      continue;
    }
    srcs.push(p);
  }
  if (!srcs.length) {
    if (intoItself) finishOperationProgress("A folder cannot be copied into itself.", true, "pc");
    return;
  }
  const doneLabel = selectionLabel("pc", srcs);
  const endBusy = beginBusy("pc", `${copyActionLabel("fs")}…`);
  showOperationProgress(`${copyActionLabel("fs")}…`, "pc", false);
  try {
    await invoke("explorer_reset_cancel");
    const plan = await invoke("build_fs_copy_plan", { destDir: dest, srcPaths: srcs });
    if (!plan.length) {
      hideOperationProgressPane("pc");
      return;
    }
    await runWithProgress("pc", formatCopyProgressMessage(plan[0], "fs", 0, plan.length), () => runFsCopyPlan(plan));
    await loadPcPane({ forceRefresh: true });
    finishOperationProgress(`Copied ${doneLabel}.`, false, "pc");
  } catch (e) {
    // A failure partway through still copied earlier files; refresh so the pane matches disk.
    await loadPcPane({ forceRefresh: true }).catch(() => {});
    if (e && e.userCancelledCopy) {
      finishOperationCancelled("Copy cancelled.", "pc");
      return;
    }
    if (isCancelledBackendError(e)) {
      finishOperationCancelled(cancelMessageFor(e, "Copy"), "pc");
      return;
    }
    finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  } finally {
    endBusy();
  }
}

/** Windows paths differ only in case and trailing slashes far more often than they differ in fact. */
function normalizedPcDir(v) {
  return normalizePath(v).replace(/\\+$/, "").toLowerCase();
}

function isSamePcDir(a, b) {
  return normalizedPcDir(a) === normalizedPcDir(b);
}

/** True when `child` is `parent` or sits under it — a folder dropped onto its own descendant. */
function isPcPathInside(parent, child) {
  const p = normalizedPcDir(parent);
  const c = normalizedPcDir(child);
  return c === p || c.startsWith(`${p}\\`);
}

/**
 * Run a PC → PC plan step by step, prompting on each conflict.
 *
 * `runInteractiveCopyPlan` cannot be reused: cart copies are sent as one batch so the COM port
 * opens once for the whole plan, while these are plain file copies with nothing to hold open.
 * @param {Record<string, unknown>[]} plan
 */
async function runFsCopyPlan(plan) {
  const total = plan.reduce((sum, st) => sum + (Number(st.bytes) || 0), 0) || 1;
  const n = plan.length;
  let doneBytes = 0;
  let yesAll = false;
  let skipAll = false;
  const initialMsg = n === 0 ? "" : formatCopyProgressMessage(plan[0], "fs", 0, n);
  await invoke("explorer_emit_progress", { done: 0, total, message: initialMsg });
  for (let i = 0; i < n; i++) {
    const step = plan[i];
    // isDir marks a directory-creation step (see InteractiveCopyStep) — mkdir, not a file copy.
    if (step.isDir === true) {
      await invoke("fs_mkdir", { path: step.destPc });
      continue;
    }
    let overwrite = true;
    let skipThis = false;
    if (step.conflictIfExists) {
      if (yesAll) {
        overwrite = true;
      } else if (skipAll) {
        skipThis = true;
      } else {
        const choice = await showFileReplaceModal({ mode: "fs", step, totalInPlan: n });
        if (choice === "cancel") throwUserCopyCancel();
        if (choice === "skip") skipThis = true;
        if (choice === "skipAll") {
          skipAll = true;
          skipThis = true;
        }
        if (choice === "yesAll") yesAll = true;
      }
    }
    if (skipThis) {
      doneBytes += Number(step.bytes) || 0;
      await invoke("explorer_emit_progress", {
        done: doneBytes,
        total,
        message: formatSkipProgressMessage(step, "fs", i, n),
      });
      continue;
    }
    await invoke("explorer_emit_progress", {
      done: doneBytes,
      total,
      message: formatCopyProgressMessage(step, "fs", i, n),
    });
    await invoke("fs_copy_one_file", {
      src: step.srcPc,
      dest: step.destPc,
      overwrite,
      progressDoneBase: doneBytes,
      progressTotal: total,
    });
    doneBytes += Number(step.bytes) || 0;
  }
}

function setupTableWrapMarquee(pane) {
  const wrap = document.getElementById(`table-wrap-${pane}`);
  if (!wrap) return;

  wrap.addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    // The busy shade sits inside the wrap, so a press on it would otherwise start a marquee.
    if (isPaneBusy(pane)) return;
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

/**
 * Drops that come from outside the app. Tauri only reports these when the window has
 * `dragDropEnabled: true`, and it is the only route that carries real paths — which is what
 * makes an Explorer drop copyable at all.
 */
function setupOsFileDrops() {
  const eventApi = window.__TAURI__?.event;
  if (!eventApi?.listen) return;
  const onOsDragMove = (payload) => {
    if (osDragInFlight) return;
    highlightDropTarget(osDropTargetFor(payload));
  };
  void eventApi.listen("tauri://drag-enter", (e) => onOsDragMove(e.payload));
  void eventApi.listen("tauri://drag-over", (e) => onOsDragMove(e.payload));
  void eventApi.listen("tauri://drag-leave", () => clearDropHighlight());
  void eventApi.listen("tauri://drag-drop", (e) => {
    const target = osDropTargetFor(e.payload);
    clearDropHighlight();
    // A drag we started ourselves, dropped back on our own window: the selection is already here.
    if (osDragInFlight) {
      endOsDrag();
      return;
    }
    void runOsFileDrop(e.payload?.paths, target);
  });
}

/** Tauri reports the pointer in physical pixels; `elementFromPoint` works in CSS pixels. */
function osDropTargetFor(payload) {
  const pos = payload?.position;
  if (!pos) return null;
  const scale = window.devicePixelRatio || 1;
  return dropTargetAt(Number(pos.x) / scale, Number(pos.y) / scale);
}

async function runOsFileDrop(paths, target) {
  const list = (Array.isArray(paths) ? paths : []).map((p) => String(p || "").trim()).filter(Boolean);
  if (!list.length || !target) return;
  if (target.pane === "cart") await copyPcToCartPaths(list, target.destPath);
  else await copyPcToPcPaths(list, target.destPath);
}

/** The pointer left the window mid-drag: the shell takes it from here. */
async function handOffDragToOs() {
  const d = pointerDrag;
  if (!d || !d.started) return;
  const { pane, paths } = d;
  cancelPointerDrag();
  if (!paths.length) return;
  if (pane === "pc") await startOsDragOut(paths, "pc");
  else await startCartDragOut(paths);
}

/**
 * Hand `paths` to the OS as a drag. Every path must exist on disk — the shell copies files, it
 * does not ask us for them, which is the whole reason cart files are staged first.
 * @param {string[]} paths
 * @param {"cart" | "pc"} pane pane the drag came from, for error reporting
 */
async function startOsDragOut(paths, pane) {
  const core = window.__TAURI__?.core;
  if (!core?.Channel) return;
  const onEvent = new core.Channel();
  onEvent.onmessage = () => endOsDrag();
  beginOsDrag();
  try {
    await invoke("plugin:drag|start_drag", { item: paths, image: OS_DRAG_IMAGE_PNG, onEvent });
  } catch (e) {
    endOsDrag();
    finishOperationProgress(userFacingErrorMessage(e, { context: pane }), true, pane);
  }
}

function beginOsDrag() {
  osDragInFlight = true;
  if (osDragWatchdogTimer != null) window.clearTimeout(osDragWatchdogTimer);
  osDragWatchdogTimer = window.setTimeout(() => {
    osDragInFlight = false;
    osDragWatchdogTimer = null;
  }, OS_DRAG_WATCHDOG_MS);
}

function endOsDrag() {
  osDragInFlight = false;
  if (osDragWatchdogTimer != null) {
    window.clearTimeout(osDragWatchdogTimer);
    osDragWatchdogTimer = null;
  }
}

/**
 * Cart paths for a staging batch, tagged with size and modified time so a file that changed on
 * the cart is exported again rather than dragged out stale.
 * @param {string[]} paths
 */
function cartStagingKeyFor(paths) {
  const byPath = new Map(state.cart.listEntries.map((e) => [e.path, e]));
  return paths
    .map((p) => {
      const e = byPath.get(p);
      return `${p}|${e ? e.size : "?"}|${e && e.modifiedMs != null ? e.modifiedMs : "?"}`;
    })
    .join("\n");
}

/** Join a staged file onto the staging directory, keeping whatever separator the backend used. */
function joinStagedPath(dir, name) {
  const base = String(dir).replace(/[\\/]+$/, "");
  return `${base}${base.includes("\\") ? "\\" : "/"}${name}`;
}

async function discardStagingDir(dir) {
  if (!dir) return;
  if (cartDragStaged?.dir === dir) cartDragStaged = null;
  try {
    await invoke("drag_staging_release", { dir });
  } catch {
    // Temp files: a failed cleanup is pruned on a later run, and is not worth a dialog.
  }
}

/** Forget staged cart copies: after a port or cart change they may be from a different card. */
async function forgetStagedCartDrag() {
  cartDragStaged = null;
  try {
    await invoke("drag_staging_clear");
  } catch {
    // Temp files; a failed cleanup is pruned on a later run.
  }
}

/**
 * Drag a cart selection out to another window.
 *
 * Windows will not start a drag for a file that does not exist, and the cart's SD card is not a
 * drive — so the first drag-out of a selection exports it to a staging directory over serial and
 * stops there. The pointer has long been released by the time that finishes, so the drag itself
 * is the *next* gesture, which finds the staged copies ready and goes straight out.
 * @param {string[]} paths
 */
async function startCartDragOut(paths) {
  const key = cartStagingKeyFor(paths);
  if (cartDragStaged && cartDragStaged.key === key) {
    await startOsDragOut(cartDragStaged.files, "cart");
    return;
  }
  await stageCartPathsForDragOut(paths, key);
}

async function stageCartPathsForDragOut(paths, key) {
  await discardStagingDir(cartDragStaged?.dir);
  const doneLabel = selectionLabel("cart", paths);
  const action = "Preparing to drag out";
  const endBusy = beginBusy("cart", `${action}…`);
  let dir = null;
  let staged = false;
  try {
    const cancelled = await withCartDaemonYield(
      async () => {
        showOperationProgress(`${action}…`, "cart", false);
        try {
          await invoke("explorer_reset_cancel");
          dir = await invoke("drag_staging_begin");
          const plan = await invoke("build_cart_export_plan", { cartPaths: paths, toPcParent: dir });
          if (!plan.length) {
            hideOperationProgressPane("cart");
            return;
          }
          await runWithProgress("cart", formatCopyProgressMessage(plan[0], "export", 0, plan.length, action), () =>
            runInteractiveCopyPlan(plan, "export", action)
          );
          cartDragStaged = { key, dir, files: paths.map((p) => joinStagedPath(dir, basenameForMessage(p))) };
          staged = true;
        } catch (e) {
          hideOperationProgressPane("cart");
          throw e;
        }
      },
      { confirm: true }
    );
    if (cancelled || !staged) {
      await discardStagingDir(dir);
      return;
    }
    finishOperationProgress(
      paths.length === 1
        ? `Ready — drag ${doneLabel} out again to copy it.`
        : `Ready — drag the ${doneLabel} out again to copy them.`,
      false,
      "cart"
    );
  } catch (e) {
    await discardStagingDir(dir);
    if (e && e.userCancelledCopy) {
      finishOperationCancelled("Drag-out cancelled.", "cart");
      return;
    }
    if (isCancelledBackendError(e)) {
      finishOperationCancelled(cancelMessageFor(e, "Drag-out"), "cart");
      return;
    }
    finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  } finally {
    endBusy();
  }
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
  updateExplorerControls();
}

/** Status line and properties message when This PC has no folder chosen. */
const BROWSE_FIRST_MSG = "Choose a folder on This PC first — use Browse folder next to the path.";

/** Shown when a pane cannot act because an operation is already running on it. */
const BUSY_REASON = "wait for the current operation to finish";

/**
 * True when Up has nowhere to go: "/" on the cart, a drive root (`C:\`) or share root on This PC.
 * @param {"cart" | "pc"} pane
 */
function isPaneAtRoot(pane) {
  if (pane === "cart") return !normalizeUsbPath(state.cart.path);
  const p = normalizePath(state.pc.path).replace(/\\+$/, "");
  if (!p) return true;
  return /^[A-Za-z]:$/.test(p) || /^\\\\[^\\]+(\\[^\\]+)?$/.test(p);
}

/**
 * Why a pane action cannot run right now, or "" when it can. Buttons show the reason in their
 * tooltip; shortcuts and menu items for an action with a reason do nothing.
 * @param {"rename" | "delete" | "transfer" | "mkdir" | "back" | "up" | "refresh"} action
 *   `transfer` is Export on the cart pane and Import on the This PC pane.
 * @param {"cart" | "pc"} pane
 * @returns {string}
 */
function actionBlockedReason(action, pane) {
  const other = pane === "cart" ? "pc" : "cart";
  if (isPaneBusy(pane) || (action === "transfer" && isPaneBusy(other))) return BUSY_REASON;
  const n = state[pane].selected.size;
  const where = pane === "cart" ? "on the cart" : "on This PC";
  switch (action) {
    case "rename":
      return n === 1 ? "" : "select one item";
    case "delete":
      return n > 0 ? "" : `select items ${where}`;
    case "transfer":
      if (n === 0) return `select items ${where}`;
      if (pane === "cart" && !normalizePath(state.pc.path)) return "choose a folder on This PC first";
      if (pane === "pc" && !cartReady) return "connect the cart first";
      return "";
    case "mkdir":
      return pane === "pc" && !normalizePath(state.pc.path) ? "choose a folder on This PC first" : "";
    case "back":
      return state[pane].histIndex > 0 ? "" : "no previous folder";
    case "up":
      return isPaneAtRoot(pane) ? "already at the top folder" : "";
    default:
      return "";
  }
}

/**
 * Enable or disable a control, keeping `aria-disabled` and its tooltip in step. The `title` is
 * set on the control itself: a disabled button does not get mouse events in every browser, but
 * its own tooltip still shows.
 * @param {HTMLButtonElement | HTMLSelectElement | null} el
 * @param {string} reason from `actionBlockedReason`, or "" when the control can be used
 * @param {{ title?: string, disabledTitle?: string, explain?: boolean }} [titles]
 *   `title` is the tooltip while enabled; while disabled it is `disabledTitle` (default: `title`)
 *   followed by " — reason", unless `explain` is false. With no `title` the tooltip is left alone.
 */
function setControlEnabled(el, reason, titles = {}) {
  if (!el) return;
  const disabled = reason !== "";
  el.disabled = disabled;
  el.setAttribute("aria-disabled", disabled ? "true" : "false");
  if (titles.title === undefined) return;
  if (!disabled) {
    el.title = titles.title;
    return;
  }
  const base = titles.disabledTitle ?? titles.title;
  el.title = titles.explain === false ? base : `${base} — ${reason}`;
}

/**
 * Every pane button, the path pickers and Export / Import, from each pane's selection, location
 * and busy state. Called on every selection change, folder load, and operation start and end.
 */
function updateExplorerControls() {
  for (const pane of /** @type {const} */ (["cart", "pc"])) {
    const busyReason = isPaneBusy(pane) ? BUSY_REASON : "";
    const q = (action) => document.querySelector(`.explorer-toolbar [data-action="${action}"][data-pane="${pane}"]`);
    setControlEnabled(q("back"), actionBlockedReason("back", pane), { title: "Back (Alt+←)", explain: false });
    setControlEnabled(q("up"), actionBlockedReason("up", pane), { title: "Up (Backspace)", explain: false });
    setControlEnabled(q("refresh"), busyReason, { title: "Refresh (F5)" });
    setControlEnabled(q("pick"), busyReason, { title: "Browse folder" });
    setControlEnabled(document.getElementById(`addr-${pane}-select`), busyReason);
    setControlEnabled(document.getElementById(`btn-mkdir-${pane}`), actionBlockedReason("mkdir", pane), {
      title: "New folder (Ctrl+Shift+N)",
    });
    setControlEnabled(document.getElementById(`btn-rename-${pane}`), actionBlockedReason("rename", pane), {
      title: "Rename (F2)",
    });
    setControlEnabled(document.getElementById(`btn-delete-${pane}`), actionBlockedReason("delete", pane), {
      title: "Delete selected (Del)",
    });
    setControlEnabled(document.getElementById(`btn-show-hidden-${pane}`), busyReason, {
      title: showHiddenForPane(pane) ? "Hide hidden files" : "Show hidden files",
    });
  }
  setControlEnabled(document.getElementById("btn-copy-to-pc"), actionBlockedReason("transfer", "cart"), {
    title: "Export selected items to the current folder on This PC (Ctrl+Shift+→)",
    disabledTitle: "Export to This PC (Ctrl+Shift+→)",
  });
  setControlEnabled(document.getElementById("btn-copy-to-cart"), actionBlockedReason("transfer", "pc"), {
    title: "Import selected items into the current folder on the cart (Ctrl+Shift+←)",
    disabledTitle: "Import to cart (Ctrl+Shift+←)",
  });
  // Switching cart or port mid-operation would fight the operation for the serial port, and Settings
  // can change both.
  const cartBusyReason = isPaneBusy("cart") ? BUSY_REASON : "";
  setControlEnabled(document.getElementById("select-cart-device"), cartBusyReason, {
    title: "Which cart is plugged in",
  });
  setControlEnabled(document.getElementById("select-usb-com"), cartBusyReason, {
    title: "Which serial port the cart is on",
  });
  setControlEnabled(document.getElementById("btn-open-settings"), cartBusyReason, { title: "Settings" });
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

async function deleteSelectedCart() {
  const paths = [...state.cart.selected];
  // Only reachable with a selection: the button, menu item and Del are off without one.
  if (paths.length === 0) return;
  const what = selectionLabel("cart", paths);
  let multi64dRunning = false;
  const endProbeBusy = beginBusy("cart", "Deleting…");
  try {
    const p = await invoke("explorer_daemon_probe", { listen: daemonListenUrl() });
    multi64dRunning = p.up === true;
  } catch {
    multi64dRunning = false;
  } finally {
    endProbeBusy();
  }
  const deleteMsg = multi64dRunning
    ? `Delete ${what} from the cart?\n\nThe Multi64 bridge is using this cart — it will pause during the delete, then resume.`
    : `Delete ${what} from the cart?`;
  if (!(await showExplorerConfirm(deleteMsg, { title: "Delete from cart?", okLabel: "Delete" }))) return;
  const endBusy = beginBusy("cart", "Deleting…");
  try {
    const cancelled = await withCartDaemonYield(async () => {
      // The backend names each item as it goes, in the same `Deleting from cart — "a" (1 of 3)…` shape.
      showOperationProgress("Deleting from cart…", "cart", false);
      await runWithProgress("cart", "Deleting from cart…", () =>
        invokeCartWrite("cart_serial_remove_cart", { paths })
      );
      state.cart.selected.clear();
      await loadCartPane();
    });
    if (cancelled) return;
    finishOperationProgress(`Deleted ${what}.`, false, "cart");
  } catch (e) {
    if (isCancelledBackendError(e)) {
      state.cart.selected.clear();
      await loadCartPane().catch(logReloadAfterCancelError);
      finishOperationCancelled(cancelMessageFor(e, "Delete"), "cart");
    } else {
      // Deletes run one at a time, so a failure partway through still removed earlier items.
      state.cart.selected.clear();
      await loadCartPane({ forceRefresh: true }).catch(() => {});
      finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
    }
  } finally {
    endBusy();
  }
}

async function deleteSelectedPc() {
  const paths = [...state.pc.selected];
  // Only reachable with a selection: the button, menu item and Del are off without one.
  if (paths.length === 0) return;
  const what = selectionLabel("pc", paths);
  if (!(await showExplorerConfirm(`Delete ${what} from This PC?`, { title: "Delete from This PC?", okLabel: "Delete" })))
    return;
  const endBusy = beginBusy("pc", "Deleting…");
  try {
    resetProgressCancel();
    const n = paths.length;
    /** One format from first item to last: `Deleting from This PC — "a" (1 of 3)…`. */
    const deletingMessage = (i) => {
      const itemName = basenameForMessage(paths[i]);
      return n === 1
        ? `Deleting from This PC — "${itemName}"…`
        : `Deleting from This PC — "${itemName}" (${i + 1} of ${n})…`;
    };
    showOperationProgress(deletingMessage(0), "pc", true);
    const fill = document.getElementById("explorer-operation-fill-pc");
    const textOp = document.getElementById("explorer-operation-text-pc");
    for (let i = 0; i < n; i++) {
      if (progressCancelRequested) {
        state.pc.selected.clear();
        await loadPcPane().catch(logReloadAfterCancelError);
        finishOperationCancelled("Delete cancelled.", "pc");
        return;
      }
      if (textOp) textOp.textContent = deletingMessage(i);
      await invoke("fs_remove", { path: paths[i] });
      const done = i + 1;
      if (fill) {
        fill.classList.remove("indeterminate");
        fill.style.width = `${(done / n) * 100}%`;
      }
    }
    state.pc.selected.clear();
    state.pc.anchorPath = null;
    await loadPcPane();
    finishOperationProgress(`Deleted ${what}.`, false, "pc");
  } catch (e) {
    // Files deleted before the failure are gone; refresh so they stop being listed.
    state.pc.selected.clear();
    await loadPcPane({ forceRefresh: true }).catch(() => {});
    finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  } finally {
    endBusy();
  }
}

async function promptMkdirCart() {
  const name = await showExplorerPrompt("Name the new folder:", PROMPT_MKDIR_OPTS);
  if (!name || !name.trim()) return;
  const path = cartRelPathForMkdir(name);
  if (!path) return;
  const display = name.trim();
  const endBusy = beginBusy("cart", "Creating folder…");
  try {
    const cancelled = await withCartDaemonYield(
      async () => {
        showOperationProgress(`Creating folder on cart — "${display}"…`, "cart");
        await invokeCartWrite("cart_serial_mkdir_cart", { path });
        await loadCartPane();
      },
      { confirm: true }
    );
    if (cancelled) return;
    finishOperationProgress(`Created "${display}".`, false, "cart");
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  } finally {
    endBusy();
  }
}

async function promptMkdirPc() {
  const parent = state.pc.path;
  if (!parent) {
    finishOperationProgress(BROWSE_FIRST_MSG, true, "pc");
    return;
  }
  const name = await showExplorerPrompt("Name the new folder:", PROMPT_MKDIR_OPTS);
  if (!name || !name.trim()) return;
  const path = `${parent.replace(/[/\\]+$/, "")}\\${name.trim()}`;
  const display = name.trim();
  const endBusy = beginBusy("pc", "Creating folder…");
  showOperationProgress(`Creating folder on This PC — "${display}"…`, "pc");
  try {
    await invoke("fs_mkdir", { path });
    await loadPcPane();
    finishOperationProgress(`Created "${display}".`, false, "pc");
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "pc" }), true, "pc");
  } finally {
    endBusy();
  }
}

async function promptRenameCart() {
  if (inlineRenameState) cancelInlineRenameRestoreDOMOnly();
  // Only reachable with one item selected: the button, menu item and F2 are off otherwise.
  if (state.cart.selected.size !== 1) return;
  const fromPath = normalizeUsbPath([...state.cart.selected][0]);
  const baseName = basenameForMessage(fromPath);
  const name = await showExplorerPrompt("New name:", { ...PROMPT_RENAME_OPTS, defaultValue: baseName });
  if (!name || !name.trim()) return;
  try {
    await runRenameCartFromPaths(fromPath, name.trim());
  } catch (e) {
    finishOperationProgress(userFacingErrorMessage(e, { context: "cart" }), true, "cart");
  }
}

async function promptRenamePc() {
  if (inlineRenameState) cancelInlineRenameRestoreDOMOnly();
  // Only reachable with one item selected: the button, menu item and F2 are off otherwise.
  if (state.pc.selected.size !== 1) return;
  const fromPath = [...state.pc.selected][0];
  const baseName = basenameForMessage(fromPath);
  const name = await showExplorerPrompt("New name:", { ...PROMPT_RENAME_OPTS, defaultValue: baseName });
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
      // Settings included: nothing behind an open dialog reacts to the keyboard.
      if (isAnyExplorerDialogOpen() || isExplorerModalOpen()) return;
      if (isExplorerContextMenuVisible()) {
        onExplorerContextMenuKeyDown(ev);
        return;
      }
      if (isKeyboardBypassTarget(ev.target)) return;
      const pane = focusedPane;
      const key = ev.key;
      if (key === "ContextMenu" || (key === "F10" && ev.shiftKey && !ev.ctrlKey && !ev.altKey)) {
        ev.preventDefault();
        openExplorerContextMenuFromKeyboard(pane);
        return;
      }
      // Every shortcut below is swallowed, then runs only if its action can: one that cannot (nothing
      // selected, already at the top, an operation running) does nothing, the same as its disabled
      // button. `null` means the action has no condition beyond the pane being idle.
      const run = (action, fn) => {
        ev.preventDefault();
        const blocked = action === null ? isPaneBusy(pane) : actionBlockedReason(action, pane) !== "";
        if (!blocked) fn();
      };
      if (key === "Delete") {
        run("delete", () => void (pane === "cart" ? deleteSelectedCart() : deleteSelectedPc()));
        return;
      }
      if (key === "Backspace") {
        run("up", () => void goUp(pane));
        return;
      }
      if (key === "F5") {
        run("refresh", () => {
          if (pane === "cart") void refreshCartPortsAndPane();
          else void loadPcPane({ forceRefresh: true });
        });
        return;
      }
      if (key === "F2") {
        if (inlineRenameState) {
          ev.preventDefault();
          inlineRenameState.input.focus();
          selectFilenameStemInInput(inlineRenameState.input);
          return;
        }
        run("rename", () => void (pane === "cart" ? promptRenameCart() : promptRenamePc()));
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && key === "a" && !ev.shiftKey && !ev.altKey) {
        run(null, () => selectAllInPane(pane));
        return;
      }
      if (key === "ArrowDown") {
        run(null, () => moveSelectionArrow(pane, 1, ev.shiftKey));
        return;
      }
      if (key === "ArrowUp") {
        run(null, () => moveSelectionArrow(pane, -1, ev.shiftKey));
        return;
      }
      if (key === "Home") {
        run(null, () => moveSelectionEdge(pane, false));
        return;
      }
      if (key === "End") {
        run(null, () => moveSelectionEdge(pane, true));
        return;
      }
      if (key === "Enter") {
        // Enter on a focused button presses that button; only elsewhere does it open the folder.
        if (ev.target instanceof Element && ev.target.closest("button, a[href], summary")) return;
        run(null, () => activateSelectedFolder(pane));
        return;
      }
      if (key === "Escape") {
        run(null, () => {
          hideNameTooltip();
          const prevSel = new Set(state[pane].selected);
          state[pane].selected.clear();
          state[pane].anchorPath = null;
          applySelectionDiffToDom(pane, prevSel);
        });
        return;
      }
      if (ev.altKey && key === "ArrowLeft") {
        run("back", () => goBack(pane));
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && key.toLowerCase() === "n") {
        run("mkdir", () => void (pane === "cart" ? promptMkdirCart() : promptMkdirPc()));
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && key === "ArrowRight") {
        if (pane !== "cart") {
          ev.preventDefault();
          return;
        }
        run("transfer", () => void copyCartToPcPaths([...state.cart.selected]));
        return;
      }
      if ((ev.ctrlKey || ev.metaKey) && ev.shiftKey && key === "ArrowLeft") {
        if (pane !== "pc") {
          ev.preventDefault();
          return;
        }
        run("transfer", () => void copyPcToCartPaths([...state.pc.selected]));
        return;
      }
    },
    true
  );
}

function navigate(pane, path, pushHist) {
  // Navigating would reload the pane under a running operation (and, on the cart, reopen its port).
  if (isPaneBusy(pane)) return;
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
  if (actionBlockedReason("back", pane)) return;
  const s = state[pane];
  s.histIndex--;
  s.path = s.history[s.histIndex];
  if (pane === "cart") void loadCartPane();
  else void loadPcPane();
}

async function goUp(pane) {
  if (actionBlockedReason("up", pane)) return;
  const s = state[pane];
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
 * Fill a serial port select: "Auto-detect" (or "No serial ports" when there are none), then each
 * port. The app bar and Settings both use it, so the two lists cannot differ.
 * @param {HTMLSelectElement} sel
 * @param {string[]} ports
 */
function fillSerialPortOptions(sel, ports) {
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
}

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
    fillSerialPortOptions(sel, ports);
    if (saved && [...sel.options].some((o) => o.value === saved)) sel.value = saved;
    // Settings' copy is refilled when Settings opens; leave an open form's edit alone.
    const settingsSel = document.getElementById("explorer-serial-port");
    if (settingsSel instanceof HTMLSelectElement && !isExplorerSettingsOpen()) {
      fillSerialPortOptions(settingsSel, ports);
      settingsSel.value = sel.value;
    }
  } finally {
    if (needFetch) endUsbLoading();
  }
}

/**
 * Relist the serial ports and reload the cart pane: the hotplug poll, F5 and Refresh on the cart
 * pane. The port select can change with the list, so the backend is told before the pane reloads.
 * @param {string[] | undefined} [ports] the list, when the caller already fetched it
 */
async function refreshCartPortsAndPane(ports) {
  await refreshUsbComPorts(ports);
  await syncPreferredComToBackend();
  await refreshUsbDetectHint();
  await loadCartPane({ forceRefresh: true });
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
  if (isExplorerSettingsOpen()) return;
  // An operation holds the port. The snapshot is left alone, so the first poll after it ends
  // still sees the change.
  if (isPaneBusy("cart") || isPaneBusy("pc")) return;
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

    await refreshCartPortsAndPane(ports);
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
    el.textContent = `Scan tries these first: ${hints.join(", ")}, then a wider grid over the cart ROM range. If several matches appear, pick the one that lists your SD card correctly.`;
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
      const tried = countNoun(checked, "address", "addresses");
      await showExplorerAlert(
        `No automatic match (${tried} tried). Try another USB port, close other apps using the cart, or enter a base address manually.`,
        { title: "No SD base found" },
      );
      finishOperationProgress(`No automatic match (${tried} tried).`, true, "cart");
    } else if (candidates.length === 1) {
      if (input) input.value = formatEd64LinearBaseForInput(candidates[0]);
      finishOperationProgress("SD base address filled in.", false, "cart");
    } else {
      if (input) input.value = formatEd64LinearBaseForInput(candidates[0]);
      const list = candidates.map((x) => formatEd64LinearBaseForInput(x)).join(", ");
      await showExplorerAlert(
        `Several possible bases: ${list}. The first is filled in — save Settings and try the cart pane; if listing fails, try the next value.`,
        { title: "Several possible SD bases" },
      );
      finishOperationProgress("Several possible bases — see the alert.", false, "cart");
    }
    return true;
  } catch (e) {
    if (isCancelledBackendError(e)) {
      scanCancelled = true;
      if (status) status.textContent = "Scan cancelled.";
      finishOperationCancelled("Scan cancelled.", "cart");
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

  const ok = await showExplorerConfirm(
    "Experimental: Xfer64 can scan cart memory for data that looks like an SD card. The EverDrive-64 X7's USB protocol has no SD command, so this is not expected to find your card. The scan sends many read commands and may take about a minute or longer.\n\nScan now?\n\nYou can cancel and use \"Scan for SD base\" in Settings → EverDrive SD (experimental) later.",
    { title: "Scan the EverDrive-64 X7 for an SD base?", okLabel: "Scan" },
  );
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
  hintEl.classList.toggle("hint-warning", v === "ed64_beta" || v === "ed64_pro");
  if (v === "ed64_beta") {
    try {
      const s = await invoke("explorer_get_settings");
      const base = s.ed64RomLinearBase;
      if (base != null && base !== "") {
        hintEl.textContent =
          "Experimental SD browsing is on, but it reads cart memory and is not expected to show your card. Adjust the address under EverDrive SD (experimental) if needed.";
      } else {
        hintEl.textContent =
          "SD browsing on the EverDrive-64 X7 is experimental and not expected to show your card (see EverDrive SD (experimental) below). For SD card access over USB, choose SummerCart64 above.";
      }
    } catch {
      hintEl.textContent =
        "SD browsing on the EverDrive-64 X7 is experimental (see EverDrive SD (experimental) below). For SD card access over USB, use a SummerCart64.";
    }
  } else if (v === "ed64_pro") {
    hintEl.textContent =
      "Experimental: SD file access through the EverDrive-64 PRO's USB link, ported from Krikzz's sources and never tested on a cart. Renaming copies the item and then deletes the original, and Xfer64 asks before the first write.";
  } else if (v === "sc64") {
    hintEl.textContent = "Full SD file access over USB serial (FAT or exFAT) for the SummerCart64.";
  } else {
    hintEl.textContent =
      "Checks each serial port and uses the first SummerCart64 or EverDrive that answers. Other serial devices are ignored; choose a cart yourself if the wrong one is picked.";
  }
}

/** Updates COM row + pane subtitle from backend probe (Settings mode + last USB probe). */
async function refreshUsbDetectHint() {
  beginUsbLoading("Detecting cart…");
  try {
    try {
      const s = await invoke("explorer_get_settings");
      const mode = normalizeCartDeviceSetting(s.cartDevice);
      const hint = document.getElementById("explorer-pane-cart-hint");
      const usbHint = document.getElementById("usb-hint");
      if (mode !== "auto") {
        lastAutoUsbCartKind = "unset";
        if (hint) hint.textContent = cartPaneBadge(mode);
        if (usbHint) usbHint.textContent = `Manual: ${cartFullName(mode)}`;
        return;
      }
      const hasEd64Base = s.ed64RomLinearBase != null && s.ed64RomLinearBase !== "";
      await withCartDaemonYield(async () => {
        const st = await invoke("cart_serial_probe_status");
        const detected = st.detectedKind === "unknown" ? "" : st.detectedKind || "";
        const found = detected === "sc64" || detected === "ed64pro" || detected === "ed64";
        if (hint) hint.textContent = found ? cartPaneBadge(detected) : "Not detected";
        if (usbHint) {
          // The select beside this already says Auto-detect; show only the result.
          const p = st.resolvedPort || "";
          usbHint.textContent = found ? `${cartFullName(detected)}${p ? ` on ${p}` : ""}` : "Not detected";
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
      if (usbHint) usbHint.textContent = "Not detected";
    }
  } finally {
    endUsbLoading();
  }
}

/**
 * Full cart name for running text and the app bar, from a Settings mode or a detected kind.
 * @param {string} kind `sc64`, `ed64_beta` / `ed64`, or `ed64_pro` / `ed64pro`
 */
function cartFullName(kind) {
  if (kind === "ed64_beta" || kind === "ed64") return "EverDrive-64 X7 (beta)";
  if (kind === "ed64_pro" || kind === "ed64pro") return "EverDrive-64 PRO (beta)";
  return "SummerCart64";
}

/**
 * Short form for the cart pane's corner badge only, where space is tight.
 * @param {string} kind same values as `cartFullName`
 */
function cartPaneBadge(kind) {
  if (kind === "ed64_beta" || kind === "ed64") return "X7 (beta)";
  if (kind === "ed64_pro" || kind === "ed64pro") return "PRO (beta)";
  return "SC64";
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
  if (hint) hint.textContent = v === "auto" ? "Auto-detect" : cartPaneBadge(v);
  updateEd64AdvancedSectionVisibility();
  void updateCartDeviceSettingsHint();
  if (!skipUsbRefresh) {
    void refreshUsbDetectHint();
  }
}

/**
 * Switch to cart type `value`. The app bar's Cart select and Settings' Save both come through
 * here, so the two cannot drift: save it, keep both Cart selects on it, and on a real change drop
 * the probe cache and any staged drag copies (they may be from another card), then refresh the hint
 * and reload the cart pane. Choosing the EverDrive-64 X7 with no SD base offers the scan.
 * @param {string} value `auto`, `sc64`, `ed64_beta` or `ed64_pro`
 * @param {{ previous?: string, reload?: boolean, offerScan?: boolean }} [opts]
 *   `previous`: the cart type before this change, when the caller has already saved `value` with
 *   other settings (Save settings does, in one write). Omitted, the saved settings are read and only
 *   `cartDevice` is written.
 *   `reload` (default true): false leaves the hint refresh and cart pane reload to the caller, so a
 *   Save that also changes the serial port reloads once.
 *   `offerScan` (default true): false when the caller decides on the scan offer itself.
 * @returns {Promise<boolean>} whether the cart type changed
 */
async function applyCartDevice(value, opts = {}) {
  const next = normalizeCartDeviceSetting(value);
  let previous = opts.previous;
  if (previous === undefined) {
    const prev = await invoke("explorer_get_settings");
    previous = normalizeCartDeviceSetting(prev.cartDevice);
    if (previous !== next) {
      const settings = { ...prev, cartDevice: next };
      await invoke("explorer_set_settings", { settings });
      lastExplorerSettingsCommit = buildExplorerSettingsCommit(settings);
    }
  }
  const changed = normalizeCartDeviceSetting(previous) !== next;
  explorerSettingsCartDeviceAtLoad = next;
  for (const id of ["select-cart-device", "explorer-cart-device"]) {
    const sel = document.getElementById(id);
    if (sel instanceof HTMLSelectElement) sel.value = next;
  }
  if (changed) {
    await invoke("cart_serial_invalidate_probe_cache");
    await forgetStagedCartDrag();
  }
  applyCartDeviceUi({ skipUsbRefresh: true });
  if (changed && opts.reload !== false) {
    await refreshUsbDetectHint();
    await loadCartPane({ forceRefresh: true });
  }
  if (changed && next === "ed64_beta" && opts.offerScan !== false) {
    // After the reload, and outside any dialog, like Save settings does.
    setTimeout(() => void maybeOfferEd64AutoScan(), 0);
  }
  return changed;
}

/**
 * Switch to serial port `value` ("" is Auto-detect). The app bar's Serial port select and Settings'
 * Save both come through here: keep the app bar on it, remember it for the next run, drop staged
 * drag copies, tell the backend, then refresh the hint and reload the cart pane.
 * @param {string} value
 * @param {{ reload?: boolean }} [opts] `reload: false` leaves the hint refresh and reload to the caller.
 */
async function applySerialPort(value, opts = {}) {
  const sel = document.getElementById("select-usb-com");
  // A port unplugged meanwhile shows as Auto-detect, exactly as at startup, and is picked again
  // when it comes back.
  if (sel instanceof HTMLSelectElement) {
    sel.value = [...sel.options].some((o) => o.value === value) ? value : "";
  }
  localStorage.setItem(LS_USB_COM, value);
  await forgetStagedCartDrag();
  await syncPreferredComToBackend();
  if (opts.reload !== false) {
    await refreshUsbDetectHint();
    await loadCartPane({ forceRefresh: true });
  }
}

/** Drops a stale answer when the hint is asked for again before the backend replied. */
let serialPortHintSeq = 0;

/**
 * Settings' Serial port hint. It speaks only when Auto-detect can't pick a port for the chosen cart;
 * what was found belongs under Cart. With Cart on Auto-detect the backend probes every port, so
 * choosing one is detection itself and needs no hint here. With a cart chosen it does not probe: it
 * takes the port whose USB name matches that cart, or the only USB serial port
 * (`cart_serial_suggest_port`), and that can find nothing.
 *
 * The backend answers for the saved cart, so an unsaved Cart edit shows no hint rather than a guess.
 */
async function updateSerialPortSettingsHint() {
  const hintEl = document.getElementById("explorer-serial-port-hint");
  if (!hintEl) return;
  const cart = normalizeCartDeviceSetting(document.getElementById("explorer-cart-device")?.value);
  const portSel = document.getElementById("explorer-serial-port");
  const onAuto = (portSel?.value ?? "") === "";
  const seq = ++serialPortHintSeq;
  let text = "";
  if (cart !== "auto" && onAuto && cart === explorerSettingsCartDeviceAtLoad) {
    let suggested = null;
    let known = true;
    try {
      suggested = await invoke("cart_serial_suggest_port");
    } catch {
      known = false;
    }
    if (seq !== serialPortHintSeq) return;
    if (known && !suggested) {
      const noPorts = portSel instanceof HTMLSelectElement && portSel.options.length <= 1;
      text = noPorts
        ? `No serial ports found. Plug in the ${cartFullName(cart)} over USB.`
        : `Auto-detect can't pick a serial port for the ${cartFullName(cart)}: no port identifies as one by its USB name. Choose its serial port.`;
    }
  }
  hintEl.textContent = text;
  hintEl.hidden = text === "";
}

function isExplorerSettingsOpen() {
  const panel = document.getElementById("explorer-settings-panel");
  return panel != null && !panel.hidden;
}

/** @type {((opts?: { restoreFocus?: boolean }) => void) | null} */
let releaseSettingsFocus = null;

function setExplorerSettingsOpen(open) {
  const panel = document.getElementById("explorer-settings-panel");
  const backdrop = document.getElementById("explorer-settings-backdrop");
  const opener = document.getElementById("btn-open-settings");
  if (!panel || !backdrop) return;
  panel.hidden = !open;
  backdrop.hidden = !open;
  opener?.setAttribute("aria-expanded", open ? "true" : "false");
  backdrop.setAttribute("aria-hidden", open ? "false" : "true");
  document.documentElement.classList.toggle("dialog-open", open);
  document.body.classList.toggle("dialog-open", open);
  if (open) {
    if (!releaseSettingsFocus) {
      releaseSettingsFocus = trapDialogFocus(panel, {
        initialFocus: document.getElementById("btn-close-settings"),
        // Escape is the close icon: it asks before discarding unsaved edits.
        onEscape: () => void requestCloseExplorerSettings(),
        fallbackFocus: opener,
      });
    }
  } else {
    const release = releaseSettingsFocus;
    releaseSettingsFocus = null;
    release?.();
    void loadExplorerSettings();
  }
}

/**
 * The Settings values that Save settings writes, as last filled in by `loadExplorerSettings()`.
 * Appearance and Send to act immediately, so they are not part of it.
 * @type {string | null}
 */
let explorerSettingsFormSnapshot = null;

function readExplorerSettingsForm() {
  return JSON.stringify({
    cartDevice: document.getElementById("explorer-cart-device")?.value ?? "",
    serialPort: document.getElementById("explorer-serial-port")?.value ?? "",
    developerMode: document.getElementById("explorer-developer-mode")?.checked ?? false,
    ed64LinearBase: document.getElementById("explorer-ed64-linear-base")?.value ?? "",
  });
}

function explorerSettingsHaveUnsavedEdits() {
  return explorerSettingsFormSnapshot !== null && readExplorerSettingsForm() !== explorerSettingsFormSnapshot;
}

/** True while the discard confirm is up, so a second close gesture does not stack another. */
let explorerSettingsDiscardPending = false;

/** Close button, backdrop and Esc: ask before throwing away unsaved edits. Save closes directly. */
async function requestCloseExplorerSettings() {
  if (!isExplorerSettingsOpen() || explorerSettingsDiscardPending) return;
  if (explorerSettingsHaveUnsavedEdits()) {
    explorerSettingsDiscardPending = true;
    let discard = false;
    try {
      discard = await showExplorerConfirm("Discard unsaved changes to Settings?", {
        title: "Discard changes?",
        okLabel: "Discard",
      });
    } finally {
      explorerSettingsDiscardPending = false;
    }
    if (!discard) return;
  }
  setExplorerSettingsOpen(false);
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
    const appBarCart = document.getElementById("select-cart-device");
    if (appBarCart instanceof HTMLSelectElement) appBarCart.value = nextCommit.cartDevice;
    // Serial port is not in the settings file: Settings shows what the app bar is using.
    const appBarPort = document.getElementById("select-usb-com");
    const portSel = document.getElementById("explorer-serial-port");
    if (appBarPort instanceof HTMLSelectElement && portSel instanceof HTMLSelectElement) {
      portSel.innerHTML = appBarPort.innerHTML;
      portSel.value = appBarPort.value;
    }
    const baseEl = document.getElementById("explorer-ed64-linear-base");
    if (baseEl) {
      baseEl.value = formatEd64LinearBaseForInput(s.ed64RomLinearBase);
    }
    explorerSettingsFormSnapshot = readExplorerSettingsForm();
    updateExplorerDevShellButton();
    void updateSerialPortSettingsHint();
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

/** @type {((opts?: { restoreFocus?: boolean }) => void) | null} */
let releaseHelpFocus = null;

function setExplorerHelpOpen(open) {
  const root = document.getElementById("explorer-help-modal");
  const opener = document.getElementById("btn-explorer-help");
  if (!root) return;
  root.hidden = !open;
  root.setAttribute("aria-hidden", open ? "false" : "true");
  document.body.classList.toggle("explorer-modal-open", open);
  if (open) {
    setHelpTab("setup");
    void ensureHelpAboutVersion();
    const panel = root.querySelector('[role="dialog"]');
    if (!releaseHelpFocus && panel instanceof HTMLElement) {
      releaseHelpFocus = trapDialogFocus(panel, {
        initialFocus: document.getElementById("explorer-help-close"),
        onEscape: () => setExplorerHelpOpen(false),
        fallbackFocus: opener,
      });
    }
  } else {
    const release = releaseHelpFocus;
    releaseHelpFocus = null;
    release?.();
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
  // Escape is handled by trapDialogFocus while the dialog is open.
}

function setupExplorerSettings() {
  document.getElementById("btn-open-settings")?.addEventListener("click", () => {
    // Disabled while the cart pane is busy; this guards a click racing that.
    if (isPaneBusy("cart")) return;
    void loadExplorerSettings().then(() => setExplorerSettingsOpen(true));
  });
  document.getElementById("btn-close-settings")?.addEventListener("click", () => {
    void requestCloseExplorerSettings();
  });
  // Cancel is the close icon in words: it asks before discarding unsaved edits.
  document.getElementById("btn-cancel-settings")?.addEventListener("click", () => {
    void requestCloseExplorerSettings();
  });
  document.getElementById("explorer-settings-backdrop")?.addEventListener("click", () => {
    void requestCloseExplorerSettings();
  });
  document.getElementById("explorer-developer-mode")?.addEventListener("change", updateExplorerDevShellButton);
  document.getElementById("explorer-cart-device")?.addEventListener("change", () => {
    updateEd64AdvancedSectionVisibility();
    void updateCartDeviceSettingsHint();
    void updateSerialPortSettingsHint();
  });
  document.getElementById("explorer-serial-port")?.addEventListener("change", () => {
    void updateSerialPortSettingsHint();
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
    // Save can change the cart and the serial port, which a running operation is holding.
    if (isPaneBusy("cart")) {
      await showExplorerAlert("Wait for the current operation to finish, then save.");
      return;
    }
    const developerMode = document.getElementById("explorer-developer-mode")?.checked ?? false;
    const cartDevice = normalizeCartDeviceSetting(document.getElementById("explorer-cart-device")?.value);
    const previousCartDevice = explorerSettingsCartDeviceAtLoad;
    const serialPort = document.getElementById("explorer-serial-port")?.value ?? "";
    const serialPortChanged = serialPort !== (document.getElementById("select-usb-com")?.value ?? "");
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
            "Enter a valid linear ROM address (for example hex 0x10000000 or a decimal number), or leave the field blank to turn off experimental SD browsing.",
            { title: "Invalid address" },
          );
          return;
        }
        settings.ed64RomLinearBase = parsed === undefined ? null : parsed;
        offerEd64ScanAfterSave = parsed === undefined;
      }
      await invoke("explorer_set_settings", { settings });
      lastExplorerSettingsCommit = buildExplorerSettingsCommit(settings);
    } catch (e) {
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
      return;
    }
    // Before closing: closing reloads the form from the saved settings, which now hold this cart.
    explorerSettingsCartDeviceAtLoad = cartDevice;
    setExplorerSettingsOpen(false);
    try {
      // Cart first, then the port; the cart pane reloads once even when both changed.
      await applyCartDevice(cartDevice, {
        previous: previousCartDevice,
        reload: !serialPortChanged,
        offerScan: false,
      });
      if (serialPortChanged) {
        await applySerialPort(serialPort);
      }
    } catch (e) {
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
    }
    // Separately, and after: a log window that fails to open must not keep the cart and port unapplied.
    if (developerMode) {
      try {
        await invoke("explorer_open_dev_shell");
      } catch (e) {
        await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
      }
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
  // Escape is handled by trapDialogFocus, which gives it to the topmost dialog only.
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
    const wrap = document.getElementById(id);
    wrap?.addEventListener(
      "scroll",
      () => {
        hideNameTooltip();
        // Not a scroll away from the menu: the one that brought its row into view as it opened.
        const opened = contextMenuOpenScroll;
        if (opened && opened.wrap === wrap && opened.top === wrap.scrollTop && opened.left === wrap.scrollLeft) return;
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

  // Both app-bar selects apply at once, through the same helpers as Save settings. They are disabled
  // while the cart pane is busy; the checks here only guard against a change racing that.
  document.getElementById("select-cart-device")?.addEventListener("change", async () => {
    const sel = /** @type {HTMLSelectElement} */ (document.getElementById("select-cart-device"));
    if (isPaneBusy("cart")) {
      sel.value = explorerSettingsCartDeviceAtLoad;
      return;
    }
    try {
      await applyCartDevice(sel.value);
    } catch (e) {
      sel.value = explorerSettingsCartDeviceAtLoad;
      await showExplorerAlert(userFacingErrorMessage(e, { context: "general" }));
    }
  });

  document.getElementById("select-usb-com")?.addEventListener("change", async () => {
    const sel = /** @type {HTMLSelectElement} */ (document.getElementById("select-usb-com"));
    if (isPaneBusy("cart")) {
      const saved = localStorage.getItem(LS_USB_COM) || "";
      sel.value = [...sel.options].some((o) => o.value === saved) ? saved : "";
      return;
    }
    await applySerialPort(sel.value);
  });

  document.querySelectorAll("[data-action]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const action = btn.dataset.action;
      const pane = btn.dataset.pane;
      if (action === "back") goBack(pane);
      else if (action === "up") await goUp(pane);
      else if (action === "refresh") {
        if (pane === "cart") await refreshCartPortsAndPane();
        else await loadPcPane({ forceRefresh: true });
      } else if (action === "pick") {
        const picked = await invoke("pick_folder");
        if (picked) navigate(pane, picked, true);
      }
    });
  });

  // These buttons are disabled whenever `actionBlockedReason` gives a reason, and say why in their
  // tooltip; the checks here only guard against a click racing a state change.
  document.getElementById("btn-copy-to-pc")?.addEventListener("click", async () => {
    if (actionBlockedReason("transfer", "cart")) return;
    await copyCartToPcPaths([...state.cart.selected]);
  });

  document.getElementById("btn-copy-to-cart")?.addEventListener("click", async () => {
    if (actionBlockedReason("transfer", "pc")) return;
    await copyPcToCartPaths([...state.pc.selected]);
  });

  document.getElementById("btn-delete-cart")?.addEventListener("click", async () => {
    if (actionBlockedReason("delete", "cart")) return;
    await deleteSelectedCart();
  });

  document.getElementById("btn-delete-pc")?.addEventListener("click", async () => {
    if (actionBlockedReason("delete", "pc")) return;
    await deleteSelectedPc();
  });

  document.getElementById("btn-mkdir-cart")?.addEventListener("click", async () => {
    if (actionBlockedReason("mkdir", "cart")) return;
    await promptMkdirCart();
  });

  document.getElementById("btn-mkdir-pc")?.addEventListener("click", async () => {
    if (actionBlockedReason("mkdir", "pc")) return;
    await promptMkdirPc();
  });

  document.getElementById("btn-rename-cart")?.addEventListener("click", async () => {
    if (actionBlockedReason("rename", "cart")) return;
    await promptRenameCart();
  });

  document.getElementById("btn-rename-pc")?.addEventListener("click", async () => {
    if (actionBlockedReason("rename", "pc")) return;
    await promptRenamePc();
  });

  updateExplorerControls();

  setupTableWrapMarquee("cart");
  setupTableWrapMarquee("pc");
  setupOsFileDrops();

  for (const pane of ["cart", "pc"]) {
    document.getElementById(`explorer-operation-cancel-${pane}`)?.addEventListener("click", () => {
      requestProgressCancel();
    });
  }

  setupUsbSerialHotplug();
}

window.addEventListener("DOMContentLoaded", () => {
  setupExplorerGlobalContextMenuSuppression();
  void init();
});
