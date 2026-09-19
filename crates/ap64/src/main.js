// AP64 page: load a seed, show what was checked, patch it; then play it on the cart.
// All ROM and cart work happens in the backend (src-tauri/src/); this only wires the page.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

let lastOutput = null;

function show(el, on) {
  el.hidden = !on;
}

function setError(el, text) {
  el.textContent = text || "";
  show(el, Boolean(text));
}

function resetResults() {
  setError($("patch-error"), "");
  show($("result"), false);
}

function checkItem(c, withHint) {
  const li = document.createElement("li");
  li.className = "check-item";
  li.dataset.ok = String(c.ok);
  const mark = document.createElement("span");
  mark.className = "check-mark";
  mark.setAttribute("aria-label", c.ok ? "passed" : "failed");
  mark.textContent = c.ok ? "✓" : "✗";
  const body = document.createElement("div");
  body.textContent = `${c.label}: `;
  const detail = document.createElement("span");
  detail.className = "check-detail";
  detail.textContent = c.detail;
  body.append(detail);
  if (withHint && c.hint) {
    const hint = document.createElement("div");
    hint.className = "check-hint";
    hint.textContent = c.hint;
    body.append(hint);
  }
  li.append(mark, body);
  return li;
}

/** One status row for the checks; failures listed open with their hints, the rest behind a toggle. */
function renderChecks(detection) {
  // With several candidate profiles, show the one that passes, else the first.
  const report =
    detection.candidates.find((r) => r.checks.every((c) => c.ok)) || detection.candidates[0];
  const failed = report ? report.checks.filter((c) => !c.ok) : [];
  $("checks").replaceChildren(...(report ? report.checks.map((c) => checkItem(c, false)) : []));
  $("failed-checks").replaceChildren(...failed.map((c) => checkItem(c, true)));
  show($("failed-checks"), failed.length > 0);
  show($("checks-more"), Boolean(report));
  $("seed-checks").textContent = !report
    ? "—"
    : failed.length
      ? `${failed.length} of ${report.checks.length} failed`
      : `All ${report.checks.length} passed`;
  return report;
}

async function loadSeed(path) {
  resetResults();
  setError($("load-error"), "");
  show($("seed"), false);
  $("drop-zone").dataset.state = "busy";
  try {
    const r = await invoke("load_rom", { path });
    const d = r.detection;
    $("seed-file").textContent = `${r.fileName} (${(r.size / 1048576).toFixed(1)} MiB, ${d.byte_order})`;
    const h = d.header;
    $("seed-header").textContent = h
      ? `${h.name || "(no name)"} · ${h.game_code} v${h.version} · boot code ${d.cic || "patched or unknown"}`
      : "unreadable";
    const report = renderChecks(d);
    if (!report) {
      $("seed-game").textContent = "Not a game AP64 knows";
      setError($("load-error"), "AP64 has no profile for this game. " + $("supported").textContent);
    } else {
      $("seed-game").textContent = `${report.game} (${report.release}) · ${report.randomizer}`;
    }
    show($("seed"), true);
    const ready = Boolean(r.chosen);
    show($("patch-controls"), ready);
    if (ready) $("output").value = r.defaultOutput;
    else if (report) setError($("load-error"), "This seed cannot take the agent: see the failed checks.");
    // The game this seed is for is the one Play will most likely want.
    if (ready && !playRunning) selectPlayGame(r.chosen);
  } catch (e) {
    setError($("load-error"), String(e));
  } finally {
    $("drop-zone").dataset.state = "idle";
  }
}

async function patch() {
  resetResults();
  const output = $("output").value.trim();
  if (!output) {
    setError($("patch-error"), "Choose where to save the ROM.");
    return;
  }
  $("btn-patch").disabled = true;
  try {
    const r = await invoke("patch_rom", { output });
    lastOutput = r.output;
    // The file name; the full path is on hover and behind Show in folder.
    const name = r.output.split(/[\\/]/).pop();
    $("result-path").textContent = `${name} (${(r.size / 1048576).toFixed(1)} MiB)`;
    $("result-path").title = r.output;
    const list = $("result-summary");
    list.replaceChildren(
      ...r.summary.map((line) => {
        const li = document.createElement("li");
        li.textContent = line;
        return li;
      }),
    );
    $("result-sha1").textContent = r.sha1;
    show($("result"), true);
    // Done with this seed: the controls fold away until another is loaded.
    show($("patch-controls"), false);
  } catch (e) {
    setError($("patch-error"), String(e));
  } finally {
    $("btn-patch").disabled = false;
  }
}

function setupDrops() {
  const zone = $("drop-zone");
  listen("tauri://drag-enter", () => (zone.dataset.state = "over"));
  listen("tauri://drag-over", () => (zone.dataset.state = "over"));
  listen("tauri://drag-leave", () => (zone.dataset.state = "idle"));
  listen("tauri://drag-drop", (event) => {
    zone.dataset.state = "idle";
    const paths = (event.payload && event.payload.paths) || [];
    if (paths.length) loadSeed(paths[0]);
  });
}

// --- Play ------------------------------------------------------------------

const URL_KEY = "ap64.daemonUrl";
const LOG_LINES = 500;
let playRunning = false;
let games = [];

const STATE_TEXT = {
  idle: "Not running",
  connecting: "Connecting to the cart",
  "waiting-client": "Waiting for the Archipelago client",
  playing: "Playing",
  stopped: "Stopped",
  failed: "Stopped with an error",
};

function readUrl(fallback) {
  try {
    return localStorage.getItem(URL_KEY) || fallback;
  } catch {
    return fallback;
  }
}

function saveUrl(value) {
  try {
    localStorage.setItem(URL_KEY, value);
  } catch {
    // Not remembered; the field still works for this run.
  }
}

function appendLog(line) {
  const log = $("play-log");
  const lines = log.textContent ? log.textContent.split("\n") : [];
  lines.push(line);
  log.textContent = lines.slice(-LOG_LINES).join("\n");
  log.scrollTop = log.scrollHeight;
  show(log, true);
}

const selectedGame = () => games.find((g) => g.id === $("play-game-select").value) || null;

function renderPlay() {
  const game = selectedGame();
  show($("play-facts"), Boolean(game));
  if (game) {
    $("play-connector").textContent = game.connector;
    $("play-client").textContent = `${game.client}, from the Archipelago Launcher`;
  }
  $("btn-play-start").disabled = playRunning || !game;
  $("btn-play-stop").disabled = !playRunning;
  $("play-game-select").disabled = playRunning;
  $("play-url").disabled = playRunning;
}

function renderStatus(s) {
  const state = s.state || "idle";
  playRunning = ["connecting", "waiting-client", "playing"].includes(state);
  const el = $("play-status");
  el.dataset.state = state;
  el.textContent = STATE_TEXT[state] + (s.detail ? ` — ${s.detail}` : "");
  $("play-counters").textContent =
    s.port != null
      ? `port ${s.port} · ${s.handled} client requests · ${s.requests} cart round trips · ` +
        `${s.stalls} stalls · ${s.reconnects} reconnects`
      : "—";
  renderPlay();
}

function selectPlayGame(id) {
  if (games.some((g) => g.id === id)) {
    $("play-game-select").value = id;
    renderPlay();
  }
}

async function startPlay() {
  setError($("play-error"), "");
  const url = $("play-url").value.trim();
  saveUrl(url);
  $("play-log").textContent = "";
  show($("play-log"), false);
  try {
    await invoke("play_start", { game: $("play-game-select").value, url });
  } catch (e) {
    setError($("play-error"), String(e));
  }
}

async function initPlay() {
  // The game list first: choosing a ROM selects its game, which needs the options there.
  games = (await invoke("play_games")) || [];
  for (const g of games) {
    const o = document.createElement("option");
    o.value = g.id;
    o.textContent = g.name;
    $("play-game-select").append(o);
  }
  listen("play://status", (event) => renderStatus(event.payload || {}));
  listen("play://log", (event) => appendLog(String(event.payload)));
  $("play-url").value = readUrl((await invoke("play_default_url")) || "");
  $("play-game-select").addEventListener("change", renderPlay);
  $("btn-play-start").addEventListener("click", startPlay);
  $("btn-play-stop").addEventListener("click", () => invoke("play_stop"));
  renderStatus((await invoke("play_status")) || { state: "idle" });
}

async function init() {
  setupDrops();
  $("btn-browse").addEventListener("click", async () => {
    const path = await invoke("pick_rom");
    if (path) loadSeed(path);
  });
  $("btn-output").addEventListener("click", async () => {
    const path = await invoke("pick_output", { suggested: $("output").value });
    if (path) $("output").value = path;
  });
  $("btn-patch").addEventListener("click", patch);
  $("btn-reveal").addEventListener("click", () => {
    if (lastOutput) invoke("reveal", { path: lastOutput });
  });
  initPlay();
  try {
    const profiles = await invoke("profiles");
    $("supported").textContent =
      "Supported: " + profiles.map((p) => `${p.name} (${p.release})`).join(", ") + ".";
  } catch (e) {
    $("supported").textContent = "";
  }
}

init();
