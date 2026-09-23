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
    profiles: () => answer([
      { id: "g1", name: "Game One", release: "US 1.0", randomizer: "Archipelago" },
      { id: "g2", name: "Game Two: The Subtitle", release: "EU 1.1", randomizer: "Game Two Randomizer" },
    ]),
    load_rom: () => answer(sc.load),
    patch_rom: () => answer(sc.patch),
    pick_rom: () => answer(sc.pick ?? null),
    pick_output: () => answer(null),
    play_default_url: () => answer("ws://127.0.0.1:38765/ws"),
    play_status: () => answer({ state: "idle", detail: "", port: null, requests: 0, reconnects: 0, stalls: 0, handled: 0 }),
    play_games: () => answer([
      { id: "g1", name: "Game One", connector: "Generic (BizHawk Client games)", client: "BizHawk Client" },
      { id: "g2", name: "Game Two: The Subtitle", connector: "Game Two connector", client: "Game Two Client" },
    ]),
    play_start: () => answer(sc.start ?? null),
    play_stop: () => answer(null),
    play_log: () => answer(sc.log ?? []),
    open_log_window: () => answer(sc.openLog ?? null),
    fit_window_height: () => answer(null),
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
  await until(p, () => document.querySelectorAll("#games-list li").length > 0);
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "play_status"));
  return p;
}

/** What the OS does when a file lands on the window: Tauri emits drag-drop with its paths. */
const drop = (p, paths) =>
  p.evaluate((ps) => window.__TAURI_LISTENERS__["tauri://drag-drop"]({ payload: { paths: ps, position: { x: 10, y: 10 } } }), paths);

const visible = (p, id) => p.evaluate((i) => document.getElementById(i)?.hidden === false, id);
const text = (p, id) => p.evaluate((i) => document.getElementById(i).textContent, id);
/** Shown to whoever is looking: `dev-only` rows are display:none until the switch is on. */
const onScreen = (p, id) =>
  p.evaluate((i) => document.getElementById(i)?.offsetParent != null, id);
const cardHeights = (p) =>
  p.evaluate(() => [...document.querySelectorAll(".card")].map((c) => Math.round(c.getBoundingClientRect().height)));
const setDev = async (p, on) => {
  await p.evaluate((v) => {
    const d = document.getElementById("dev-mode");
    d.checked = v;
    d.dispatchEvent(new Event("change"));
  }, on);
};

{
  const p = await openScenario({ load: loadOk, patch: { output: "C:\\seeds\\seed-agent.z64", size: 12587268, sha1: "66B5", summary: ["Frame hook: jal", "Agent: 4356 bytes"] } });
  // The supported games are a window, not a sentence under the drop zone: the list grows
  // with every profile added, and the card may not.
  check("the games are not listed on the card", !(await visible(p, "games-dialog")));
  await p.click("#btn-games");
  await until(p, () => !document.getElementById("games-dialog").hidden);
  // innerText, not textContent: the developer half of each row is in the DOM either way, and
  // what is being checked here is what a player actually sees.
  const gameRows = () => p.evaluate(() => [...document.querySelectorAll("#games-list li")].map((li) => li.innerText));
  check("the window lists every supported game", (await gameRows()).length === 2, JSON.stringify(await gameRows()));
  check("by its own full title", (await gameRows())[1].startsWith("Game Two: The Subtitle"), JSON.stringify(await gameRows()));
  // Archipelago patched the seed, so the release was settled long before AP64 saw it.
  check("with no release in front of a player", !(await gameRows()).join(" ").includes("US 1.0"), JSON.stringify(await gameRows()));
  await setDev(p, true);
  check("and the release behind developer details", (await gameRows())[0].includes("US 1.0") && (await gameRows())[0].includes("Archipelago"), JSON.stringify(await gameRows()));
  await setDev(p, false);
  await p.click("#btn-games-close");
  check("the window closes", !(await visible(p, "games-dialog")));
  // Each card says the one thing to do; the rest is behind its info button, so the cards do
  // not carry a paragraph each and a player has somewhere to go when one line is not enough.
  for (const [card, btn, panel, want] of [
    ["patch", "btn-help-patch", "help-patch-dialog", "Generate and patch your seed with Archipelago"],
    ["play", "btn-help-play", "help-play-dialog", "Open the game's client from the Archipelago Launcher"],
  ]) {
    const there = await onScreen(p, btn);
    check(`the ${card} card has an info button`, there);
    check(`and its instructions are not on the card`, !(await visible(p, panel)));
    // Without it there is nothing to click, and a click that waits 30s for an element that
    // will never appear reports nothing about the checks after it.
    if (!there) continue;
    await p.click(`#${btn}`);
    await until(p, (id) => !document.getElementById(id).hidden, panel);
    const body = await p.evaluate((id) => document.getElementById(id)?.innerText ?? "", panel);
    check(`the ${card} info window opens with the steps`, body.includes(want), want);
    await p.click(`#${btn}-close`);
    check(`the ${card} info window closes`, !(await visible(p, panel)));
  }
  // A player reads these, so they are not behind the developer switch.
  check("the info buttons are for everyone", await p.evaluate(() => {
    const b = document.getElementById("btn-help-play");
    return b != null && b.closest(".dev-only") === null;
  }));
  const idle = await cardHeights(p);
  await drop(p, ["C:\\seeds\\seed.z64", "C:\\other.z64"]);
  await until(p, () => !document.getElementById("patch-dialog").hidden);
  const loads = await callsOf(p, "load_rom");
  check("a drop loads the first file dropped", loads.length === 1 && loads[0].args.path === "C:\\seeds\\seed.z64", JSON.stringify(loads));
  check("a drop opens the dialog rather than growing the card", (await cardHeights(p)).join() === idle.join(), `${idle} then ${await cardHeights(p)}`);
  check("the card names the seed and its game", (await text(p, "seed-line")).includes("seed.z64") && (await text(p, "seed-line")).includes("Game One"));
  const marks = await p.evaluate(() => [...document.querySelectorAll(".check-item")].map((li) => li.dataset.ok));
  check("every check is listed", marks.join() === "true,true,true", marks.join());
  check("a passing seed offers to patch", await visible(p, "patch-controls"));
  check("the output defaults beside the seed", (await p.inputValue("#output")) === "C:\\seeds\\seed-agent.z64");
  check("the game is named, without its release", (await text(p, "seed-game")) === "Game One · Archipelago", await text(p, "seed-game"));
  check("passing checks read as ready", (await text(p, "seed-checks")) === "Ready for the agent" && !(await visible(p, "failed-checks")));
  check("the detail starts folded", !(await p.evaluate(() => document.getElementById("checks-more").open)));
  check("the detail is there without Developer details", await p.evaluate(() => document.querySelector("#checks-more summary").offsetParent !== null));
  check("the header line is inside it", (await text(p, "seed-header")).includes("NG1E") && (await text(p, "seed-header")).includes("z64 (big-endian)"));
  await p.click("#btn-patch");
  await until(p, () => !document.getElementById("pane-result").hidden);
  const patches = await callsOf(p, "patch_rom");
  check("Add agent patches to the chosen output", patches.length === 1 && patches[0].args.output === "C:\\seeds\\seed-agent.z64", JSON.stringify(patches));
  check("the result shows the summary", (await p.locator("#result-summary li").count()) === 2);
  check("the list of writes starts folded", !(await p.evaluate(() => document.querySelector("#pane-result details").open)));
  check("the result shows the SHA-1", (await text(p, "result-sha1")).includes("66B5"));
  check("the result names the file, with the path on hover", (await text(p, "result-path")).startsWith("seed-agent.z64") && (await p.getAttribute("#result-path", "title")) === "C:\\seeds\\seed-agent.z64");
  check("the seed pane gives way to the result", !(await visible(p, "pane-seed")));
  await p.click("#btn-patch-close");
  check("patching leaves the cards where they were", (await cardHeights(p)).join() === idle.join(), `${idle} then ${await cardHeights(p)}`);
  check("the card says the ROM is ready", (await text(p, "seed-line")).includes("ready for the console"));
  check("Show in folder is offered on the card", !(await p.evaluate(() => document.getElementById("btn-reveal").disabled)));
  check("a loaded seed picks its game for Play", (await p.inputValue("#play-game-select")) === "g1");
  check("Play names the client to open", (await text(p, "link-client")).includes("BizHawk Client"));
  check("Start is enabled once a game is chosen", !(await p.evaluate(() => document.getElementById("btn-play-start").disabled)));
  check("Play asks for no ROM file", (await p.locator("#play-rom, #btn-play-rom").count()) === 0);
  await p.close();
}

{
  const p = await openScenario({});
  check("the daemon URL defaults to the Multi64 app's", (await p.inputValue("#play-url")) === "ws://127.0.0.1:38765/ws");
  const options = await p.evaluate(() => [...document.getElementById("play-game-select").options].map((o) => o.value));
  check("the game list offers every game", options.join() === ",g1,g2", options.join());
  check("Start is disabled until a game is chosen", await p.evaluate(() => document.getElementById("btn-play-start").disabled));
  const before = await cardHeights(p);
  await p.selectOption("#play-game-select", "g2");
  check("choosing a game names the client to open", (await text(p, "link-client")).includes("Game Two Client"));
  check("choosing a game does not move the card", (await cardHeights(p)).join() === before.join(), `${before} then ${await cardHeights(p)}`);
  check("and enables Start", !(await p.evaluate(() => document.getElementById("btn-play-start").disabled)));
  await p.close();
}

{
  const p = await openScenario({});
  await p.selectOption("#play-game-select", "g1");
  await setDev(p, true);
  await p.click("#btn-advanced");
  await p.fill("#play-url", "ws://127.0.0.1:38766/ws");
  await p.click("#btn-advanced-close");
  await p.click("#btn-play-start");
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "play_start"));
  const starts = await callsOf(p, "play_start");
  check("Start sends the game and the daemon URL, no ROM", starts.length === 1 && starts[0].args.game === "g1" && !("romPath" in starts[0].args) && starts[0].args.url === "ws://127.0.0.1:38766/ws", JSON.stringify(starts));
  const emit = (name, payload) => p.evaluate(([n, pl]) => window.__TAURI_LISTENERS__[n]({ payload: pl }), [name, payload]);
  // Every status from a live session carries running: true, and Start and Stop follow that
  // and nothing else: whether a session exists is not something to read off its wording.
  const live = { running: true, port: 43055, requests: 0, reconnects: 0, stalls: 0, handled: 0 };
  await emit("play://status", { ...live, state: "waiting-client", detail: "open BizHawk Client from the Archipelago Launcher", bridge: "ok", console: "ok", client: "waiting" });
  check("waiting shows what to do next", (await text(p, "play-status")).includes("open BizHawk Client"));
  // Each link is tracked on its own: one line can say what is happening, but not which part is.
  const links = () => p.evaluate(() => ["link-bridge", "link-console", "link-client"].map((i) => {
    const el = document.getElementById(i);
    return `${el.dataset.state}: ${el.textContent}`;
  }));
  check("waiting has the cart up and the client not", (await links()).join(" | ") === "ok: Connected | ok: Running | waiting: Open BizHawk Client to connect", (await links()).join(" | "));
  check("a link that is up says so in one word", (await links()).filter((l) => l.startsWith("ok:")).every((l) => l.split(": ")[1].split(" ").length === 1), (await links()).join(" | "));
  const buttons = () => p.evaluate(() => ({
    start: document.getElementById("btn-play-start").disabled,
    stop: document.getElementById("btn-play-stop").disabled,
    game: document.getElementById("play-game-select").disabled,
    url: document.getElementById("play-url").disabled,
  }));
  check("running disables Start and enables Stop", await p.evaluate(() => document.getElementById("btn-play-start").disabled && !document.getElementById("btn-play-stop").disabled));
  check("running locks the game", (await buttons()).game);
  check("running locks the URL", (await buttons()).url);
  await emit("play://status", { ...live, state: "playing", detail: "BizHawk Client connected", requests: 120, reconnects: 1, stalls: 2, handled: 40, bridge: "ok", console: "ok", client: "ok" });
  check("playing shows the counters", (await text(p, "play-counters")).includes("120 cart round trips") && (await text(p, "play-counters")).includes("1 reconnects"));
  check("playing has all three up", (await links()).every((l) => l.startsWith("ok:")), (await links()).join(" | "));
  // A console reset while a session runs. The session stays up and keeps trying: the person
  // who pressed Start is the only one who decides it is over.
  await emit("play://status", { ...live, state: "waiting-console", detail: "the ROM stopped answering; load it again on the console", requests: 120, reconnects: 1, stalls: 2, handled: 40, bridge: "ok", console: "failed", client: "idle" });
  check("a ROM that stopped is reported, with Multi64 still up", (await links())[1] === "failed: Not answering" && (await links())[0] === "ok: Connected", (await links()).join(" | "));
  // Its own sentence: "Waiting for the console -- the ROM stopped answering" says it twice.
  check("and the status says so once, not twice", (await text(p, "play-status")).startsWith("The ROM stopped answering"), await text(p, "play-status"));
  check("a lost console keeps Stop and withholds Start", (await buttons()).start && !(await buttons()).stop, JSON.stringify(await buttons()));
  check("and leaves the game and URL locked", (await buttons()).game && (await buttons()).url);
  // The wrong game on the console is the same kind of thing: someone is about to fix it.
  await emit("play://status", { ...live, state: "waiting-console", detail: "the cart is running ZELDA [CZLE v0], not Game One US 1.0 [NG1E v0]", bridge: "ok", console: "failed", client: "idle" });
  check("a wrong game names the link that is down, not just the session", (await links())[1].startsWith("failed:") && (await links())[0].startsWith("ok:"), (await links()).join(" | "));
  check("and it is a session still waiting, not one that ended", (await buttons()).start && !(await buttons()).stop);
  await emit("play://status", { ...live, state: "playing", detail: "BizHawk Client connected", requests: 140, reconnects: 1, stalls: 2, handled: 41, bridge: "ok", console: "ok", client: "ok" });
  check("and it goes back up on its own when the ROM returns", (await links())[1] === "ok: Running", (await links()).join(" | "));
  await p.click("#btn-play-stop");
  await until(p, () => window.__TAURI_CALLS__.some((c) => c.cmd === "play_stop"));
  check("Stop asks the backend to stop", (await callsOf(p, "play_stop")).length === 1);
  // Only the backend reporting the session over gives Start back.
  await emit("play://status", { running: false, state: "stopped", detail: "", port: null, requests: 140, reconnects: 2, stalls: 2, handled: 41, bridge: "idle", console: "idle", client: "idle" });
  check("stopping hands Start back and takes Stop away", !(await buttons()).start && (await buttons()).stop, JSON.stringify(await buttons()));
  check("and unlocks the game and the URL", !(await buttons()).game && !(await buttons()).url);
  await p.close();
}

// What a player is shown, and what turning the switch on adds. The page is built off by
// default so a player never sees developer rows appear and then vanish.
{
  const p = await openScenario({});
  check("developer details start off", await p.evaluate(() => document.getElementById("dev-mode").checked === false));
  // The window cannot be resized by hand, so the page must ask for the room it needs.
  const fits = () => callsOf(p, "fit_window_height");
  const firstFit = (await fits()).at(-1)?.args.height;
  const pageHeight = await p.evaluate(() => Math.ceil(document.body.getBoundingClientRect().height));
  check("the window is fitted to the page", firstFit === pageHeight, `asked ${firstFit}, page ${pageHeight}`);
  await p.selectOption("#play-game-select", "g1");
  for (const id of ["play-connector", "play-counters", "btn-advanced"]) {
    check(`${id} is hidden from a player`, !(await onScreen(p, id)));
  }
  check("the session log is offered to everyone", await onScreen(p, "btn-log"));
  await setDev(p, true);
  for (const id of ["play-connector", "play-counters", "btn-advanced"]) {
    check(`${id} appears with developer details`, await onScreen(p, id));
  }
  const emit = (name, payload) => p.evaluate(([n, pl]) => window.__TAURI_LISTENERS__[n]({ payload: pl }), [name, payload]);
  const failure = { running: true, state: "waiting-console", detail: "the cart is running Game One without AP64's agent", detailDev: "the hook at 0x1601C is 0x00000000", port: null, requests: 0, reconnects: 0, stalls: 0, handled: 0 };
  await emit("play://status", failure);
  check("a failure shows its addresses with developer details", (await text(p, "play-status")).includes("0x1601C"));
  await setDev(p, false);
  check("and reads plainly without them", !(await text(p, "play-status")).includes("0x1601C") && (await text(p, "play-status")).includes("without AP64's agent"));
  const stored = await p.evaluate(() => localStorage.getItem("ap64.devDetails"));
  check("the choice is remembered", stored === "off", String(stored));
  await setDev(p, true);
  // The page is measured by a ResizeObserver, which fires after the layout it is watching.
  const grew = await until(
    p,
    (h) => window.__TAURI_CALLS__.filter((c) => c.cmd === "fit_window_height").some((c) => c.args.height > h),
    firstFit,
  );
  const taller = (await fits()).at(-1)?.args.height;
  check("developer rows are given more window, not a scrollbar", grew && taller > firstFit, `${firstFit} then ${taller}`);
  await setDev(p, false);
  await p.close();
}

// The log is a window of its own, kept by the backend: the page only asks for it.
{
  const p = await openScenario({});
  // Nothing to move the card with: the page neither listens for log lines nor holds any.
  check("the page keeps no log of its own", await p.evaluate(() => document.getElementById("play-log") === null));
  check("and does not listen for log lines", await p.evaluate(() => !("play://log" in window.__TAURI_LISTENERS__)));
  await p.click("#btn-log");
  check("the button asks for the log window", (await callsOf(p, "open_log_window")).length === 1);
  await p.click("#btn-log");
  check("asking again is the backend's to answer, not a second window here", (await callsOf(p, "open_log_window")).length === 2);
  await p.close();
}

// The log window itself: what the session logged before it opened, then what arrives after.
{
  const p = await browser.newPage({ viewport: { width: 620, height: 420 } });
  p.on("console", (m) => { if (m.type() === "error") consoleErrors.push(m.text()); });
  p.on("pageerror", (e) => consoleErrors.push(`uncaught: ${e.message}`));
  await p.addInitScript(installTauriStub, { log: ["cart agent answered: 8 MiB RDRAM, writable", "OoT Client connected"] });
  await p.goto(`${origin}/log.html`);
  await until(p, () => document.getElementById("log").textContent.includes("OoT Client connected"));
  check("the log window shows what it missed", (await text(p, "log")).includes("8 MiB RDRAM"));
  check("and counts the lines", (await text(p, "log-count")) === "2 lines");
  await p.evaluate(() => window.__TAURI_LISTENERS__["play://log"]({ payload: "Archipelago: Got Roast Chicken" }));
  check("and follows the session while open", (await text(p, "log")).includes("Roast Chicken") && (await text(p, "log-count")) === "3 lines");
  await p.close();
}

// A log that cannot be read must say so: an empty window reads as a quiet session, which is how
// a backlog that never arrived went unnoticed.
{
  const p = await browser.newPage({ viewport: { width: 620, height: 420 } });
  p.on("pageerror", (e) => consoleErrors.push(`uncaught: ${e.message}`));
  await p.addInitScript(installTauriStub, { log: { error: "play_log: command not found" } });
  await p.goto(`${origin}/log.html`);
  await until(p, () => document.getElementById("log").textContent.includes("could not read"));
  check("a log that cannot be read says so", (await text(p, "log")).includes("play_log: command not found"));
  await p.close();
}

{
  const p = await openScenario({ load: loadFailing });
  await drop(p, ["C:\\seeds\\bad.z64"]);
  await until(p, () => !document.getElementById("patch-dialog").hidden);
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
  // No window to open from inside an error, so this one still carries the names itself.
  check("an unknown game names what is supported", (await text(p, "load-error")).includes("Game One") && (await text(p, "load-error")).includes("Game Two: The Subtitle"), await text(p, "load-error"));
  check("an unknown game does not offer to patch", !(await visible(p, "patch-controls")));
  check("and does not open the dialog", !(await visible(p, "patch-dialog")));
  await p.close();
}

{
  const p = await openScenario({ load: { error: "not an N64 ROM (no z64, v64 or n64 header)" }, pick: "C:\\x.zip" });
  await p.click("#btn-browse");
  await until(p, () => !document.getElementById("load-error").hidden);
  check("Browse loads the picked file", (await callsOf(p, "load_rom"))[0]?.args.path === "C:\\x.zip");
  check("a load error is shown", (await text(p, "load-error")).includes("not an N64 ROM"));
  check("a load error leaves no seed on the card", (await text(p, "seed-line")) === "No seed chosen");
  await p.close();
}

{
  const p = await openScenario({ load: loadOk, patch: { error: "C:\\seeds\\seed-agent.z64: Access is denied." } });
  await drop(p, ["C:\\seeds\\seed.z64"]);
  await until(p, () => !document.getElementById("patch-controls").hidden);
  await p.click("#btn-patch");
  await until(p, () => !document.getElementById("patch-error").hidden);
  check("a write error is shown", (await text(p, "patch-error")).includes("Access is denied"));
  check("a write error shows no result", !(await visible(p, "pane-result")));
  check("the button is usable again", !(await p.evaluate(() => document.getElementById("btn-patch").disabled)));
  await p.close();
}

// The shared palette rule: colors live in styles.css :root blocks, never in the app sheet.
{
  const css = await readFile(join(FRONTEND_DIR, "app.css"), "utf8");
  const literals = css.replace(/\/\*[\s\S]*?\*\//g, "").match(/#[0-9a-fA-F]{3,8}\b|\brgba?\(\s*\d|\bhsla?\(/g) || [];
  check("app.css has no color literals", literals.length === 0, literals.join(", "));
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
