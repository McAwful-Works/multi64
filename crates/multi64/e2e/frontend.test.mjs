/**
 * Headless checks for the Multi64 window.
 *
 * `src/index.html` is served as-is and `window.__TAURI__` is replaced with a stub that records every
 * `invoke` and answers from a table, so the window is judged by what it shows for the backend's
 * answers and what it sends back: no bridge, no serial port, no Tauri.
 *
 * What it cannot see: anything the backend owns, such as which ports are really plugged in or
 * whether the bridge starts. Green here means the frontend wiring holds, not that the feature works.
 *
 * Run: npm ci && npx playwright install --with-deps chromium && npm test
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
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

/** Serve `src/` for the run: `main.js` is an ES module, which a file:// URL refuses to load. */
async function serveFrontend() {
  const server = createServer(async (req, res) => {
    const rel = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^([/\\])+/, "");
    // A Tauri window never asks for one; a browser does, and any console error fails the run.
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
 * Every `invoke` is recorded; an unknown command resolves to null rather than throwing, so a
 * command added elsewhere does not break these checks. `scenario`:
 * - `settings`: fields merged into the settings file
 * - `ports`: the port names the backend enumerates; `window.__TAURI_PORTS__` replaces it mid-run
 * - `running`: the bridge is running, started for `runningCart` (the backend's label)
 */
function installTauriStub(scenario = {}) {
  const sc = scenario || {};
  const calls = [];
  window.__TAURI_CALLS__ = calls;
  window.__TAURI_LISTENERS__ = {};

  // `set_settings` replaces it, as the backend does, so reopening Settings reads back what Save wrote.
  let settings = {
    serialPort: null, baud: 115200, listen: "127.0.0.1:38765", autoStartDaemon: true, autostartApp: false,
    minimizeToTrayOnClose: true, startMinimized: false, trayEnabled: true, developerMode: false,
    multi64dLogPreset: "default", cart: "auto",
    ...(sc.settings || {}),
  };
  // `SerialPortOptions`, with no SummerCart64 recognized by its USB IDs.
  const portOptions = () => ({
    ports: [...(window.__TAURI_PORTS__ ?? sc.ports ?? ["COM3", "COM4"])],
    auto: null,
    autoWarning: "No SummerCart64 found. Plug it in, or pick its serial port.",
    ambiguous: false,
    everdriveHint: "For a fixed Cart type, Auto-detect finds only a SummerCart64. Pick the EverDrive's serial port, or set Cart to Auto-detect.",
  });
  const cartLabels = { auto: "Auto-detect", sc64: "SummerCart64", ed64: "EverDrive-64 X7 (beta)", ed64pro: "EverDrive-64 PRO (beta)" };

  const handlers = {
    get_settings: () => ({ ...settings }),
    set_settings: (a) => { settings = { ...a.settings }; return null; },
    get_serial_port_options: () => portOptions(),
    get_daemon_status: (a) => ({
      running: sc.running === true,
      healthy: sc.running === true,
      listen: settings.listen,
      message: "",
      cart: sc.running ? sc.runningCart : cartLabels[settings.cart],
      ...(a.withPorts ? { portOptions: portOptions() } : {}),
    }),
    get_daemon_logs: () => [],
  };

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        calls.push({ cmd, args: JSON.parse(JSON.stringify(args ?? {})) });
        return handlers[cmd] ? handlers[cmd](args || {}) : null;
      },
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
  // Set MULTI64_E2E_CHROMIUM when the environment already has a browser that `playwright install`
  // did not put there; otherwise Playwright finds its own.
  executablePath: process.env.MULTI64_E2E_CHROMIUM || undefined,
});
const consoleErrors = [];

/** A fresh page on `scenario`, loaded to the end of the window's startup. */
async function openScenario(scenario) {
  const p = await browser.newPage({ viewport: { width: 900, height: 800 } });
  p.on("console", (m) => { if (m.type() === "error") consoleErrors.push(m.text()); });
  p.on("pageerror", (e) => consoleErrors.push(`uncaught: ${e.message}`));
  await p.addInitScript(installTauriStub, scenario);
  await p.goto(`${origin}/index.html`);
  // Startup subscribes to `daemon-changed` last, after the settings load and the first status
  // refresh have finished.
  await until(p, () => "daemon-changed" in window.__TAURI_LISTENERS__);
  return p;
}

const until = (p, fn, arg, timeout = 5000) =>
  p.waitForFunction(fn, arg, { timeout }).then(() => true, () => false);
const callsOf = (p, name) => p.evaluate((n) => window.__TAURI_CALLS__.filter((c) => c.cmd === n), name);

/** A select's options and current value, as the user sees them. */
const selectState = (p, id) => p.evaluate((i) => {
  const s = document.getElementById(i);
  return {
    value: s.value,
    selected: s.selectedOptions[0]?.textContent ?? null,
    options: [...s.options].map((o) => [o.value, o.textContent]),
  };
}, id);

const statusCard = (p) => p.evaluate(() => ({
  bridge: document.getElementById("status-running").textContent,
  cart: document.getElementById("status-cart").textContent,
  start: document.getElementById("btn-start").disabled,
  stop: document.getElementById("btn-stop").disabled,
}));

/** Open Settings and wait until it has loaded, which is when it moves focus to its close button. */
async function openSettings(p) {
  await p.click("#btn-open-settings");
  return until(p, () => document.activeElement?.id === "btn-close-settings");
}

// --- Status card -----------------------------------------------------------
{
  const p = await openScenario({ running: true, runningCart: "SummerCart64 on COM3", settings: { serialPort: "COM3", cart: "sc64" } });
  const card = await statusCard(p);
  check("a running bridge's Status card names its cart and serial port",
    card.bridge === "Running" && card.cart === "SummerCart64 on COM3", JSON.stringify(card));
  check("and offers Stop but not Start", card.start && !card.stop, JSON.stringify(card));
  await p.close();
}
{
  const p = await openScenario({ settings: { cart: "sc64" } });
  const card = await statusCard(p);
  check("a stopped bridge's Status card offers Start but not Stop",
    card.bridge === "Stopped" && card.cart === "SummerCart64" && !card.start && card.stop, JSON.stringify(card));
  await p.close();
}

// --- Settings: Cart and Serial port ---------------------------------------
{
  const p = await openScenario({ settings: { cart: "ed64pro", serialPort: "COM4" } });
  const loaded = await openSettings(p);
  const cart = await selectState(p, "cart");
  check("Settings lists the four carts and selects the saved one",
    loaded && cart.value === "ed64pro" && JSON.stringify(cart.options) === JSON.stringify([
      ["auto", "Auto-detect"], ["sc64", "SummerCart64"], ["ed64", "EverDrive-64 X7 (beta)"], ["ed64pro", "EverDrive-64 PRO (beta)"],
    ]), JSON.stringify(cart));
  const hint = () => p.evaluate(() => {
    const h = document.getElementById("cart-hint");
    return { hidden: h.hidden, warning: h.classList.contains("hint-warning"), text: h.textContent };
  });
  const pro = await hint();
  check("a chosen EverDrive shows the experimental warning under Cart",
    !pro.hidden && pro.warning && pro.text.startsWith("Experimental: the EverDrive-64 PRO"), JSON.stringify(pro));
  await p.selectOption("#cart", "sc64");
  const sc64 = await hint();
  check("and SummerCart64 shows none", sc64.hidden && !sc64.warning, JSON.stringify(sc64));

  const port = await selectState(p, "serial-port");
  check("Settings lists Auto-detect, then the ports, and selects the saved one",
    port.value === "COM4" && JSON.stringify(port.options) === JSON.stringify([["", "Auto-detect"], ["COM3", "COM3"], ["COM4", "COM4"]]),
    JSON.stringify(port));
  await p.close();
}

// --- #168: an unplugged saved serial port ------------------------------------
{
  const p = await openScenario({ settings: { cart: "sc64", serialPort: "COM7" }, ports: ["COM3", "COM4"] });
  const unplugged = (s) => s.value === "COM7" && s.selected === "COM7 (not connected)";

  const loaded = await openSettings(p);
  const opened = await selectState(p, "serial-port");
  check("#168: an unplugged saved port is listed as not connected and selected when Settings opens",
    loaded && unplugged(opened) && opened.options.map(([v]) => v).join() === ",COM3,COM4,COM7", JSON.stringify(opened));

  // The 2 s status poll refreshes the port list while Settings is open. Unplug COM4 so it rebuilds.
  const polls = (await callsOf(p, "get_daemon_status")).length;
  await p.evaluate(() => { window.__TAURI_PORTS__ = ["COM3"]; });
  const polled = await until(p, ([n]) => window.__TAURI_CALLS__.filter((c) => c.cmd === "get_daemon_status" && c.args.withPorts).length > 0 &&
    window.__TAURI_CALLS__.filter((c) => c.cmd === "get_daemon_status").length > n &&
    ![...document.getElementById("serial-port").options].some((o) => o.value === "COM4"), [polls]);
  const afterPoll = await selectState(p, "serial-port");
  check("#168: and stays selected when the status poll rebuilds the port list",
    polled && unplugged(afterPoll), JSON.stringify(afterPoll));

  await p.click("#btn-save");
  const closed = await until(p, () => document.getElementById("settings-panel").hidden);
  const saved = (await callsOf(p, "set_settings")).at(-1)?.args.settings.serialPort;
  check("#168: Save writes the unplugged port back, not Auto-detect", closed && saved === "COM7", JSON.stringify({ closed, saved }));

  await until(p, () => document.activeElement?.id === "btn-open-settings");
  const reloaded = await openSettings(p);
  const reopened = await selectState(p, "serial-port");
  check("#168: and it is still selected when Settings opens again", reloaded && unplugged(reopened), JSON.stringify(reopened));

  const enumerations = (await callsOf(p, "get_serial_port_options")).length;
  await p.click("#btn-ports");
  const refreshed = await until(p, ([n]) => window.__TAURI_CALLS__.filter((c) => c.cmd === "get_serial_port_options").length > n, [enumerations]);
  const afterRefresh = await selectState(p, "serial-port");
  check("#168: and after Refresh ports", refreshed && unplugged(afterRefresh), JSON.stringify(afterRefresh));

  const edits = await p.evaluate(() => {
    document.getElementById("btn-cancel-settings").click();
    return !document.getElementById("discard-panel").hidden;
  });
  check("#168: none of that counts as an unsaved edit", !edits);
  await p.close();
}

// --- The window fits the page ------------------------------------------------
{
  /** The height the page last asked the window for, and the page's own height. */
  const fit = (p) => p.evaluate(() => ({
    asked: window.__TAURI_CALLS__.filter((c) => c.cmd === "fit_window_height").at(-1)?.args.height,
    page: Math.ceil(document.body.getBoundingClientRect().height),
  }));
  const settle = (p, want) => until(p, ([w]) => {
    const asked = window.__TAURI_CALLS__.filter((c) => c.cmd === "fit_window_height").at(-1)?.args.height;
    return w === "page" ? asked === Math.ceil(document.body.getBoundingClientRect().height) : asked >= w;
  }, [want]);

  const plain = await openScenario({});
  await plain.setViewportSize({ width: 560, height: 640 });
  const plainFit = (await settle(plain, "page")) && await fit(plain);
  check("the window is fitted to the page: the Status card alone is well under the old 640 px",
    plainFit && plainFit.asked === plainFit.page && plainFit.page < 480, JSON.stringify(plainFit));

  const dev = await openScenario({ settings: { developerMode: true } });
  await dev.setViewportSize({ width: 560, height: 640 });
  const devFit = (await settle(dev, "page")) && await fit(dev);
  const log = () => dev.evaluate(() => document.getElementById("daemon-log").getBoundingClientRect().height);
  const emptyLog = await log();
  check("Developer mode grows the window by the Developer card",
    devFit && plainFit && devFit.asked === devFit.page && devFit.page > plainFit.page + emptyLog, JSON.stringify({ devFit, plainFit, emptyLog }));

  // A long log scrolls inside the box; neither the box nor the window grows.
  await dev.evaluate(() => {
    document.getElementById("daemon-log").textContent = Array.from({ length: 200 }, (_, i) => `line ${i}`).join("\n");
  });
  const fullLog = await log();
  const scrolls = await dev.evaluate(() => {
    const el = document.getElementById("daemon-log");
    return el.scrollHeight > el.clientHeight && getComputedStyle(el).overflowY === "auto";
  });
  const devAfter = await fit(dev);
  check("the log is a fixed height that scrolls inside, however much is logged",
    emptyLog === fullLog && scrolls && devAfter.asked === devFit.asked, JSON.stringify({ emptyLog, fullLog, scrolls, devAfter }));
  await dev.close();

  const opened = (await openSettings(plain)) && (await settle(plain, 640)) && await fit(plain);
  check("while a dialog is open the window is at least 640 px, room for the dialog to scroll in",
    opened && opened.asked === 640, JSON.stringify(opened));
  await plain.click("#btn-cancel-settings");
  const closed = (await settle(plain, "page")) && await fit(plain);
  check("and it shrinks back to the page when the dialog closes",
    closed && closed.asked === closed.page && closed.page === plainFit.page, JSON.stringify(closed));
  await plain.close();
}

check("no console errors", consoleErrors.length === 0, consoleErrors.join("\n        "));

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
