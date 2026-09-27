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
  notes: { panel: "notes-dialog", backdrop: "notes-backdrop", focus: "btn-play-start" },
  welcome: { panel: "welcome-dialog", backdrop: "welcome-backdrop", focus: "btn-browse" },
};

/*
 * The first time AP64 is opened, one window saying what has to be in place. Remembered once it
 * is closed, however it is closed; a browser that cannot remember shows it again, which is the
 * safe way to fail.
 */
const WELCOMED_KEY = "ap64.welcomed";

function welcomed() {
  try {
    return localStorage.getItem(WELCOMED_KEY) === "1";
  } catch {
    return false;
  }
}

function rememberWelcomed() {
  try {
    localStorage.setItem(WELCOMED_KEY, "1");
  } catch {
    // Shown again next time.
  }
}
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
  if (openDialogName === "welcome") rememberWelcomed();
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

/*
 * The flash cart the patch builds the agent for. Multi64's, when its daemon is running, since
 * that is the cart the ROM will be played through; otherwise the last one chosen here. A seed is
 * checked as that cart's build, so changing it checks the seed again.
 */
const CART_KEY = "ap64.cart";
let carts = [];
let loadedPath = null;

function readCart() {
  try {
    return localStorage.getItem(CART_KEY) || "sc64";
  } catch {
    return "sc64";
  }
}

function saveCart(id) {
  try {
    localStorage.setItem(CART_KEY, id);
  } catch {
    // Not remembered; Multi64's cart or the SummerCart64 next time.
  }
}

function selectedCart() {
  return $("patch-cart").value || "sc64";
}

function setCart(id) {
  if (!carts.some((c) => c.id === id)) return;
  $("patch-cart").value = id;
  const cart = carts.find((c) => c.id === id);
  show($("cart-note"), !cart.tested);
}

async function initCarts() {
  carts = (await invoke("carts")) || [];
  for (const c of carts) {
    const o = document.createElement("option");
    o.value = c.id;
    o.textContent = c.tested ? c.name : `${c.name} (experimental)`;
    $("patch-cart").append(o);
  }
  setCart(readCart());
  $("patch-cart").addEventListener("change", () => {
    setCart($("patch-cart").value);
    saveCart(selectedCart());
    if (loadedPath) loadSeed(loadedPath);
  });
  // Multi64's cart wins when it answers: it is the cart the ROM will be played through.
  try {
    const running = await invoke("multi64_cart", { url: $("play-url").value.trim() });
    if (running) setCart(running);
  } catch {
    // Not running: the last choice stands.
  }
}

async function loadSeed(path) {
  loadedPath = path;
  resetResults();
  setError($("load-error"), "");
  $("release-notes").textContent = "";
  show($("release-notes"), false);
  setSeedLine("Reading…");
  $("btn-open-patch").disabled = true;
  $("drop-zone").dataset.state = "busy";
  try {
    const r = await invoke("load_rom", { path, cart: selectedCart() });
    const d = r.detection;
    $("seed-file").textContent = `${r.fileName} (${(r.size / 1048576).toFixed(1)} MiB)`;
    const h = d.header;
    $("seed-header").textContent = h
      ? `${h.name || "(no name)"} · ${h.game_code} v${h.version} · boot code ${d.cic || "patched or unknown"} · ${d.byte_order}`
      : "unreadable";
    const report = renderChecks(d);
    // A different randomizer release: worth knowing before the console, never a refusal.
    const notes = r.notes || [];
    $("release-notes").textContent = notes.join(" ");
    show($("release-notes"), notes.length > 0);
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

/*
 * The game menu: a button and a listbox in place of the select they drive (index.html). The select
 * keeps the value, the options and the disabled state, and the rest of the page reads and sets it
 * as before; `sync` brings the button up to date after any of that. Choosing here sets its value
 * and fires its `change`, exactly as choosing in the select would.
 */
const gameMenu = {
  select: null,
  button: null,
  list: null,
  active: -1,
  typed: "",
  typedAt: 0,

  init() {
    this.select = $("play-game-select");
    this.button = $("play-game-button");
    this.list = $("play-game-list");
    this.button.addEventListener("click", () => (this.isOpen() ? this.close(true) : this.open()));
    this.button.addEventListener("keydown", (e) => {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) {
        e.preventDefault();
        this.open();
      }
    });
    this.list.addEventListener("keydown", (e) => this.key(e));
    this.list.addEventListener("click", (e) => {
      const li = e.target.closest("li");
      if (li) this.choose(li.dataset.value);
    });
    this.list.addEventListener("mousemove", (e) => {
      const li = e.target.closest("li");
      if (li) this.activate(this.items().indexOf(li), false);
    });
    // A click anywhere else closes it without choosing, as a select's list does.
    document.addEventListener("pointerdown", (e) => {
      if (this.isOpen() && !e.target.closest(".menu")) this.close(false);
    });
    this.list.addEventListener("focusout", (e) => {
      if (this.isOpen() && !this.list.contains(e.relatedTarget) && e.relatedTarget !== this.button) {
        this.close(false);
      }
    });
  },

  /** One row per game, from the select's options; the "Choose a game…" placeholder is the button's. */
  build() {
    const rows = [...this.select.options]
      .filter((o) => o.value)
      .map((o, i) => {
        const li = document.createElement("li");
        li.id = `play-game-option-${i}`;
        li.setAttribute("role", "option");
        li.dataset.value = o.value;
        li.textContent = o.textContent;
        return li;
      });
    this.list.replaceChildren(...rows);
    this.sync();
  },

  sync() {
    // renderPlay can run before initPlay has set the menu up; build() syncs it then.
    if (!this.select) return;
    const chosen = this.select.selectedOptions[0];
    $("play-game-text").textContent = chosen ? chosen.textContent : "";
    this.button.disabled = this.select.disabled;
    for (const li of this.items()) {
      li.setAttribute("aria-selected", String(li.dataset.value === this.select.value));
    }
    if (this.select.disabled && this.isOpen()) this.close(false);
  },

  items() {
    return [...this.list.children];
  },

  isOpen() {
    return !this.list.hidden;
  },

  open() {
    if (this.button.disabled || !this.items().length) return;
    const items = this.items();
    show(this.list, true);
    this.button.setAttribute("aria-expanded", "true");
    // Downward unless the window has no room for it there and more above.
    this.list.classList.remove("up");
    const room = this.list.getBoundingClientRect();
    const button = this.button.getBoundingClientRect();
    if (room.bottom > window.innerHeight && button.top > window.innerHeight - button.bottom) {
      this.list.classList.add("up");
    }
    const current = items.findIndex((li) => li.dataset.value === this.select.value);
    this.activate(Math.max(current, 0), true);
    this.list.focus();
  },

  /** `refocus`: back to the button, as after choosing or Escape. */
  close(refocus) {
    show(this.list, false);
    this.button.setAttribute("aria-expanded", "false");
    this.list.removeAttribute("aria-activedescendant");
    if (refocus) this.button.focus();
  },

  activate(i, scroll) {
    const items = this.items();
    if (!items.length) return;
    this.active = Math.min(Math.max(i, 0), items.length - 1);
    items.forEach((li, n) => li.classList.toggle("active", n === this.active));
    const li = items[this.active];
    this.list.setAttribute("aria-activedescendant", li.id);
    if (scroll) li.scrollIntoView({ block: "nearest" });
  },

  choose(value) {
    this.close(true);
    if (value === this.select.value) return;
    this.select.value = value;
    this.select.dispatchEvent(new Event("change"));
    this.sync();
  },

  key(e) {
    const page = 6;
    const last = this.items().length - 1;
    // A space in the middle of typing a name is part of it, not a choice.
    const typing = e.key === " " && this.typed && Date.now() - this.typedAt < 700;
    const moves = {
      ArrowDown: this.active + 1,
      ArrowUp: this.active - 1,
      PageDown: this.active + page,
      PageUp: this.active - page,
      Home: 0,
      End: last,
    };
    if (e.key in moves) {
      e.preventDefault();
      this.activate(moves[e.key], true);
    } else if (e.key === "Enter" || (e.key === " " && !typing)) {
      e.preventDefault();
      const li = this.items()[this.active];
      if (li) this.choose(li.dataset.value);
    } else if (e.key === "Escape") {
      // Its own: the page's Escape closes dialogs, and this is not one.
      e.preventDefault();
      e.stopPropagation();
      this.close(true);
    } else if (e.key === "Tab") {
      this.close(false);
    } else if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      // Type to jump: letters typed together name a game; one letter again moves to the next.
      const now = Date.now();
      this.typed = now - this.typedAt < 700 ? this.typed + e.key.toLowerCase() : e.key.toLowerCase();
      this.typedAt = now;
      const items = this.items();
      const same = [...this.typed].every((c) => c === this.typed[0]);
      const needle = same ? this.typed[0] : this.typed;
      const from = same ? this.active + 1 : this.active;
      for (let n = 0; n < items.length; n++) {
        const i = (from + n) % items.length;
        if (items[i].textContent.toLowerCase().startsWith(needle)) {
          this.activate(i, true);
          break;
        }
      }
    }
  },
};

function renderPlay() {
  const game = selectedGame();
  $("play-connector").textContent = game ? game.connector : "—";
  // A pending client hint is dropped by choosing another game, which changes what they say.
  renderLinks(lastStatus);
  $("btn-play-start").disabled = playRunning || !game;
  $("btn-play-stop").disabled = !playRunning;
  $("play-game-select").disabled = playRunning;
  $("play-url").disabled = playRunning;
  gameMenu.sync();
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
    // A link that is up says so in a word: the interesting rows are the ones that are not,
    // and the status line says what to do about them. Every game's client is "the AP client"
    // here; which one it is underneath is the session log's business, not the player's.
    let text = LINK_TEXT[key][state];
    if (key === "client") {
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
    $("client-fix-text").textContent = sentence(setup.message);
    $("client-fix-path").textContent = setup.path || "";
    openDialog("clientFix");
    return; // The dialog's buttons carry on from here.
  }
  // A version AP64 does not recognize may still connect: say so, and start anyway.
  if (setup?.state === "unknown") setError($("play-error"), sentence(setup.message));
  await startAfterNotes();
}

/*
 * A game's notes: what to know about its randomizer on a console, which AP64 cannot change.
 * Shown at Start, after the client fix and never over it, until put away for that game. What
 * was put away is the notes' text, so a game whose notes change shows them again.
 */
const NOTES_KEY = "ap64.notesSeen.";

function notesSeen(game) {
  try {
    return localStorage.getItem(NOTES_KEY + game.id) === game.notes.join("\n");
  } catch {
    return false;
  }
}

async function startAfterNotes() {
  const game = selectedGame();
  if (game?.notes?.length && !notesSeen(game)) {
    $("notes-title").textContent = `Before you play ${game.name}`;
    $("notes-list").replaceChildren(
      ...game.notes.map((note) => {
        const li = document.createElement("li");
        li.textContent = note;
        return li;
      }),
    );
    $("notes-hide").checked = false;
    openDialog("notes");
    return; // The dialog's buttons carry on from here.
  }
  await beginSession();
}

async function startFromNotes() {
  const game = selectedGame();
  if (game && $("notes-hide").checked) {
    try {
      localStorage.setItem(NOTES_KEY + game.id, game.notes.join("\n"));
    } catch {
      // Not remembered: they show again next time, which is the safe way to fail.
    }
  }
  closeDialog();
  await beginSession();
}

async function fixAndStart() {
  const game = selectedGame();
  if (!game) return closeDialog();
  $("btn-client-fix-go").disabled = true;
  try {
    await invoke("play_client_fix", { game: game.id });
    closeDialog();
    clientHint = "Restart the Archipelago Launcher, then open the AP client";
    renderLinks(lastStatus);
    await startAfterNotes();
  } catch (e) {
    closeDialog();
    setError($("play-error"), String(e));
  } finally {
    $("btn-client-fix-go").disabled = false;
  }
}

async function initPlay() {
  gameMenu.init();
  // The game list first: choosing a ROM selects its game, which needs the options there.
  games = (await invoke("play_games")) || [];
  for (const g of games) {
    const o = document.createElement("option");
    o.value = g.id;
    o.textContent = g.name;
    $("play-game-select").append(o);
  }
  gameMenu.build();
  listen("play://status", (event) => renderStatus(event.payload || {}));
  $("play-url").value = readUrl((await invoke("play_default_url")) || "");
  // Needs the Multi64 address, to ask which cart it is set up for.
  initCarts();
  $("play-game-select").addEventListener("change", () => {
    clientHint = "";
    renderPlay();
  });
  $("btn-client-fix-go").addEventListener("click", fixAndStart);
  $("btn-client-fix-cancel").addEventListener("click", closeDialog);
  $("client-fix-backdrop").addEventListener("click", closeDialog);
  $("btn-notes-go").addEventListener("click", startFromNotes);
  $("btn-notes-cancel").addEventListener("click", closeDialog);
  $("notes-backdrop").addEventListener("click", closeDialog);
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
  $("btn-welcome-close").addEventListener("click", closeDialog);
  $("welcome-backdrop").addEventListener("click", closeDialog);
  if (!welcomed()) openDialog("welcome");
  initPlay();
  try {
    profiles = (await invoke("profiles")) || [];
  } catch {
    profiles = [];
  }
  renderGames();
}

init();
