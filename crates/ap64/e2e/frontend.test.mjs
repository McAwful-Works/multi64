/**
 * Headless checks for the AP64 window.
 *
 * `src/index.html` is served as-is and `window.__TAURI__` is replaced with a stub that records
 * every `invoke` and answers from a scenario, so the page is judged by what it shows for the
 * backend's answers and what it sends back. No ROM is read: green here means the page wiring
 * holds, not that a seed patches (that is `crates/ap64-core`'s tests).
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
};

async function serveFrontend() {
  const server = createServer(async (req, res) => {
    const rel = normalize(decodeURIComponent(new URL(req.url, "http://x").pathname)).replace(/^([/\\])+/, "");
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
 * Stand in for the Tauri bridge. `scenario`:
 * - `load`: what `load_rom` resolves to, or `{ error }` to reject
 * - `patch`: what `patch_rom` resolves to, or `{ error }` to reject
 * - `pick`: what `pick_rom` resolves to
 */
function installTauriStub(scenario) {
  const sc = scenario || {};
  const calls = [];
  window.__TAURI_CALLS__ = calls;
  window.__TAURI_LISTENERS__ = {};
  const answer = (v) => (v && v.error ? Promise.reject(v.error) : Promise.resolve(v ?? null));
  const handlers = {
    profiles: () => answer([{ id: "g1", name: "Game One", release: "US 1.0" }]),
    load_rom: () => answer(sc.load),
    patch_rom: () => answer(sc.patch),
    pick_rom: () => answer(sc.pick ?? null),
    pick_output: () => answer(null),
    play_default_url: () => answer("ws://127.0.0.1:38765/ws"),
    play_status: () => answer({ state: "idle", detail: "", port: null, requests: 0, reconnects: 0, stalls: 0, handled: 0 }),
    play_games: () => answer([
      { id: "g1", name: "Game One (US 1.0)", connector: "Generic (BizHawk Client games)", client: "BizHawk Client" },
      { id: "g2", name: "Game Two (US 1.0)", connector: "Game Two connector", client: "Game Two Client" },
    ]),
    play_start: () => answer(sc.start ?? null),
    play_stop: () => answer(null),
  };
  window.__TAURI__ = {
    core: {
      invoke: (cmd, args) => {
        calls.push({ cmd, args: args ?? null });
        return handlers[cmd] ? handlers[cmd](args) : Promise.resolve(null);
      },
    },
    event: {
      listen: (name, cb) => {
        window.__TAURI_LISTENERS__[name] = cb;
        return Promise.resolve(() => {});
      },
    },
  };
}

const passing = (label) => ({ label, ok: true, detail: `${label} fine`, hint: "" });
const loadOk = {
  path: "C:\\seeds\\seed.z64",
  fileName: "seed.z64",
  size: 12582912,
  detection: {
    byte_order: "z64 (big-endian)",
    header: { name: "GAME ONE", game_code: "NG1E", version: 0 },
    cic: null,
    candidates: [
      {
        profile_id: "g1",
        game: "Game One",
        release: "US 1.0",
        randomizer: "Archipelago",
        checks: [passing("Game"), passing("Frame hook site"), passing("Room for the agent")],
      },
    ],
  },
  chosen: "g1",
  defaultOutput: "C:\\seeds\\seed-agent.z64",
};
const loadFailing = structuredClone(loadOk);
loadFailing.chosen = null;
loadFailing.detection.candidates[0].checks[1] = {
  label: "Frame hook site",
  ok: false,
  detail: "0x1601C holds 0x00000000",
  hint: "Generate with the option off.",
};
const loadUnknown = structuredClone(loadOk);
loadUnknown.chosen = null;
loadUnknown.detection.candidates = [];


const results = [];
const check = (name, pass, detail = "") => results.push({ name, pass, detail });

const { server, origin } = await serveFrontend();
const browser = await chromium.launch({ executablePath: process.env.AP64_E2E_CHROMIUM || undefined });
const consoleErrors = [];

const until = (p, fn, arg, timeout = 5000) =>
  p.waitForFunction(fn, arg, { timeout }).then(() => true, () => false);
const callsOf = (p, name) => p.evaluate((n) => window.__TAURI_CALLS__.filter((c) => c.cmd === n), name);

async function openScenario(scenario) {
  const p = await browser.newPage({ viewport: { width: 700, height: 900 } });
  p.on("console", (m) => { if (m.type() === "error") consoleErrors.push(m.text()); });
  p.on("pageerror", (e) => consoleErrors.push(`uncaught: ${e.message}`));
  await p.addInitScript(installTauriStub, scenario);
  await p.goto(`${origin}/index.html`);
  await until(p, () => document.getElementById("supported").textContent.length > 0);
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "play_status"));
  return p;
}

/** What the OS does when a file lands on the window: Tauri emits drag-drop with its paths. */
const drop = (p, paths) =>
  p.evaluate((ps) => window.__TAURI_LISTENERS__["tauri://drag-drop"]({ payload: { paths: ps, position: { x: 10, y: 10 } } }), paths);

const visible = (p, id) => p.evaluate((i) => !document.getElementById(i).hidden, id);
const text = (p, id) => p.evaluate((i) => document.getElementById(i).textContent, id);

{
  const p = await openScenario({ load: loadOk, patch: { output: "C:\\seeds\\seed-agent.z64", size: 12587268, sha1: "66B5", summary: ["Frame hook: jal", "Agent: 4356 bytes"] } });
  check("lists the supported games", (await text(p, "supported")).includes("Game One (US 1.0)"));
  await drop(p, ["C:\\seeds\\seed.z64", "C:\\other.z64"]);
  await until(p, () => !document.getElementById("seed").hidden);
  const loads = await callsOf(p, "load_rom");
  check("a drop loads the first file dropped", loads.length === 1 && loads[0].args.path === "C:\\seeds\\seed.z64", JSON.stringify(loads));
  const marks = await p.evaluate(() => [...document.querySelectorAll(".check-item")].map((li) => li.dataset.ok));
  check("every check is listed", marks.join() === "true,true,true", marks.join());
  check("a passing seed offers to patch", await visible(p, "patch-controls"));
  check("the output defaults beside the seed", (await p.inputValue("#output")) === "C:\\seeds\\seed-agent.z64");
  check("the game is named", (await text(p, "seed-game")).startsWith("Game One (US 1.0)"));
  check("passing checks are one status row", (await text(p, "seed-checks")) === "All 3 passed" && !(await visible(p, "failed-checks")));
  check("the full checklist starts folded", !(await p.evaluate(() => document.getElementById("checks-more").open)));
  await p.click("#btn-patch");
  await until(p, () => !document.getElementById("result").hidden);
  const patches = await callsOf(p, "patch_rom");
  check("Add agent patches to the chosen output", patches.length === 1 && patches[0].args.output === "C:\\seeds\\seed-agent.z64", JSON.stringify(patches));
  check("the result shows the summary", (await p.locator("#result-summary li").count()) === 2);
  check("the list of writes starts folded", !(await p.evaluate(() => document.querySelector("#result details").open)));
  check("the result shows the SHA-1", (await text(p, "result-sha1")).includes("66B5"));
  check("the result names the file, with the path on hover", (await text(p, "result-path")).startsWith("seed-agent.z64") && (await p.getAttribute("#result-path", "title")) === "C:\\seeds\\seed-agent.z64");  check("the patch controls fold away once done", !(await visible(p, "patch-controls")));
  check("the file line names its byte order", (await text(p, "seed-file")).includes("z64 (big-endian)"));
  check("a loaded seed picks its game for Play", (await p.inputValue("#play-game-select")) === "g1");
  check("Play names the client to open", (await text(p, "play-client")).startsWith("BizHawk Client"));
  check("Start is enabled once a game is chosen", !(await p.evaluate(() => document.getElementById("btn-play-start").disabled)));
  check("Play asks for no ROM file", (await p.locator("#play-rom, #btn-play-rom").count()) === 0);
  await p.close();
}

{
  const p = await openScenario({});
  check("the daemon URL defaults to the Multi64 app's", (await p.inputValue("#play-url")) === "ws://127.0.0.1:38765/ws");
  const options = await p.evaluate(() => [...document.getElementById("play-game-select").options].map((o) => o.value));
  check("the game list offers every game", options.join() === ",g1,g2", options.join());
  check("no game chosen shows no connector", !(await visible(p, "play-facts")));
  check("Start is disabled until a game is chosen", await p.evaluate(() => document.getElementById("btn-play-start").disabled));
  await p.selectOption("#play-game-select", "g2");
  check("choosing a game names its connector and client", (await text(p, "play-connector")) === "Game Two connector" && (await text(p, "play-client")).startsWith("Game Two Client"));
  check("and enables Start", !(await p.evaluate(() => document.getElementById("btn-play-start").disabled)));
  await p.close();
}

{
  const p = await openScenario({});
  await p.selectOption("#play-game-select", "g1");
  await p.click("#play-advanced summary");
  await p.fill("#play-url", "ws://127.0.0.1:38766/ws");
  await p.click("#btn-play-start");
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "play_start"));
  const starts = await callsOf(p, "play_start");
  check("Start sends the game and the daemon URL, no ROM", starts.length === 1 && starts[0].args.game === "g1" && !("romPath" in starts[0].args) && starts[0].args.url === "ws://127.0.0.1:38766/ws", JSON.stringify(starts));
  const emit = (name, payload) => p.evaluate(([n, pl]) => window.__TAURI_LISTENERS__[n]({ payload: pl }), [name, payload]);
  await emit("play://status", { state: "waiting-client", detail: "open BizHawk Client from the Archipelago Launcher", port: 43055, requests: 0, reconnects: 0, stalls: 0, handled: 0 });
  check("waiting shows what to do next", (await text(p, "play-status")).includes("open BizHawk Client"));
  check("running disables Start and enables Stop", await p.evaluate(() => document.getElementById("btn-play-start").disabled && !document.getElementById("btn-play-stop").disabled));
  check("running locks the game", await p.evaluate(() => document.getElementById("play-game-select").disabled));
  check("running locks the URL", await p.evaluate(() => document.getElementById("play-url").disabled));
  await emit("play://status", { state: "playing", detail: "BizHawk Client connected", port: 43055, requests: 120, reconnects: 1, stalls: 2, handled: 40 });
  check("playing shows the counters", (await text(p, "play-counters")).includes("120 cart round trips") && (await text(p, "play-counters")).includes("1 reconnects"));
  await emit("play://log", "Archipelago: Got Roast Chicken");
  check("log lines are shown", (await visible(p, "play-log")) && (await text(p, "play-log")).includes("Got Roast Chicken"));
  await p.click("#btn-play-stop");
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "play_stop"));
  check("Stop asks the backend to stop", (await callsOf(p, "play_stop")).length === 1);
  await emit("play://status", { state: "failed", detail: "the cart is running ZELDA [CZLE v0], not Game One US 1.0 [NG1E v0]", port: null, requests: 3, reconnects: 0, stalls: 0, handled: 0 });
  check("a cart running another game is reported, and Start comes back", (await text(p, "play-status")).includes("the cart is running ZELDA") && !(await p.evaluate(() => document.getElementById("btn-play-start").disabled)));
  await p.close();
}

{
  const p = await openScenario({ load: loadFailing });
  await drop(p, ["C:\\seeds\\bad.z64"]);
  await until(p, () => !document.getElementById("seed").hidden);
  check("a failing seed does not offer to patch", !(await visible(p, "patch-controls")));
  check("a failing seed says why", (await visible(p, "load-error")) && (await text(p, "load-error")).includes("failed checks"));
  const failed = await p.evaluate(() => document.querySelector('#failed-checks .check-hint')?.textContent);
  check("a failing seed counts its failures", (await text(p, "seed-checks")) === "1 of 3 failed");
  check("only the failures are listed open", (await p.locator("#failed-checks li").count()) === 1);
  check("a failed check shows its hint", failed === "Generate with the option off.", String(failed));
  await p.close();
}

{
  const p = await openScenario({ load: loadUnknown });
  await drop(p, ["C:\\seeds\\other.z64"]);
  await until(p, () => !document.getElementById("load-error").hidden);
  check("an unknown game names what is supported", (await text(p, "load-error")).includes("Game One"));
  check("an unknown game does not offer to patch", !(await visible(p, "patch-controls")));
  await p.close();
}

{
  const p = await openScenario({ load: { error: "not an N64 ROM (no z64, v64 or n64 header)" }, pick: "C:\\x.zip" });
  await p.click("#btn-browse");
  await until(p, () => !document.getElementById("load-error").hidden);
  check("Browse loads the picked file", (await callsOf(p, "load_rom"))[0]?.args.path === "C:\\x.zip");
  check("a load error is shown", (await text(p, "load-error")).includes("not an N64 ROM"));
  check("a load error hides the seed", !(await visible(p, "seed")));
  await p.close();
}

{
  const p = await openScenario({ load: loadOk, patch: { error: "C:\\seeds\\seed-agent.z64: Access is denied." } });
  await drop(p, ["C:\\seeds\\seed.z64"]);
  await until(p, () => !document.getElementById("patch-controls").hidden);
  await p.click("#btn-patch");
  await until(p, () => !document.getElementById("patch-error").hidden);
  check("a write error is shown", (await text(p, "patch-error")).includes("Access is denied"));
  check("a write error shows no result", !(await visible(p, "result")));
  check("the button is usable again", !(await p.evaluate(() => document.getElementById("btn-patch").disabled)));
  await p.close();
}

// The shared palette rule: colours live in styles.css :root blocks, never in the app sheet.
{
  const css = await readFile(join(FRONTEND_DIR, "app.css"), "utf8");
  const literals = css.replace(/\/\*[\s\S]*?\*\//g, "").match(/#[0-9a-fA-F]{3,8}\b|\brgba?\(\s*\d|\bhsla?\(/g) || [];
  check("app.css has no colour literals", literals.length === 0, literals.join(", "));
}

check("no console errors", consoleErrors.length === 0, consoleErrors.join("\n        "));

await browser.close();
server.close();

let failed = 0;
for (const r of results) {
  if (!r.pass) failed++;
  console.log(`${r.pass ? "ok  " : "FAIL"}  ${r.name}${r.pass ? "" : `\n        ${r.detail}`}`);
}
console.log(`\n${results.length - failed}/${results.length} checks passed`);
process.exit(failed ? 1 : 0);
