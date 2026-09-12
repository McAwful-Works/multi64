/**
 * Headless checks for the Xfer64 explorer's drag and drop.
 *
 * The real thing only exists on Windows against a cart, so this covers the half that is ours:
 * which backend command each gesture reaches, and with what arguments. `src/index.html` is served
 * as-is and `window.__TAURI__` is replaced with a stub that records every `invoke`, so a drag is
 * judged by the calls it produces — no cart, no serial port, no Tauri.
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
 */
function installTauriStub() {
  const calls = [];
  window.__TAURI_CALLS__ = calls;
  window.__TAURI_LISTENERS__ = {};
  window.__PROMISE_MODE__ = "ok";

  const cartEntries = [
    { name: "roms", path: "/roms", isDir: true, size: 0, modifiedMs: 1, hidden: false },
    { name: "sm64.z64", path: "/sm64.z64", isDir: false, size: 8388608, modifiedMs: 2, hidden: false },
    { name: "mk64.z64", path: "/mk64.z64", isDir: false, size: 12582912, modifiedMs: 3, hidden: false },
  ];
  const pcEntries = [
    { name: "patches", path: "C:\\dl\\patches", isDir: true, size: 0, modifiedMs: 1, hidden: false },
    { name: "banjo.z64", path: "C:\\dl\\banjo.z64", isDir: false, size: 16777216, modifiedMs: 2, hidden: false },
  ];
  const listing = (entries) => ({ entries, total: entries.length, done: true, truncated: false, hasMore: false });
  const step = (over) => ({ srcPc: null, destPc: null, cartPath: null, bytes: 4, conflictIfExists: false, isDir: false, ...over });

  const handlers = {
    fs_user_dirs: () => ({ home: "C:\\Users\\t", documents: "C:\\dl", desktop: "C:\\Users\\t\\Desktop" }),
    explorer_get_settings: () => ({
      developerMode: false, preferredCom: "", cartDevice: "auto", ed64RomLinearBase: null,
      savedCartFolder: "", quickUploadCartPath: "", quickUploadOverwrite: false, autoDetect: true,
    }),
    cart_serial_ed64_linear_hint_bases: () => [],
    cart_serial_list_ports: () => [{ port: "COM3", label: "COM3" }],
    cart_serial_suggest_port: () => "COM3",
    cart_serial_probe_status: () => ({ ok: true, kind: "sc64", port: "COM3", message: "SummerCart64" }),
    cart_serial_list_dir_page: () => listing(cartEntries),
    fs_list_dir_page: () => listing(pcEntries),
    xfer64_app_version: () => "0.0.0-test",
    explorer_daemon_probe: () => ({ up: false }),
    build_fs_copy_plan: (a) => (a.srcPaths || []).map((p) =>
      step({ mode: "fs", srcPc: p, destPc: `${a.destDir}\\${p.split("\\").pop()}` })),
    build_cart_import_plan: (a) => (a.fromPcPaths || []).map((p) =>
      step({ mode: "import", srcPc: p, cartPath: `${a.cartParent}/${p.split("\\").pop()}` })),
    build_cart_export_plan: (a) => (a.cartPaths || []).map((p) =>
      step({ mode: "export", cartPath: p, destPc: `${a.toPcParent}\\${p.split("/").pop()}` })),
    drag_staging_begin: () => "C:\\Temp\\xfer64-drag\\42-1\\d0",
    // The promise drag: window.__PROMISE_MODE__ lets a check make it fail, standing in for a
    // platform without promises.
    drag_start_cart_promise: () => {
      if (window.__PROMISE_MODE__ === "unsupported") {
        throw new Error("File promises are a Windows feature.");
      }
      // The shell accepted the drop but took nothing: DROPEFFECT_NONE.
      if (window.__PROMISE_MODE__ === "refused") {
        return { dropped: true, effect: 0 };
      }
      // The shell asked, but the cart could not be read.
      if (window.__PROMISE_MODE__ === "cartfailed") {
        return { dropped: true, effect: 0, error: "could not read /sm64.z64 from the cart: port busy" };
      }
      return { dropped: true, effect: 1 };
    },
  };

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        // The Channel passed to start_drag is not JSON; record a marker instead of serialising it.
        calls.push({ cmd, args: JSON.parse(JSON.stringify(args ?? {}, (k, v) => (k === "onEvent" ? "<channel>" : v))) });
        return handlers[cmd] ? handlers[cmd](args || {}) : null;
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
  const promises = await callsOf("drag_start_cart_promise");
  check("a cart drag-out promises the file instead of staging it",
    promises.length === 1, JSON.stringify(promises.map((p) => p.args)));
  check("the promise carries the name and size the shell needs up front",
    promises[0]?.args?.files?.[0]?.cartPath === "/sm64.z64" &&
      promises[0]?.args?.files?.[0]?.name === "sm64.z64" &&
      promises[0]?.args?.files?.[0]?.size === 8388608,
    JSON.stringify(promises[0]?.args?.files));
  check("nothing is exported to a staging directory",
    !seen.includes("drag_staging_begin") && !seen.includes("build_cart_export_plan"), seen.join(","));
  check("and there is no second gesture to wait for",
    !/drag .* again/i.test((await page.locator("#explorer-operation-text-cart").textContent()) || ""),
    String(await page.locator("#explorer-operation-text-cart").textContent()));
}

// --- a drop that copied nothing must say so ----------------------------
await reset();
await page.evaluate(() => {
  window.__PROMISE_MODE__ = "refused";
  document.querySelectorAll("#tbody-cart tr.selected").forEach((r) => r.classList.remove("selected"));
});
await drag(CART_FILE, await center(CART_FILE), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(500);
{
  const status = await page.locator("#explorer-operation-text-cart").textContent();
  check("a drop the shell took nothing from is reported, not called a success",
    /copied nothing/i.test(status || ""), String(status));
}
await page.evaluate(() => { window.__PROMISE_MODE__ = "ok"; });

// --- a failure reason reaches the status strip ---------------------------
await reset();
await page.evaluate(() => {
  window.__PROMISE_MODE__ = "cartfailed";
  document.querySelectorAll("#tbody-cart tr.selected").forEach((r) => r.classList.remove("selected"));
});
await drag(CART_FILE, await center(CART_FILE), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(500);
{
  const status = await page.locator("#explorer-operation-text-cart").textContent();
  check("a failed drag reports the cause, not just the symptom",
    /port busy/.test(status || ""), String(status));
}
await page.evaluate(() => { window.__PROMISE_MODE__ = "ok"; });

// --- a folder cannot be promised, so it stages ---------------------------
await reset();
await page.evaluate(() => document.querySelectorAll("#tbody-cart tr.selected").forEach((r) => r.classList.remove("selected")));
await drag(CART_FOLDER, await center(CART_FOLDER), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(600);
{
  const seen = await cmds();
  check("a cart folder falls back to staging", seen.includes("drag_staging_begin"), seen.join(","));
  check("a folder is not offered as a promise",
    !seen.includes("drag_start_cart_promise"), seen.join(","));
}

// --- and so does a platform without promises -----------------------------
await reset();
await page.evaluate(() => {
  window.__PROMISE_MODE__ = "unsupported";
  document.querySelectorAll("#tbody-cart tr.selected").forEach((r) => r.classList.remove("selected"));
});
await drag(CART_FILE_2, await center(CART_FILE_2), { x: -40, y: 300 }, { upOutside: true });
await page.waitForTimeout(600);
{
  const seen = await cmds();
  check("a failed promise falls back to staging",
    seen.includes("drag_start_cart_promise") && seen.includes("drag_staging_begin"), seen.join(","));
  const status = await page.locator("#explorer-operation-text-cart").textContent();
  check("and the fallback still says to drag again", /drag .* again/i.test(status || ""), String(status));
}
await page.evaluate(() => { window.__PROMISE_MODE__ = "ok"; });

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

check("the page logged no errors", consoleErrors.length === 0, consoleErrors.join(" | "));

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
