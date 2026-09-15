/**
 * Headless checks for the Xfer64 explorer's drag and drop.
 *
 * The real thing only exists on Windows against a cart, so this covers the half that is ours:
 * which backend command each gesture reaches, and with what arguments. `src/index.html` is served
 * as-is and `window.__TAURI__` is replaced with a stub that records every `invoke`, so a drag is
 * judged by the calls it produces — no cart, no serial port, no Tauri. The run also checks the
 * explorer's control states (disabled with nothing selected, the show-hidden toggle, the delayed
 * busy overlay) and keyboard access (dialog focus trap, Escape and focus return, dialogs asked for
 * while another is open, the context menu's arrow keys, sort headers), which need the same stubbed
 * page.
 *
 * What it cannot see: anything the OS owns. Whether Windows accepts the drag we start, whether
 * `tauri://drag-*` fires at all, and whether a real Explorer drop carries the paths we expect are
 * all beyond it. Green here means the frontend wiring holds, not that the feature works.
 *
 * Run: npm install && npx playwright install chromium && npm test
 */
import { chromium } from "playwright";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const FRONTEND_DIR = fileURLToPath(new URL("../src/", import.meta.url));

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".json": "application/json",
};

/** Serve `src/` for the run: the frontend is ES modules, which file:// URLs refuse to load. */
async function serveFrontend() {
  const server = createServer(async (req, res) => {
    const rel = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^([/\\])+/, "");
    // A Tauri window never asks for one; a browser does, and the checks below treat any console
    // error as a failure. Answer it here rather than blunt that check.
    if (rel === "favicon.ico") {
      res.writeHead(204).end();
      return;
    }
    try {
      const body = await readFile(join(FRONTEND_DIR, rel));
      res.writeHead(200, { "content-type": MIME[extname(rel)] || "application/octet-stream" });
      res.end(body);
    } catch {
      res.writeHead(404).end("not found");
    }
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, origin: `http://127.0.0.1:${server.address().port}` };
}

/**
 * Stand in for the Tauri bridge.
 *
 * Every `invoke` is recorded; the handler table answers the commands a boot and a copy need. An
 * unknown command resolves to null rather than throwing, so a command added elsewhere in the app
 * does not break these checks — only a command this file asserts on has to be kept in step.
 *
 * `scenario` sets up a fresh page for the checks at the end of this file (none for the main run):
 * - `settings`: fields merged into the settings file
 * - `localStorage`: keys written before the page loads
 * - `daemonUp`: multi64d answers the probe
 * - `releaseFails`: `explorer_daemon_release` rejects, as a timed-out request does
 * - `listFails`: cart paths whose listing rejects, or `"all"`
 * - `pickerPaths`: the files Quick upload was opened with
 */
function installTauriStub(scenario = {}) {
  const sc = scenario || {};
  for (const [k, v] of Object.entries(sc.localStorage || {})) localStorage.setItem(k, v);
  const calls = [];
  window.__TAURI_CALLS__ = calls;
  // Commands whose (possibly delayed) answer has been returned, in order.
  window.__TAURI_DONE__ = [];
  window.__TAURI_LISTENERS__ = {};

  const cartEntries = [
    { name: "roms", path: "/roms", isDir: true, size: 0, modifiedMs: 1, hidden: false },
    { name: "sm64.z64", path: "/sm64.z64", isDir: false, size: 8388608, modifiedMs: 2, hidden: false },
    { name: "mk64.z64", path: "/mk64.z64", isDir: false, size: 12582912, modifiedMs: 3, hidden: false },
  ];
  const pcEntries = [
    { name: "patches", path: "C:\\dl\\patches", isDir: true, size: 0, modifiedMs: 1, hidden: false },
    { name: "banjo.z64", path: "C:\\dl\\banjo.z64", isDir: false, size: 16777216, modifiedMs: 2, hidden: false },
  ];
  // A folder far taller than its pane, for the checks that scroll; `__TAURI_LONG_PC__` lists it.
  const longPcEntries = Array.from({ length: 300 }, (_, i) => {
    const name = `file-${String(i).padStart(3, "0")}.bin`;
    return { name, path: `C:\\dl\\${name}`, isDir: false, size: 1024, modifiedMs: 10 + i, hidden: false };
  });
  const listing = (entries) => ({ entries, total: entries.length, done: true, truncated: false, hasMore: false });
  const step = (over) => ({ srcPc: null, destPc: null, cartPath: null, bytes: 4, conflictIfExists: false, isDir: false, ...over });

  // The settings file: `explorer_set_settings` replaces it, as the backend does, so a check can see
  // what a save wrote come back on the next read.
  let settings = {
    developerMode: false, preferredCom: "", cartDevice: "auto", ed64RomLinearBase: null,
    savedCartFolder: "", quickUploadCartPath: "", quickUploadOverwrite: false, autoDetect: true,
    ...(sc.settings || {}),
  };
  // `__TAURI_LIST_FAILS__` replaces `sc.listFails` mid-run: a cart that becomes readable.
  const listFails = (path) => {
    const f = window.__TAURI_LIST_FAILS__ ?? sc.listFails;
    return f === "all" || (Array.isArray(f) && f.includes(path));
  };

  const handlers = {
    fs_user_dirs: () => ({ home: "C:\\Users\\t", documents: "C:\\dl", desktop: "C:\\Users\\t\\Desktop" }),
    explorer_get_settings: () => ({ ...settings }),
    explorer_set_settings: (a) => { settings = { ...a.settings }; return null; },
    cart_serial_ed64_linear_hint_bases: () => [],
    // Port names, as `serialport::available_ports` gives them.
    cart_serial_list_ports: () => ["COM3", "COM4"],
    // A check sets `__TAURI_NO_SUGGEST__` to play a cart whose port Auto-detect can't pick.
    cart_serial_suggest_port: () => (window.__TAURI_NO_SUGGEST__ ? null : "COM3"),
    cart_serial_probe_status: () => ({ resolvedPort: "COM3", mode: settings.cartDevice, detectedKind: "sc64", message: null }),
    cart_serial_list_dir_page: (a) => {
      if (listFails(a.path)) throw new Error("Couldn't open the serial port: access denied.");
      return listing(cartEntries);
    },
    fs_list_dir_page: () => listing(window.__TAURI_LONG_PC__ ? longPcEntries : pcEntries),
    // The EverDrive SD base scan, finding nothing: it ends in an alert.
    cart_serial_probe_ed64_linear_base: () => ({ candidates: [], basesChecked: 12 }),
    xfer64_app_version: () => "0.0.0-test",
    explorer_daemon_probe: () => ({ up: sc.daemonUp === true }),
    explorer_daemon_release: () => {
      if (sc.releaseFails) throw new Error("POST http://127.0.0.1:38765/v1/serial/release: timeout");
      return null;
    },
    upload_picker_get_paths: () => sc.pickerPaths || [],
    // Held open until a check settles it through `__FINISH_UPLOAD__`.
    upload_picker_run: () => new Promise((resolve, reject) => { window.__FINISH_UPLOAD__ = { resolve, reject }; }),
    build_fs_copy_plan: (a) => (a.srcPaths || []).map((p) =>
      step({ mode: "fs", srcPc: p, destPc: `${a.destDir}\\${p.split("\\").pop()}` })),
    build_cart_import_plan: (a) => (a.fromPcPaths || []).map((p) =>
      step({ mode: "import", srcPc: p, cartPath: `${a.cartParent}/${p.split("\\").pop()}` })),
    build_cart_export_plan: (a) => (a.cartPaths || []).map((p) =>
      step({ mode: "export", cartPath: p, destPc: `${a.toPcParent}\\${p.split("/").pop()}` })),
    drag_staging_begin: () => "C:\\Temp\\xfer64-drag\\42-1\\d0",
  };

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        // The Channel passed to start_drag is not JSON; record a marker instead of serialising it.
        calls.push({ cmd, args: JSON.parse(JSON.stringify(args ?? {}, (k, v) => (k === "onEvent" ? "<channel>" : v))) });
        // A check can slow one command down to watch what the UI does while it runs.
        const delay = window.__TAURI_DELAYS__?.[cmd];
        if (delay) await new Promise((resolve) => setTimeout(resolve, delay));
        const result = handlers[cmd] ? handlers[cmd](args || {}) : null;
        window.__TAURI_DONE__.push(cmd);
        return result;
      },
      Channel: class { set onmessage(fn) { this._fn = fn; } },
    },
    event: {
      listen: async (name, cb) => {
        window.__TAURI_LISTENERS__[name] = cb;
        return () => {};
      },
    },
  };
}

const results = [];
const check = (name, pass, detail = "") => results.push({ name, pass, detail });

const { server, origin } = await serveFrontend();
const browser = await chromium.launch({
  // Set XFER64_E2E_CHROMIUM when the environment already has a browser that `playwright install`
  // did not put there; otherwise Playwright finds its own.
  executablePath: process.env.XFER64_E2E_CHROMIUM || undefined,
});
const page = await browser.newPage({ viewport: { width: 1100, height: 700 } });

const consoleErrors = [];
page.on("console", (m) => { if (m.type() === "error") consoleErrors.push(m.text()); });
page.on("pageerror", (e) => consoleErrors.push(`uncaught: ${e.message}`));

await page.addInitScript(installTauriStub);
await page.goto(`${origin}/index.html`);
await page.waitForSelector("#tbody-cart tr[data-path]", { timeout: 15000 });

// --- helpers -------------------------------------------------------------
const reset = () => page.evaluate(() => { window.__TAURI_CALLS__.length = 0; });
const cmds = () => page.evaluate(() => window.__TAURI_CALLS__.map((c) => c.cmd));
const callsOf = (name) => page.evaluate((n) => window.__TAURI_CALLS__.filter((c) => c.cmd === n), name);
const center = async (selector) => {
  const box = await page.locator(selector).boundingBox();
  if (!box) throw new Error(`no layout box for ${selector}`);
  return { x: Math.round(box.x + box.width / 2), y: Math.round(box.y + box.height / 2) };
};

/**
 * Drive one pointer drag. `upOutside` stops before the release, which is how a drag that left the
 * window is simulated — past that edge the app hands the gesture to the shell.
 */
const drag = (selector, from, to, { steps = 6, upOutside = false } = {}) =>
  page.evaluate(({ selector, from, to, steps, upOutside }) => {
    const row = document.querySelector(selector);
    const fire = (type, x, y, target) => target.dispatchEvent(new PointerEvent(type, {
      bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0, buttons: 1,
      pointerId: 1, pointerType: "mouse", isPrimary: true,
    }));
    fire("pointerdown", from.x, from.y, row);
    for (let i = 1; i <= steps; i++) {
      fire("pointermove", from.x + ((to.x - from.x) * i) / steps, from.y + ((to.y - from.y) * i) / steps, window);
    }
    if (!upOutside) fire("pointerup", to.x, to.y, window);
  }, { selector, from, to, steps, upOutside });

/** Fire a `tauri://drag-*` event the way Tauri does: physical pixels, not CSS ones. */
const osDrag = async (name, { at, paths } = {}) => {
  const scale = await page.evaluate(() => window.devicePixelRatio || 1);
  const payload = at ? { position: { x: at.x * scale, y: at.y * scale } } : {};
  if (paths) payload.paths = paths;
  await page.evaluate(({ name, payload }) => window.__TAURI_LISTENERS__[name]({ payload }), { name, payload });
};

const CART_FILE = '#tbody-cart tr[data-path="/sm64.z64"]';
const CART_FILE_2 = '#tbody-cart tr[data-path="/mk64.z64"]';
const CART_FOLDER = '#tbody-cart tr[data-path="/roms"]';
const PC_FILE = '#tbody-pc tr[data-path$="banjo.z64"]';
const PC_FOLDER = '#tbody-pc tr[data-path$="patches"]';

/** A control's enabled state as the user and assistive tech see it. */
const controlState = (selector) => page.evaluate((s) => {
  const el = document.querySelector(s);
  return { disabled: el.disabled, aria: el.getAttribute("aria-disabled"), title: el.title };
}, selector);

// --- controls that cannot act are off ------------------------------------
{
  const idle = await Promise.all(["#btn-rename-cart", "#btn-delete-pc", "#btn-copy-to-pc", "#btn-copy-to-cart"].map(controlState));
  check("with nothing selected, Rename, Delete, Export and Import are disabled and say why",
    idle.every((s) => s.disabled && s.aria === "true" && s.title.includes(" — ")), JSON.stringify(idle));
  const up = await controlState('[data-action="up"][data-pane="cart"]');
  const back = await controlState('[data-action="back"][data-pane="cart"]');
  check("Up and Back are disabled at the cart root with no history", up.disabled && back.disabled, JSON.stringify({ up, back }));

  await page.keyboard.press("F2");
  await page.keyboard.press("Delete");
  await page.waitForTimeout(150);
  const quiet = await page.evaluate(() => ({
    modal: !document.getElementById("explorer-modal-root").hidden,
    status: !document.getElementById("explorer-operation-cart").hidden,
  }));
  check("F2 and Delete with nothing selected do nothing", !quiet.modal && !quiet.status, JSON.stringify(quiet));

  await page.click(CART_FILE);
  await page.waitForTimeout(100);
  const picked = await Promise.all(["#btn-rename-cart", "#btn-delete-cart", "#btn-copy-to-pc"].map(controlState));
  check("selecting a cart file enables Rename, Delete and Export",
    picked.every((s) => !s.disabled && s.aria === "false" && !s.title.includes(" — ")), JSON.stringify(picked));
  await page.keyboard.press("Escape");
  await page.waitForTimeout(100);
  check("and clearing the selection disables them again", (await controlState("#btn-delete-cart")).disabled);

  const toggle = "#btn-show-hidden-pc";
  const pressed = () => page.evaluate((s) => {
    const b = document.querySelector(s);
    return { pressed: b.getAttribute("aria-pressed"), title: b.title };
  }, toggle);
  const before = await pressed();
  await page.click(toggle);
  await page.waitForTimeout(150);
  const on = await pressed();
  await page.click(toggle);
  await page.waitForTimeout(150);
  const off = await pressed();
  check("the show-hidden toggle reports its state and names what a click does",
    before.pressed === "false" && before.title === "Show hidden files" &&
      on.pressed === "true" && on.title === "Hide hidden files" && off.pressed === "false",
    JSON.stringify({ before, on, off }));
}

// --- pane to pane --------------------------------------------------------
await reset();
await drag(CART_FILE, await center(CART_FILE), await center("#tbody-pc"));
await page.waitForTimeout(400);
{
  const plans = await callsOf("build_cart_export_plan");
  check("a cart row dragged to the Windows pane exports it",
    plans.length === 1 && plans[0].args.cartPaths?.[0] === "/sm64.z64", JSON.stringify(plans.map((p) => p.args)));
  check("with no row under the pointer it lands in the pane's folder",
    plans[0]?.args?.toPcParent === "C:\\dl", String(plans[0]?.args?.toPcParent));
}

await reset();
await drag(CART_FILE_2, await center(CART_FILE_2), await center(PC_FOLDER));
await page.waitForTimeout(400);
check("a drop on a folder row targets that folder",
  (await callsOf("build_cart_export_plan"))[0]?.args?.toPcParent === "C:\\dl\\patches");

await reset();
await drag(PC_FILE, await center(PC_FILE), await center(PC_FOLDER));
await page.waitForTimeout(300);
check("a drag within one pane copies nothing",
  !(await cmds()).some((c) => c.startsWith("build_")), (await cmds()).join(","));

await reset();
await page.click(PC_FILE);
await page.waitForTimeout(150);
check("a plain click still selects the row",
  (await page.locator(`${PC_FILE}.selected`).count()) === 1);

// --- drops from Explorer -------------------------------------------------
await reset();
{
  const at = await center(CART_FOLDER);
  await osDrag("tauri://drag-over", { at });
  const litDuringDrag = await page.locator("#tbody-cart tr.drag-over-drop-target").count();
  await osDrag("tauri://drag-drop", { at, paths: ["C:\\dl\\oot.z64"] });
  await page.waitForTimeout(400);
  const plans = await callsOf("build_cart_import_plan");
  check("an Explorer drop on the cart pane imports", plans.length === 1, JSON.stringify(plans.map((p) => p.args)));
  // normalizeUsbPath drops the leading slash; that is the cart-path form the whole app uses.
  check("it targets the hovered cart folder", plans[0]?.args?.cartParent === "roms", String(plans[0]?.args?.cartParent));
  check("hovering an OS drag lights the folder row", litDuringDrag === 1, `count=${litDuringDrag}`);
  check("the highlight is cleared after the drop",
    (await page.locator(".drag-over-drop-target, .drag-over-target").count()) === 0);
}

await reset();
await osDrag("tauri://drag-drop", { at: await center("#tbody-pc"), paths: ["C:\\other\\zelda.z64"] });
await page.waitForTimeout(400);
{
  const plans = await callsOf("build_fs_copy_plan");
  check("an Explorer drop on the Windows pane is a file copy",
    plans.length === 1 && plans[0].args.destDir === "C:\\dl", JSON.stringify(plans.map((p) => p.args)));
  check("and the copy actually runs", (await cmds()).includes("fs_copy_one_file"), (await cmds()).join(","));
}

// --- dragging out --------------------------------------------------------
await reset();
await drag(PC_FILE, await center(PC_FILE), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(400);
{
  const starts = await callsOf("plugin:drag|start_drag");
  check("a Windows-pane drag-out hands the shell the real path",
    starts.length === 1 && starts[0].args.item?.[0] === "C:\\dl\\banjo.z64", JSON.stringify(starts.map((s) => s.args?.item)));
  check("the OS drag carries a drag image",
    String(starts[0]?.args?.image || "").startsWith("data:image/png;base64,"));
}

await reset();
await page.evaluate(() => document.querySelectorAll("#tbody-cart tr.selected").forEach((r) => r.classList.remove("selected")));
await drag(CART_FILE, await center(CART_FILE), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(600);
{
  const seen = await cmds();
  check("the first cart drag-out opens a staging directory", seen.includes("drag_staging_begin"), seen.join(","));
  check("it exports into that staging directory",
    (await callsOf("build_cart_export_plan"))[0]?.args?.toPcParent === "C:\\Temp\\xfer64-drag\\42-1\\d0");
  check("it does not start an OS drag yet", !seen.includes("plugin:drag|start_drag"), seen.join(","));
  const status = await page.locator("#explorer-operation-text-cart").textContent();
  check("and the status line says to drag again", /drag .* again/i.test(status || ""), String(status));
}

await reset();
await drag(CART_FILE, await center(CART_FILE), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(500);
{
  const starts = await callsOf("plugin:drag|start_drag");
  check("the second cart drag-out drags the staged copy",
    starts.length === 1 && starts[0].args.item?.[0] === "C:\\Temp\\xfer64-drag\\42-1\\d0\\sm64.z64",
    JSON.stringify(starts.map((s) => s.args?.item)));
  check("and does not export again", !(await cmds()).includes("drag_staging_begin"), (await cmds()).join(","));
}

// --- the ghost, and abandoning a drag ------------------------------------
await reset();
{
  const from = await center(CART_FILE_2);
  const to = await center("#tbody-pc");
  const seen = await page.evaluate(({ from, to, selector }) => {
    const row = document.querySelector(selector);
    const fire = (type, x, y, target) => target.dispatchEvent(new PointerEvent(type, {
      bubbles: true, cancelable: true, clientX: x, clientY: y, button: 0, buttons: 1,
      pointerId: 1, pointerType: "mouse", isPrimary: true,
    }));
    fire("pointerdown", from.x, from.y, row);
    fire("pointermove", to.x, to.y, window);
    const during = {
      ghost: document.querySelectorAll(".explorer-drag-ghost").length,
      dragging: document.body.classList.contains("explorer-dragging"),
    };
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    fire("pointerup", to.x, to.y, window);
    return {
      ...during,
      ghostAfter: document.querySelectorAll(".explorer-drag-ghost").length,
      draggingAfter: document.body.classList.contains("explorer-dragging"),
    };
  }, { from, to, selector: CART_FILE_2 });
  await page.waitForTimeout(300);
  check("a ghost follows the pointer while dragging", seen.ghost === 1 && seen.dragging, JSON.stringify(seen));
  check("the ghost goes when the drag ends", seen.ghostAfter === 0 && !seen.draggingAfter, JSON.stringify(seen));
  check("Escape abandons the drag without copying",
    !(await cmds()).some((c) => c.startsWith("build_")), (await cmds()).join(","));
}

// --- the busy overlay ----------------------------------------------------
await reset();
{
  await page.evaluate(() => { window.__TAURI_DELAYS__ = { fs_mkdir: 1500 }; });
  await page.click(PC_FILE);
  await page.keyboard.press("Escape");
  await page.keyboard.press("Control+Shift+N");
  await page.waitForSelector("#explorer-modal-input:not([hidden])");
  await page.waitForFunction(() => document.activeElement?.id === "explorer-modal-input");
  // Every change to the shade and to Refresh, stamped with the page's own clock: the timing checks
  // below compare these stamps, so a slow test runner cannot make them pass or fail.
  await page.evaluate(() => {
    const shade = document.getElementById("table-shade-pc");
    const refresh = document.querySelector('[data-action="refresh"][data-pane="pc"]');
    const log = (window.__SHADE_LOG__ = []);
    const snap = () => log.push({
      t: performance.now(),
      hidden: shade.hidden,
      pending: shade.classList.contains("explorer-table-shade--pending"),
      refreshDisabled: refresh.disabled,
    });
    snap();
    const observer = new MutationObserver(snap);
    observer.observe(shade, { attributes: true, attributeFilter: ["hidden", "class"] });
    observer.observe(refresh, { attributes: true, attributeFilter: ["disabled"] });
    window.__SHADE_OBSERVER__ = observer;
  });
  await page.keyboard.press("Enter");
  const reached = (fn, timeout = 5000) => page.waitForFunction(fn, null, { timeout }).then(() => true, () => false);
  const blocked = await reached(() => window.__SHADE_LOG__.some((s) => !s.hidden && s.pending));
  const drawn = await reached(() => window.__SHADE_LOG__.some((s) => !s.hidden && !s.pending));
  const cleared = await reached(() => {
    const log = window.__SHADE_LOG__;
    const i = log.findIndex((s) => !s.hidden && !s.pending);
    return i >= 0 && log.slice(i + 1).some((s) => s.hidden) && log.at(-1).hidden && !log.at(-1).refreshDisabled;
  });
  const timing = await page.evaluate(() => {
    window.__SHADE_OBSERVER__.disconnect();
    const log = window.__SHADE_LOG__;
    const first = log.find((s) => !s.hidden && s.pending);
    const shown = log.find((s) => !s.hidden && !s.pending);
    return {
      refreshDisabled: first?.refreshDisabled,
      drawnAfterMs: first && shown ? Math.round(shown.t - first.t) : null,
      log: log.map((s) => ({ ...s, t: Math.round(s.t) })),
    };
  });
  await page.evaluate(() => { window.__TAURI_DELAYS__ = {}; });
  check("an operation blocks its pane at once but draws no overlay yet",
    blocked && timing.refreshDisabled === true, JSON.stringify(timing));
  check("the overlay is drawn once the operation passes 300 ms, and not before",
    drawn && timing.drawnAfterMs >= 295 && timing.drawnAfterMs <= 900, JSON.stringify(timing));
  check("and removed when it ends, with the pane usable again", cleared, JSON.stringify(timing));
  check("the folder was created", (await cmds()).includes("fs_mkdir"), (await cmds()).join(","));
}

// --- dialogs and the context menu from the keyboard ----------------------
const activeId = () => page.evaluate(() => document.activeElement?.id || document.activeElement?.tagName || "");
const isHidden = (id) => page.evaluate((i) => document.getElementById(i).hidden, id);

await reset();
{
  await page.click("#btn-open-settings");
  await page.waitForSelector("#explorer-settings-panel:not([hidden])");
  await page.waitForTimeout(100);
  const opened = await activeId();
  check("Settings opens with focus on its close button", opened === "btn-close-settings", opened);

  await page.focus("#btn-save-explorer-settings");
  await page.keyboard.press("Tab");
  const wrapped = await activeId();
  await page.keyboard.press("Shift+Tab");
  const wrappedBack = await activeId();
  check("focus trap: Tab from the last Settings control wraps to the first, Shift+Tab wraps back",
    wrapped === "btn-close-settings" && wrappedBack === "btn-save-explorer-settings",
    JSON.stringify({ wrapped, wrappedBack }));

  // An unsaved edit makes Escape ask first, so the confirm opens on top of Settings.
  await page.click("#explorer-developer-mode");
  await page.keyboard.press("Escape");
  await page.waitForSelector("#explorer-modal-root:not([hidden])");
  const confirmFocus = await activeId();
  await page.keyboard.press("Tab");
  const confirmTab = await activeId();
  check("a dialog opened from Settings starts on its primary button and traps Tab itself",
    confirmFocus === "explorer-modal-ok" && confirmTab === "explorer-modal-cancel",
    JSON.stringify({ confirmFocus, confirmTab }));

  await page.keyboard.press("Escape");
  await page.waitForTimeout(100);
  const afterEsc = await page.evaluate(() => ({
    confirm: !document.getElementById("explorer-modal-root").hidden,
    settings: !document.getElementById("explorer-settings-panel").hidden,
    focus: document.activeElement?.id,
  }));
  check("Escape closes only the top dialog, and focus returns into the one underneath",
    !afterEsc.confirm && afterEsc.settings && afterEsc.focus === "explorer-developer-mode", JSON.stringify(afterEsc));

  await page.keyboard.press("Escape");
  await page.waitForSelector("#explorer-modal-root:not([hidden])");
  await page.keyboard.press("Enter");
  await page.waitForSelector("#explorer-settings-panel", { state: "hidden" });
  await page.waitForTimeout(100);
  const closed = await activeId();
  check("discarding closes Settings and returns focus to the Settings button", closed === "btn-open-settings", closed);
}

{
  await page.click("#btn-explorer-help");
  await page.waitForSelector("#explorer-help-modal:not([hidden])");
  const opened = await activeId();
  await page.keyboard.press("Tab");
  const wrapped = await activeId();
  await page.keyboard.press("Escape");
  await page.waitForTimeout(100);
  const after = { hidden: await isHidden("explorer-help-modal"), focus: await activeId() };
  check("Help opens on Close, Tab wraps inside it, and Escape returns focus to the Help button",
    opened === "explorer-help-close" && wrapped === "explorer-help-tab-setup" && after.hidden && after.focus === "btn-explorer-help",
    JSON.stringify({ opened, wrapped, after }));
}

{
  await page.click(CART_FILE);
  await page.waitForTimeout(100);
  await page.keyboard.press("Shift+F10");
  await page.waitForSelector("#explorer-context-menu:not([hidden])");
  const enabled = await page.evaluate(() =>
    [...document.querySelectorAll('#explorer-context-menu [role="menuitem"]')]
      .filter((b) => !b.disabled && !b.closest("[hidden]"))
      .map((b) => b.dataset.ctx));
  const item = () => page.evaluate(() => document.activeElement?.dataset?.ctx || "");
  const seen = { first: await item() };
  await page.keyboard.press("ArrowUp");
  seen.upWraps = await item();
  await page.keyboard.press("ArrowDown");
  seen.downWraps = await item();
  await page.keyboard.press("ArrowDown");
  seen.down = await item();
  await page.keyboard.press("End");
  seen.end = await item();
  await page.keyboard.press("Home");
  seen.home = await item();
  check("Shift+F10 opens the row's menu on its first enabled item, skipping disabled ones",
    enabled.length > 1 && !enabled.includes("open") && seen.first === enabled[0], JSON.stringify({ enabled, seen }));
  check("arrow keys in the context menu move between enabled items and wrap; Home and End reach the ends",
    seen.upWraps === enabled.at(-1) && seen.downWraps === enabled[0] && seen.down === enabled[1] &&
      seen.end === enabled.at(-1) && seen.home === enabled[0],
    JSON.stringify({ enabled, seen }));

  await page.keyboard.press("Escape");
  await page.waitForTimeout(100);
  const escaped = { hidden: await isHidden("explorer-context-menu"), focus: await activeId() };
  check("Escape closes the context menu and returns focus to the list it opened from",
    escaped.hidden && escaped.focus === "table-wrap-cart", JSON.stringify(escaped));

  await page.keyboard.press("ContextMenu");
  await page.waitForSelector("#explorer-context-menu:not([hidden])");
  await page.keyboard.press("End");
  const last = await item();
  await page.keyboard.press("Enter");
  await page.waitForSelector("#explorer-properties-modal:not([hidden])");
  await page.waitForTimeout(150);
  const propsFocus = await activeId();
  await page.keyboard.press("Escape");
  await page.waitForTimeout(100);
  const propsAfter = { hidden: await isHidden("explorer-properties-modal"), focus: await activeId() };
  check("Enter runs a menu item: Properties opens on Close, and Escape returns focus to the list",
    last === "properties" && propsFocus === "explorer-properties-close" && propsAfter.hidden && propsAfter.focus === "table-wrap-cart",
    JSON.stringify({ last, propsFocus, propsAfter }));
}

{
  // A list taller than its pane. Scrolling a list closes the context menu, so opening the menu from
  // the keyboard must not scroll a row that is already in view, nor close over a scroll it made itself.
  await page.evaluate(() => { window.__TAURI_LONG_PC__ = true; });
  await page.click('[data-action="refresh"][data-pane="pc"]');
  await page.waitForFunction(() => {
    const wrap = document.getElementById("table-wrap-pc");
    return document.querySelector('#tbody-pc tr[data-path$="file-000.bin"]') && wrap.scrollHeight > wrap.clientHeight * 3;
  }, null, { timeout: 5000 });
  // The lowest row wholly in view: centring it, as the menu used to, would scroll the list.
  const name = await page.evaluate(() => {
    const wrap = document.getElementById("table-wrap-pc");
    const bottom = wrap.getBoundingClientRect().top + wrap.clientHeight;
    const rows = [...document.querySelectorAll("#tbody-pc tr[data-path]")].filter((tr) => tr.getBoundingClientRect().bottom <= bottom);
    return rows.at(-1)?.dataset.path.split("\\").pop();
  });
  const row = `#tbody-pc tr[data-path$="${name}"]`;
  /** After two animation frames, by when any scroll event the key caused has fired. */
  const menuAfterFrames = () => page.evaluate(() => new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const menu = document.getElementById("explorer-context-menu");
      const first = [...menu.querySelectorAll('[role="menuitem"]')].find((b) => !b.disabled && !b.closest("[hidden]"));
      resolve({
        open: !menu.hidden,
        onFirstItem: first != null && document.activeElement === first,
        scrollTop: document.getElementById("table-wrap-pc").scrollTop,
      });
    }));
  }));

  await page.click(row);
  const before = await page.evaluate(() => document.getElementById("table-wrap-pc").scrollTop);
  await page.keyboard.press("Shift+F10");
  const inView = await menuAfterFrames();
  check("in a list taller than its pane, Shift+F10 on a row in view stays open on its first item without scrolling",
    Boolean(name) && inView.open && inView.onFirstItem && inView.scrollTop === before, JSON.stringify({ name, before, inView }));

  await page.keyboard.press("Escape");
  await page.evaluate(() => new Promise((resolve) => {
    document.getElementById("table-wrap-pc").scrollTop = 1e6;
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  }));
  await page.keyboard.press("Shift+F10");
  const scrolledBack = await menuAfterFrames();
  const rowShown = await page.evaluate((sel) => {
    const tr = document.querySelector(sel);
    const wrap = document.getElementById("table-wrap-pc").getBoundingClientRect();
    const r = tr?.getBoundingClientRect();
    return r != null && r.top >= wrap.top && r.bottom <= wrap.bottom;
  }, row);
  check("and on a row scrolled out of view, it brings the row back and the menu stays open",
    scrolledBack.open && scrolledBack.onFirstItem && rowShown, JSON.stringify({ scrolledBack, rowShown }));

  await page.keyboard.press("Escape");
  await page.evaluate(() => {
    delete window.__TAURI_LONG_PC__;
    document.getElementById("table-wrap-pc").scrollTop = 0;
  });
  await page.click('[data-action="refresh"][data-pane="pc"]');
  await page.waitForSelector(PC_FILE, { timeout: 5000 });
}

{
  const sort = await page.evaluate(() => {
    const th = document.querySelector('#table-cart th[data-sort-key="size"]');
    const btn = th.querySelector("button.explorer-sort-btn");
    btn.click();
    return { role: th.getAttribute("role"), tabindex: th.getAttribute("tabindex"), sort: th.getAttribute("aria-sort"),
      name: document.querySelector('#table-cart th[data-sort-key="name"]').getAttribute("aria-sort") };
  });
  await page.evaluate(() => document.querySelector('#table-cart th[data-sort-key="name"] button').click());
  check("a sort header is a column header holding a button, and aria-sort follows the sort",
    sort.role === null && sort.tabindex === null && sort.sort === "ascending" && sort.name === "none", JSON.stringify(sort));
}

// --- Cart and Serial port: app bar and Settings --------------------------
const selects = () => page.evaluate(() => ({
  appCart: document.getElementById("select-cart-device").value,
  appPort: document.getElementById("select-usb-com").value,
  cart: document.getElementById("explorer-cart-device").value,
  port: document.getElementById("explorer-serial-port").value,
  appPorts: [...document.getElementById("select-usb-com").options].map((o) => o.value),
  ports: [...document.getElementById("explorer-serial-port").options].map((o) => o.value),
  stored: localStorage.getItem("multi64.explorer.usbCom"),
}));

await reset();
{
  await page.selectOption("#select-cart-device", "sc64");
  await page.waitForTimeout(400);
  const seen = await cmds();
  const saved = (await callsOf("explorer_set_settings")).map((c) => c.args.settings);
  check("changing the app-bar Cart select saves the new cartDevice, keeping the other settings",
    saved.length === 1 && saved[0].cartDevice === "sc64" && saved[0].developerMode === false && "savedCartFolder" in saved[0],
    JSON.stringify(saved));
  check("and drops the probe cache and reloads the cart pane",
    seen.indexOf("cart_serial_invalidate_probe_cache") >= 0 &&
      seen.lastIndexOf("cart_serial_list_dir_page") > seen.indexOf("explorer_set_settings"),
    seen.join(","));
}

await reset();
{
  await page.evaluate(() => { window.__TAURI_DELAYS__ = { cart_serial_mkdir_cart: 900 }; });
  await page.click(CART_FILE);
  await page.keyboard.press("Escape");
  await page.keyboard.press("Control+Shift+N");
  await page.waitForSelector("#explorer-modal-input:not([hidden])");
  await page.waitForFunction(() => document.activeElement?.id === "explorer-modal-input");
  await page.keyboard.press("Enter");
  // Settings is off too: its Save can change the cart and the serial port.
  const appBar = ["#select-cart-device", "#select-usb-com", "#btn-open-settings"];
  const allDisabled = (want) => page.waitForFunction(
    ({ sels, want }) => sels.every((s) => document.querySelector(s).disabled === want), { sels: appBar, want }, { timeout: 5000 },
  ).catch(() => {});
  await allDisabled(true);
  const during = await Promise.all(appBar.map(controlState));
  await allDisabled(false);
  const after = await Promise.all(appBar.map(controlState));
  await page.evaluate(() => { window.__TAURI_DELAYS__ = {}; });
  check("both app-bar selects and the Settings button are disabled while the cart pane is busy, and say why",
    during.every((s) => s.disabled && s.aria === "true" && s.title.includes(" — wait for")), JSON.stringify(during));
  check("and enabled again when the operation ends",
    after.every((s) => !s.disabled && s.aria === "false" && !s.title.includes(" — ")), JSON.stringify(after));
  check("the cart folder was created", (await cmds()).includes("cart_serial_mkdir_cart"), (await cmds()).join(","));
}

await reset();
{
  await page.selectOption("#select-usb-com", "COM4");
  await page.waitForTimeout(400);
  const pinned = (await callsOf("cart_serial_set_preferred_com")).map((c) => c.args.port);
  check("changing the app-bar Serial port stores it and tells the backend",
    (await selects()).stored === "COM4" && pinned.at(-1) === "COM4", JSON.stringify(pinned));

  await page.click("#btn-open-settings");
  await page.waitForSelector("#explorer-settings-panel:not([hidden])");
  await page.waitForTimeout(150);
  const opened = await selects();
  check("Settings opens with Cart and Serial port matching the app bar",
    opened.cart === "sc64" && opened.cart === opened.appCart && opened.port === "COM4" && opened.port === opened.appPort &&
      opened.ports.join() === opened.appPorts.join() && opened.ports.join() === ",COM3,COM4",
    JSON.stringify(opened));

  await page.selectOption("#explorer-serial-port", "COM3");
  await page.click("#btn-close-settings");
  const asked = await page.waitForSelector("#explorer-modal-root:not([hidden])", { timeout: 2000 }).then(() => true, () => false);
  check("the unsaved-changes confirm fires for a Serial port-only edit", asked);
  if (asked) {
    await page.keyboard.press("Escape");
    await page.waitForTimeout(100);
  }

  await page.selectOption("#explorer-cart-device", "ed64_pro");
  await reset();
  await page.click("#btn-save-explorer-settings");
  await page.waitForSelector("#explorer-settings-panel", { state: "hidden" });
  await page.waitForTimeout(600);
  const saved = await selects();
  const seen = await cmds();
  const written = (await callsOf("explorer_set_settings")).map((c) => c.args.settings.cartDevice);
  const pinnedAfter = (await callsOf("cart_serial_set_preferred_com")).map((c) => c.args.port);
  check("saving Cart and Serial port in Settings updates both app-bar selects",
    saved.appCart === "ed64_pro" && saved.appPort === "COM3" && written.at(-1) === "ed64_pro", JSON.stringify({ saved, written }));
  check("and stores the port where the app bar does, and tells the backend",
    saved.stored === "COM3" && pinnedAfter.at(-1) === "COM3", JSON.stringify({ stored: saved.stored, pinnedAfter }));
  check("and reloads the cart pane once, after both are applied",
    seen.filter((c) => c === "cart_serial_list_dir_page").length === 1 &&
      seen.indexOf("cart_serial_list_dir_page") > seen.indexOf("cart_serial_set_preferred_com") &&
      seen.indexOf("cart_serial_set_preferred_com") > seen.indexOf("cart_serial_invalidate_probe_cache"),
    seen.join(","));
}

await reset();
{
  // With a cart chosen, Auto-detect takes the port the backend suggests; none means it can't pick.
  const hint = () => page.evaluate(() => {
    const el = document.getElementById("explorer-serial-port-hint");
    return { hidden: el.hidden, text: el.textContent };
  });
  await page.click("#btn-open-settings");
  await page.waitForSelector("#explorer-settings-panel:not([hidden])");
  await page.selectOption("#explorer-serial-port", "");
  await page.waitForTimeout(150);
  const suggested = await hint();
  await page.evaluate(() => { window.__TAURI_NO_SUGGEST__ = true; });
  await page.selectOption("#explorer-serial-port", "COM4");
  await page.selectOption("#explorer-serial-port", "");
  await page.waitForTimeout(150);
  const none = await hint();
  await page.selectOption("#explorer-cart-device", "auto");
  await page.waitForTimeout(150);
  const onAuto = await hint();
  await page.evaluate(() => { delete window.__TAURI_NO_SUGGEST__; });
  check("the Serial port hint shows only when Auto-detect can't pick a port for the chosen cart",
    suggested.hidden && !none.hidden && /can't pick a serial port for the EverDrive-64 PRO/.test(none.text) && onAuto.hidden,
    JSON.stringify({ suggested, none, onAuto }));
  await page.keyboard.press("Escape");
  await page.waitForSelector("#explorer-modal-root:not([hidden])");
  await page.keyboard.press("Enter");
  await page.waitForSelector("#explorer-settings-panel", { state: "hidden" });
}

await reset();
{
  // Alert, confirm and prompt share one dialog root. Settings' discard confirm is open when a scan
  // started from Settings ends in an alert: the alert must wait rather than take over the confirm's
  // root, where Enter used to answer the confirm underneath and leave the alert stuck.
  await page.evaluate(() => {
    window.__TAURI_DELAYS__ = { cart_serial_probe_ed64_linear_base: 700 };
    window.__TAURI_DONE__.length = 0;
  });
  await page.click("#btn-open-settings");
  await page.waitForSelector("#explorer-settings-panel:not([hidden])");
  await page.selectOption("#explorer-cart-device", "ed64_beta");
  await page.waitForSelector("#explorer-ed64-advanced-section:not([hidden])");
  await page.click("#btn-ed64-probe-linear-base");
  await page.keyboard.press("Escape");
  await page.waitForSelector("#explorer-modal-root:not([hidden])");
  await page.waitForFunction(() => window.__TAURI_DONE__.includes("cart_serial_probe_ed64_linear_base"), null, { timeout: 5000 });
  // Nothing should change on screen now, so there is no state to poll for: give the alert a moment.
  await page.waitForTimeout(250);
  const underConfirm = await page.evaluate(() => ({
    title: document.getElementById("explorer-modal-title").textContent,
    cancelShown: !document.getElementById("explorer-modal-cancel").hidden,
  }));
  check("an alert asked for while a confirm is open waits, leaving the confirm's text and buttons alone",
    underConfirm.title === "Discard changes?" && underConfirm.cancelShown, JSON.stringify(underConfirm));

  // Keep editing: Escape answers the confirm, and the waiting alert opens in its place.
  await page.keyboard.press("Escape");
  const alertOpened = await page.waitForFunction(() =>
    !document.getElementById("explorer-modal-root").hidden &&
      document.getElementById("explorer-modal-title").textContent === "No SD base found",
  null, { timeout: 3000 }).then(() => true, () => false);
  const alertFocus = await activeId();
  await page.keyboard.press("Enter");
  // The scan's own cleanup runs only once its alert has resolved.
  await page.waitForFunction(() => !document.getElementById("btn-ed64-probe-linear-base").disabled, null, { timeout: 3000 }).catch(() => {});
  const afterAlert = await page.evaluate(() => ({
    modal: !document.getElementById("explorer-modal-root").hidden,
    settings: !document.getElementById("explorer-settings-panel").hidden,
    cart: document.getElementById("explorer-cart-device").value,
    scanStatus: document.getElementById("explorer-ed64-probe-status").textContent,
    focusInSettings: document.getElementById("explorer-settings-panel").contains(document.activeElement),
  }));
  check("Enter on that alert answers the alert only: Settings stays open with its edit, and the scan finishes",
    alertOpened && alertFocus === "explorer-modal-ok" && !afterAlert.modal && afterAlert.settings &&
      afterAlert.cart === "ed64_beta" && afterAlert.scanStatus === "" && afterAlert.focusInSettings,
    JSON.stringify({ alertOpened, alertFocus, afterAlert }));

  // Both dialogs settled and left the dialog stack: Settings traps Tab again, and Escape asks again.
  await page.focus("#btn-save-explorer-settings");
  await page.keyboard.press("Tab");
  const wrapped = await activeId();
  await page.keyboard.press("Escape");
  const askedAgain = await page.waitForFunction(() =>
    !document.getElementById("explorer-modal-root").hidden &&
      document.getElementById("explorer-modal-title").textContent === "Discard changes?",
  null, { timeout: 2000 }).then(() => true, () => false);
  check("and both dialogs have left the dialog stack: Settings traps Tab again, and Escape asks to discard again",
    wrapped === "btn-close-settings" && askedAgain, JSON.stringify({ wrapped, askedAgain }));
  if (askedAgain) await page.keyboard.press("Enter");
  await page.waitForSelector("#explorer-settings-panel", { state: "hidden", timeout: 3000 }).catch(() => {});
  await page.evaluate(() => { window.__TAURI_DELAYS__ = {}; });
}

{
  // The app bar has to wrap, not scroll sideways, in a narrow window.
  const overflow = {};
  for (const width of [700, 900, 1000]) {
    await page.setViewportSize({ width, height: 700 });
    await page.waitForTimeout(100);
    overflow[width] = await page.evaluate(() => {
      const bar = document.querySelector(".explorer-appbar");
      const past = [...bar.querySelectorAll("label, select, button, #usb-hint")]
        .filter((el) => el.getBoundingClientRect().right > window.innerWidth + 0.5)
        .map((el) => el.id || el.tagName);
      return { scroll: bar.scrollWidth > bar.clientWidth, past };
    });
  }
  await page.setViewportSize({ width: 1100, height: 700 });
  check("the app bar wraps without overflowing at 700, 900 and 1000 px",
    Object.values(overflow).every((o) => !o.scroll && o.past.length === 0), JSON.stringify(overflow));
}

check("the page logged no errors", consoleErrors.length === 0, consoleErrors.join(" | "));

// --- fresh pages: the bridge, the saved cart folder, Quick upload ---------
/** Open `file` in its own page, with the stub playing `scenario`. */
async function openScenario(file, scenario) {
  const p = await browser.newPage({ viewport: { width: 1100, height: 700 } });
  await p.addInitScript(installTauriStub, scenario);
  await p.goto(`${origin}/${file}`);
  return p;
}
const callsIn = (p) => p.evaluate(() => window.__TAURI_CALLS__);
const until = (p, fn, timeout = 5000) => p.waitForFunction(fn, null, { timeout }).then(() => true, () => false);
const callLog = (calls) => calls.map((c) => (c.args?.path !== undefined ? `${c.cmd}(${c.args.path})` : c.cmd)).join(",");
/** Call `i` ran with the bridge paused: after a release, with the resume still to come. */
const whilePaused = (calls, i) => {
  if (i < 0) return false;
  const release = calls.slice(0, i).map((c) => c.cmd).lastIndexOf("explorer_daemon_release");
  return release >= 0 &&
    !calls.slice(release, i).some((c) => c.cmd === "explorer_daemon_resume") &&
    calls.slice(i + 1).some((c) => c.cmd === "explorer_daemon_resume");
};
const clearedSavedFolder = (calls) =>
  calls.filter((c) => c.cmd === "explorer_set_quick_upload_cart_path" && c.args.path === "");

{
  // The bridge holds the serial port and the cart can't be read; COM9 was saved and is unplugged.
  const p = await openScenario("index.html", {
    daemonUp: true,
    listFails: "all",
    settings: { quickUploadCartPath: "roms" },
    localStorage: { "multi64.explorer.usbCom": "COM9" },
  });
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "fs_list_dir_page"));
  await p.waitForTimeout(400);
  const calls = await callsIn(p);
  const probed = calls.findIndex((c) => c.cmd === "cart_serial_list_dir_page" && c.args.path === "roms");
  check("at startup the saved cart folder is checked with the bridge paused", whilePaused(calls, probed), callLog(calls));
  check("and a cart that can't be read leaves the saved cart folder alone",
    probed >= 0 && clearedSavedFolder(calls).length === 0, callLog(calls));
  const port = await p.evaluate(() => {
    const s = document.getElementById("select-usb-com");
    return { index: s.selectedIndex, value: s.value, label: s.selectedOptions[0]?.textContent ?? null };
  });
  check("a saved serial port that is unplugged at startup leaves the Serial port select on Auto-detect, not blank",
    port.index === 0 && port.value === "" && port.label === "Auto-detect", JSON.stringify(port));
  await p.close();
}

{
  const p = await openScenario("index.html", { settings: { quickUploadCartPath: "gone" }, listFails: ["gone"] });
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "fs_list_dir_page"));
  await p.waitForTimeout(400);
  const calls = await callsIn(p);
  check("a saved cart folder is cleared once the cart lists and the folder really is missing",
    clearedSavedFolder(calls).length === 1, callLog(calls));
  await p.close();
}

{
  // The release request times out; multi64d may still apply it afterwards.
  const p = await openScenario("index.html", { daemonUp: true, releaseFails: true });
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "explorer_daemon_release"));
  await p.waitForTimeout(400);
  const seen = (await callsIn(p)).map((c) => c.cmd);
  const released = seen.indexOf("explorer_daemon_release");
  check("a release that fails or times out is still followed by a resume",
    released >= 0 && seen.indexOf("explorer_daemon_resume", released) > released, seen.join(","));
  await p.close();
}

{
  const p = await openScenario("upload-picker.html", {
    daemonUp: true,
    pickerPaths: ["C:\\dl\\game.z64"],
    settings: { quickUploadCartPath: "roms" },
  });
  const ready = await until(p, () => document.getElementById("upload-picker-btn-upload")?.disabled === false);
  const calls = await callsIn(p);
  const lists = calls.map((c, i) => (c.cmd === "cart_serial_list_dir_page" ? i : -1)).filter((i) => i >= 0);
  check("Quick upload checks its saved folder and lists the cart with the bridge paused",
    ready && lists.length >= 2 && lists.every((i) => whilePaused(calls, i)), callLog(calls));

  const progress = (payload) => p.evaluate((pl) => window.__TAURI_LISTENERS__["explorer-progress"]({ payload: pl }), payload);
  const status = () => p.evaluate(() => {
    const bar = document.getElementById("upload-picker-status-bar");
    return {
      text: document.getElementById("upload-picker-status-text").textContent,
      warning: bar.classList.contains("upload-picker-status-bar--warning"),
      success: bar.classList.contains("upload-picker-status-bar--success"),
    };
  });
  const startUpload = async () => {
    await p.evaluate(() => { window.__FINISH_UPLOAD__ = null; });
    await p.click("#upload-picker-btn-upload");
    return until(p, () => Boolean(window.__FINISH_UPLOAD__) && typeof window.__TAURI_LISTENERS__["explorer-progress"] === "function");
  };

  const started = await startUpload();
  await progress({ done: 0, total: 200, message: 'Uploading to cart — "game.z64"…' });
  await progress({ done: 50, total: 200 });
  const midFile = (await status()).text;
  check("Quick upload progress keeps the file name on updates that carry none",
    started && midFile === 'Uploading to cart — "game.z64" (25%)…', midFile);

  await p.evaluate(() => window.__FINISH_UPLOAD__.resolve({
    uploaded: 1,
    skipped: 0,
    resumeWarning: "The Multi64 bridge was paused for this upload and could not be resumed: connection refused.",
  }));
  await until(p, () => !document.body.classList.contains("upload-picker-uploading"));
  const done = await status();
  check("a bridge that couldn't be resumed after Quick upload shows as a warning after the summary, not as success",
    done.warning && !done.success && done.text.startsWith("Uploaded 1 file to cart.") && done.text.includes("could not be resumed"),
    JSON.stringify(done));

  const restarted = await startUpload();
  await p.click("#upload-picker-btn-close");
  await progress({ done: 150, total: 200, message: 'Uploading to cart — "game.z64"…' });
  await progress({ done: 180, total: 200 });
  const cancelling = (await status()).text;
  check("and a progress update doesn't overwrite Cancelling…", restarted && cancelling === "Cancelling…", cancelling);
  await p.evaluate(() => window.__FINISH_UPLOAD__.reject("Cancelled"));
  await p.close();
}

{
  const p = await openScenario("upload-picker.html", {
    daemonUp: true,
    listFails: "all",
    pickerPaths: ["C:\\dl\\game.z64"],
    settings: { quickUploadCartPath: "roms" },
  });
  await until(p, () => !document.body.classList.contains("upload-picker-booting"));
  await p.waitForTimeout(300);
  const destination = () => p.evaluate(() => document.getElementById("upload-picker-selection").textContent);
  const whileUnreadable = { destination: await destination(), cleared: clearedSavedFolder(await callsIn(p)).length };
  check("Quick upload keeps its saved folder while the cart can't be read",
    whileUnreadable.destination === "Destination: /roms" && whileUnreadable.cleared === 0, JSON.stringify(whileUnreadable));

  // The cart becomes readable: the reconnect poll lists it, and the saved folder is still the destination.
  await p.evaluate(() => { window.__TAURI_LIST_FAILS__ = []; });
  const reconnected = await until(p, () => document.getElementById("upload-picker-btn-upload")?.disabled === false, 6000);
  const afterReconnect = { reconnected, destination: await destination(), cleared: clearedSavedFolder(await callsIn(p)).length };
  check("and uploads there once the cart can be read",
    reconnected && afterReconnect.destination === "Destination: /roms" && afterReconnect.cleared === 0, JSON.stringify(afterReconnect));
  await p.close();
}

// --- report --------------------------------------------------------------
await browser.close();
server.close();

let failed = 0;
for (const r of results) {
  if (!r.pass) failed++;
  console.log(`${r.pass ? "ok  " : "FAIL"}  ${r.name}${r.pass ? "" : `\n        ${r.detail}`}`);
}
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
