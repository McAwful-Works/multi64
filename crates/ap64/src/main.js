// AP64 page: load a seed, show what was checked, patch it; then play it on the cart.
// All ROM and cart work happens in the backend (src-tauri/src/); this only wires the page.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

let lastOutput = null;
// The last status, so turning developer details on can re-render it without waiting for
// the next event -- a stopped session never sends another.
let lastStatus = { state: "idle" };
let devDetails = false;

function show(el, on) {
  el.hidden = !on;
}

/*
 * Dialogs.
 *
 * Only one is ever open, so there is no stacking to reason about: the page tracks which, keeps
 * Tab inside it, closes it on Esc, and gives focus back to whatever opened it. `dialog-open` on
 * html and body is what stops the page behind from scrolling (styles.css).
 */
const DIALOGS = {
  patch: { panel: "patch-dialog", backdrop: "patch-backdrop", focus: "btn-open-patch" },
  advanced: { panel: "advanced-dialog", backdrop: "advanced-backdrop", focus: "btn-advanced" },
  games: { panel: "games-dialog", backdrop: "games-backdrop", focus: "btn-games" },
  helpPatch: { panel: "help-patch-dialog", backdrop: "help-patch-backdrop", focus: "btn-help-patch" },
  helpPlay: { panel: "help-play-dialog", backdrop: "help-play-backdrop", focus: "btn-help-play" },
  clientFix: { panel: "client-fix-dialog", backdrop: "client-fix-backdrop", focus: "btn-play-start" },
};
let openDialogName = null;
let dialogReturnFocus = null;

/*
 * The window is sized to the page.
 *
 * It cannot be resized by hand, so nothing here may rely on the user finding a scrollbar: when
 * what the page shows changes height -- developer details, a longer status, a dialog opening or
 * its arrow unfolding -- the window is asked for exactly that much room. A dialog is measured
 * too, since it can be taller than the page behind it.
 */
let fittedHeight = 0;

function fitWindow() {
  const page = Math.ceil(document.body.getBoundingClientRect().height);
  let height = page;
  if (openDialogName) {
    const panel = $(DIALOGS[openDialogName].panel).querySelector(".dialog-panel");
    // The dialog is centerd with a margin above and below it; give it both.
    height = Math.max(page, Math.ceil(panel.getBoundingClientRect().height) + 48);
  }
  if (height === fittedHeight) return;
  fittedHeight = height;
  invoke("fit_window_height", { height }).catch(() => {
    // Outside the app shell (the headless checks) there is no window to fit.
  });
}

const focusableIn = (root) =>
  [...root.querySelectorAll("button, [href], input, select, textarea, summary, [tabindex]")].filter(
    (el) => !el.disabled && el.tabIndex !== -1 && el.offsetParent !== null,
  );

function openDialog(name) {
  const d = DIALOGS[name];
  // Measured after it is on screen, below.
  dialogReturnFocus = document.activeElement;
  openDialogName = name;
  show($(d.backdrop), true);
  show($(d.panel), true);
  document.documentElement.classList.add("dialog-open");
  document.body.classList.add("dialog-open");
  const items = focusableIn($(d.panel));
  if (items.length) items[0].focus();
  fitWindow();
}

function closeDialog() {
  if (!openDialogName) return;
  const d = DIALOGS[openDialogName];
  show($(d.backdrop), false);
  show($(d.panel), false);
  document.documentElement.classList.remove("dialog-open");
  document.body.classList.remove("dialog-open");
  const back =
    dialogReturnFocus instanceof HTMLElement && dialogReturnFocus.isConnected && !dialogReturnFocus.disabled
      ? dialogReturnFocus
      : $(d.focus);
  openDialogName = null;
  dialogReturnFocus = null;
  if (back && !back.disabled) back.focus();
  fitWindow();
}

/** Tab and Shift+Tab stay inside the open dialog, so nothing behind it can take focus. */
function trapTab(e) {
  if (!openDialogName) return;
  const panel = $(DIALOGS[openDialogName].panel);
  const items = focusableIn(panel);
  if (!items.length) {
    e.preventDefault();
    return;
  }
  const [first, last] = [items[0], items[items.length - 1]];
  const inside = panel.contains(document.activeElement);
  if (e.shiftKey && (!inside || document.activeElement === first)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && (!inside || document.activeElement === last)) {
    e.preventDefault();
    first.focus();
  }
}

function setError(el, text) {
  el.textContent = text || "";
  show(el, Boolean(text));
}

function resetResults() {
  setError($("patch-error"), "");
  show($("pane-result"), false);
  show($("pane-seed"), true);
  show($("btn-patch"), true);
  show($("btn-output"), true);
  show($("btn-result-reveal"), false);
  $("btn-patch-close").textContent = "Cancel";
}

/** The card says which seed is loaded and how far it got; the dialog says the rest. */
function setSeedLine(text) {
  $("seed-line").textContent = text;
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

let lastReport = null;

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
  lastReport = report;
  renderSeedChecks();
  return report;
}

/** The Checks row: what it means, with the list itself behind the arrow. */
function renderSeedChecks() {
  const report = lastReport;
  const failed = report ? report.checks.filter((c) => !c.ok) : [];
  $("seed-checks").textContent = !report
    ? "—"
    : failed.length
      ? `${failed.length} of ${report.checks.length} failed`
      : "Ready for the agent";
}

async function loadSeed(path) {
  resetResults();
  setError($("load-error"), "");
  setSeedLine("Reading…");
  $("btn-open-patch").disabled = true;
  $("drop-zone").dataset.state = "busy";
  try {
    const r = await invoke("load_rom", { path });
    const d = r.detection;
    $("seed-file").textContent = `${r.fileName} (${(r.size / 1048576).toFixed(1)} MiB)`;
    const h = d.header;
    $("seed-header").textContent = h
      ? `${h.name || "(no name)"} · ${h.game_code} v${h.version} · boot code ${d.cic || "patched or unknown"} · ${d.byte_order}`
      : "unreadable";
    const report = renderChecks(d);
    if (!report) {
      $("seed-game").textContent = "Not a game AP64 knows";
      setError($("load-error"), `AP64 has no profile for this game. Supported: ${supportedNames()}.`);
    } else if (!r.chosen) {
      setError($("load-error"), "This seed cannot take the agent: see the failed checks.");
      $("seed-game").textContent = `${report.game} · ${report.randomizer}`;
    } else {
      $("seed-game").textContent = `${report.game} · ${report.randomizer}`;
    }
    const ready = Boolean(r.chosen);
    show($("patch-controls"), ready);
    $("btn-patch").disabled = !ready;
    if (ready) $("output").value = r.defaultOutput;
    setSeedLine(
      report
        ? `${r.fileName} — ${report.game}${ready ? "" : ", cannot take the agent"}`
        : `${r.fileName} — not a game AP64 knows`,
    );
    $("btn-open-patch").disabled = !report;
    $("btn-reveal").disabled = true;
    lastOutput = null;
    // Straight into the dialog: dropping a seed is the ask, and everything it needs is there.
    if (report) openDialog("patch");
    // The game this seed is for is the one Play will most likely want.
    if (ready && !playRunning) selectPlayGame(r.chosen);
  } catch (e) {
    setError($("load-error"), String(e));
    setSeedLine("No seed chosen");
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
    // The dialog becomes the receipt: same window, so nothing behind it moves.
    show($("pane-seed"), false);
    show($("pane-result"), true);
    show($("btn-patch"), false);
    show($("btn-output"), false);
    show($("btn-result-reveal"), true);
    $("btn-patch-close").textContent = "Close";
    $("btn-result-reveal").focus();
    setSeedLine(`${name} (${(r.size / 1048576).toFixed(1)} MiB) — ready for the console`);
    $("btn-reveal").disabled = false;
    $("btn-open-patch").disabled = true;
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
const DEV_KEY = "ap64.devDetails";
const LOG_LINES = 500;
let playRunning = false;
/** The game profiles AP64 was built with, as the Supported games window lists them. */
let profiles = [];
let games = [];

/**
 * What each of the three links says for each state it can be in.
 *
 * `null` means the state does not apply to that link, which is why they are spelled out rather
 * than shared: "not reachable" is a thing the Multi64 app can be and the client cannot.
 */
const LINK_TEXT = {
  bridge: {
    idle: "Not connected",
    // Not "looking for it": AP64 asked the app's process and it is not there.
    waiting: "Not running",
    nobridge: "Running, but its bridge is silent",
    nocart: "Running, but no cart connected",
    ok: "Connected",
    failed: "Not reachable",
  },
  console: {
    idle: "Not running",
    waiting: "Waiting for the ROM",
    ok: "Running",
    failed: "Not answering",
  },
  client: {
    idle: "Not connected",
    waiting: "Waiting for it to connect",
    ok: "Connected",
    failed: "Not connected",
  },
};

/*
 * What the status line says, in the words someone playing would use. The backend's `detail`
 * follows it after a dash and is written the same way; `detailDev` is the address, port or
 * error behind it, and only appears with Developer details on.
 */
const STATE_TEXT = {
  idle: "Not running",
  connecting: "Starting",
  // A session that was running and lost the cart: the ROM went away, not the session.
  "waiting-console": "Waiting for the console",
  "waiting-client": "Ready to play",
  playing: "Playing",
  stopped: "Stopped",
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

function readDev() {
  try {
    return localStorage.getItem(DEV_KEY) === "on";
  } catch {
    return false;
  }
}

function setDev(on) {
  devDetails = on;
  document.body.dataset.dev = on ? "on" : "off";
  try {
    localStorage.setItem(DEV_KEY, on ? "on" : "off");
  } catch {
    // Not remembered; it still applies for this run.
  }
  // Both of these say more in developer details, so they are rebuilt rather than left stale.
  renderStatus(lastStatus);
  renderSeedChecks();
}

// The log itself lives in its own window (log.html), and the lines are kept by the backend, so
// this page neither stores nor shows them.

const selectedGame = () => games.find((g) => g.id === $("play-game-select").value) || null;

/** For the one message that still has to name them inline, with no window to open. */
const supportedNames = () => profiles.map((p) => p.name).join(", ");

/**
 * The supported games, by their own full titles.
 *
 * No region or version: Archipelago patched the seed before AP64 ever saw it, so the release
 * is settled and naming it only invites the question of whether there is another to pick. It
 * is still what the cart's header is checked against, so it stays on the developer rows.
 */
function renderGames() {
  $("games-list").replaceChildren(
    ...profiles.map((p) => {
      const li = document.createElement("li");
      li.textContent = p.name;
      const sub = document.createElement("div");
      sub.className = "game-sub dev-only";
      sub.textContent = `${p.release} · ${p.randomizer}`;
      li.append(sub);
      return li;
    }),
  );
  if (!profiles.length) {
    const li = document.createElement("li");
    li.textContent = "No game profiles are installed.";
    $("games-list").append(li);
  }
}

function renderPlay() {
  const game = selectedGame();
  $("play-connector").textContent = game ? game.connector : "—";
  // The link rows name the game and the client, so a chosen game changes what they say.
  renderLinks(lastStatus);
  $("btn-play-start").disabled = playRunning || !game;
  $("btn-play-stop").disabled = !playRunning;
  $("play-game-select").disabled = playRunning;
  $("play-url").disabled = playRunning;
}

/** One row per link: the state drives both the words and the dot's color (app.css). */
function renderLinks(s) {
  for (const [key, id] of [
    ["bridge", "link-bridge"],
    ["console", "link-console"],
    ["client", "link-client"],
  ]) {
    const state = LINK_TEXT[key][s[key]] ? s[key] : "idle";
    $(id).dataset.state = state;
    // The console row names the game, and the client row the client, once one is chosen.
    // A link that is up says so in a word: the interesting rows are the ones that are not,
    // and those say what to do about it. Which client to open is worth naming before a session
    // as well as during one.
    const game = selectedGame();
    let text = LINK_TEXT[key][state];
    if (game && key === "client") {
      if (state === "idle") text = `${game.client} — not connected`;
      if (state === "waiting") text = `Open ${game.client} to connect`;
      // Once it has connected, the fix is behind it.
      if (state === "ok") clientHint = "";
      if (clientHint && (state === "idle" || state === "waiting")) text = clientHint;
    }
    $(id).textContent = text;
  }
}

function renderStatus(s) {
  lastStatus = s;
  renderLinks(s);
  const state = s.state || "idle";
  // What the backend says, not what the wording implies. A session waiting for a console to
  // come back is still a session, and Start and Stop belong to whoever pressed them.
  playRunning = !!s.running;
  const el = $("play-status");
  el.dataset.state = state;
  // detail says what happened; detailDev says which address or value it was read from, which
  // is an answer to a question only someone working on AP64 asks.
  const dev = devDetails && s.detailDev ? ` (${s.detailDev})` : "";
  // When the detail is already a whole sentence about the console, it is the whole line:
  // "Waiting for the console — waiting for the ROM on the console" says one thing twice.
  el.textContent =
    state === "waiting-console" && s.detail
      ? s.detail.charAt(0).toUpperCase() + s.detail.slice(1) + dev
      : STATE_TEXT[state] + (s.detail ? ` — ${s.detail}${dev}` : "");
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
    clientHint = "";
    renderPlay();
  }
}

/** A message from the backend as a sentence: they are written without the full stop. */
const sentence = (text) => (text && !/[.!?]$/.test(text) ? `${text}.` : text || "");

/**
 * What the Client row says in place of its usual words, until the client connects: after a fix,
 * the one thing left to do before it can.
 */
let clientHint = "";

async function beginSession() {
  const url = $("play-url").value.trim();
  saveUrl(url);
  try {
    await invoke("play_start", { game: $("play-game-select").value, url });
  } catch (e) {
    setError($("play-error"), String(e));
  }
}

/**
 * Start. For a game whose Archipelago client has to be changed before it can reach AP64
 * (`play_client_setup`), that is settled first: a client needing the fix gets one dialog, and one
 * that is not installed stops here and says so. Every other game answers null and just starts.
 */
async function startPlay() {
  setError($("play-error"), "");
  const game = selectedGame();
  if (!game) return;
  let setup = null;
  try {
    setup = await invoke("play_client_setup", { game: game.id });
  } catch {
    setup = null;
  }
  if (setup?.state === "missing") {
    setError($("play-error"), sentence(setup.message));
    return;
  }
  if (setup?.state === "needed") {
    $("client-fix-title").textContent = `${game.client} needs a one-time fix`;
    $("client-fix-text").textContent = sentence(setup.message);
    $("client-fix-path").textContent = setup.path || "";
    openDialog("clientFix");
    return; // The dialog's buttons carry on from here.
  }
  // A version AP64 does not recognize may still connect: say so, and start anyway.
  if (setup?.state === "unknown") setError($("play-error"), sentence(setup.message));
  await beginSession();
}

async function fixAndStart() {
  const game = selectedGame();
  if (!game) return closeDialog();
  $("btn-client-fix-go").disabled = true;
  try {
    await invoke("play_client_fix", { game: game.id });
    closeDialog();
    clientHint = `Restart the Archipelago Launcher, then open ${game.client}`;
    renderLinks(lastStatus);
    await beginSession();
  } catch (e) {
    closeDialog();
    setError($("play-error"), String(e));
  } finally {
    $("btn-client-fix-go").disabled = false;
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
  $("play-url").value = readUrl((await invoke("play_default_url")) || "");
  $("play-game-select").addEventListener("change", () => {
    clientHint = "";
    renderPlay();
  });
  $("btn-client-fix-go").addEventListener("click", fixAndStart);
  $("btn-client-fix-cancel").addEventListener("click", closeDialog);
  $("client-fix-backdrop").addEventListener("click", closeDialog);
  $("btn-play-start").addEventListener("click", startPlay);
  $("btn-play-stop").addEventListener("click", () => invoke("play_stop"));
  renderStatus((await invoke("play_status")) || { state: "idle" });
}

async function init() {
  // Covers everything: a wrapped line, a details arrow, rows appearing with developer details.
  new ResizeObserver(fitWindow).observe(document.body);
  for (const d of Object.values(DIALOGS)) {
    new ResizeObserver(fitWindow).observe($(d.panel).querySelector(".dialog-panel"));
  }
  const dev = $("dev-mode");
  dev.checked = readDev();
  setDev(dev.checked);
  dev.addEventListener("change", () => setDev(dev.checked));
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
  const reveal = () => {
    if (lastOutput) invoke("reveal", { path: lastOutput });
  };
  $("btn-reveal").addEventListener("click", reveal);
  $("btn-result-reveal").addEventListener("click", reveal);
  $("btn-open-patch").addEventListener("click", () => openDialog("patch"));
  $("btn-patch-close").addEventListener("click", closeDialog);
  $("patch-backdrop").addEventListener("click", closeDialog);
  $("btn-log").addEventListener("click", () => {
    invoke("open_log_window").catch((e) => setError($("play-error"), String(e)));
  });
  $("btn-advanced").addEventListener("click", () => openDialog("advanced"));
  $("btn-advanced-close").addEventListener("click", closeDialog);
  $("advanced-backdrop").addEventListener("click", closeDialog);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeDialog();
    else if (e.key === "Tab") trapTab(e);
  });
  // One info button per card: the card says the one thing to do, the window says the rest.
  for (const [name, btn] of [
    ["helpPatch", "btn-help-patch"],
    ["helpPlay", "btn-help-play"],
  ]) {
    $(btn).addEventListener("click", () => openDialog(name));
    $(`${btn}-close`).addEventListener("click", closeDialog);
    $(DIALOGS[name].backdrop).addEventListener("click", closeDialog);
  }
  $("btn-games").addEventListener("click", () => openDialog("games"));
  $("btn-games-close").addEventListener("click", closeDialog);
  $("games-backdrop").addEventListener("click", closeDialog);
  initPlay();
  try {
    profiles = (await invoke("profiles")) || [];
  } catch {
    profiles = [];
  }
  renderGames();
}

init();
