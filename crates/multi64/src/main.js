const { invoke } = window.__TAURI__.core;

/** Set text only when it changed, so the 2 s status poll does not re-announce the status region. */
function setText(id, text) {
  const el = document.getElementById(id);
  if (el.textContent !== text) el.textContent = text;
}

async function refreshStatus() {
  try {
    // While Settings is open and loaded, the same poll (and enumeration) refreshes its port hints.
    const generation = settingsGeneration;
    const s = await invoke("get_daemon_status", { withPorts: settingsSnapshot !== null });
    if (s.portOptions && generation === settingsGeneration && settingsSnapshot !== null) {
      applyPolledPortOptions(s.portOptions);
    }
    setText("status-running", s.running ? "Running" : "Stopped");
    setText("status-healthy", s.healthy ? "OK" : s.running ? "Not responding yet" : "—");
    setText("status-cart", s.cart || "—");
    setText("status-listen", s.listen || "—");
    setText("status-msg", s.message || "—");
    // A bridge that is running now (e.g. started from the tray) makes an earlier start error stale.
    if (s.running) showInlineError("status-error", "");
    // Match the tray, which offers only the action that applies. Leaving both live means
    // "Start bridge" on a running bridge, which reports a failure for a no-op.
    document.getElementById("btn-start").disabled = s.running;
    document.getElementById("btn-stop").disabled = !s.running;
  } catch (e) {
    setText("status-msg", String(e));
    // Status is unknown, so neither action can be ruled out; leave both usable.
    document.getElementById("btn-start").disabled = false;
    document.getElementById("btn-stop").disabled = false;
  }
}

/**
 * The last port enumeration's auto pick, kept so a cart change can re-render without re-enumerating.
 * `everdriveHint` is the backend's wording (`SerialPortOptions`), so it is not repeated here.
 */
let lastAuto = { auto: null, autoWarning: null, ambiguous: false, everdriveHint: "" };

/** The port names the Serial port dropdown was last built from; null before the first build. */
let lastPorts = null;

/** Cart values the backend accepts (`CartSetting`); anything else reads as the default, Auto-detect. */
const CART_VALUES = ["auto", "sc64", "ed64", "ed64pro"];

/** @param {unknown} v */
function knownCart(v) {
  return CART_VALUES.includes(v) ? v : "auto";
}

/** The Settings → Cart warning for each experimental cart. */
const CART_HINTS = {
  ed64:
    "Experimental: the EverDrive-64 X7 link has never been run against a cart, so a running bridge does not show that it works. Saving restarts the bridge if it is running.",
  ed64pro:
    "Experimental: the EverDrive-64 PRO link has never been run against a cart, so a running bridge does not show that it works. The EverDrive-64 PRO always runs at 921600 baud, so Baud does not apply. Saving restarts the bridge if it is running.",
};

async function refreshPorts() {
  // One call, one port enumeration: asking for the list and the auto pick separately enumerated
  // twice and could disagree if a cart was plugged in between the two.
  return applyPortOptions(await invoke("get_serial_port_options"));
}

function rememberAuto({ auto, autoWarning, ambiguous, everdriveHint }) {
  lastAuto = { auto, autoWarning, ambiguous: !!ambiguous, everdriveHint: everdriveHint || "" };
}

/**
 * Rebuild the Serial port dropdown from `ports`, keeping the selection if still listed. With
 * `keepSelection`, a selected port that is no longer listed stays as an option, so the selection
 * never changes.
 */
function fillPortSelect(ports, keepSelection) {
  lastPorts = [...ports];
  const sel = document.getElementById("serial-port");
  const selected = sel.value;
  const names = keepSelection && selected !== "" && !ports.includes(selected) ? [...ports, selected] : ports;
  sel.innerHTML = "";
  const optAuto = document.createElement("option");
  optAuto.value = "";
  sel.appendChild(optAuto);
  for (const p of names) {
    const o = document.createElement("option");
    o.value = p;
    o.textContent = p;
    sel.appendChild(o);
  }
  if ([...sel.options].some((o) => o.value === selected)) {
    sel.value = selected;
  }
}

/** Rebuild the Serial port dropdown from one enumeration, keeping the selection if still listed. */
function applyPortOptions(options) {
  rememberAuto(options);
  fillPortSelect(options.ports, false);
  renderCartAndAuto();
  return { ports: options.ports, auto: options.auto };
}

/**
 * The status poll's enumeration, while Settings is open: refresh the hints, but leave what the
 * user chose alone. The dropdown is rebuilt only when the port list changed, and keeps its
 * selection even when that port is gone, so `settingsHaveUnsavedEdits()` cannot change.
 */
function applyPolledPortOptions(options) {
  rememberAuto(options);
  const { ports } = options;
  const unchanged = lastPorts !== null && ports.length === lastPorts.length && ports.every((p, i) => p === lastPorts[i]);
  if (!unchanged) fillPortSelect(ports, true);
  renderCartAndAuto();
}

/**
 * Both hints depend on the cart being edited, so they re-render when it changes. The Auto options
 * read plain "Auto-detect" in both dropdowns; what Auto found is said in the hints.
 *
 * - Cart on Auto-detect: what was found is about the cart, so it goes under Cart — the
 *   SummerCart64's port, or that Start bridge tests each serial port. Only more than one
 *   SummerCart64 warns, under Serial port, because that is the one case Start refuses.
 * - Cart on SummerCart64: the Serial port hint names the port, or warns there is none to use.
 * - A chosen EverDrive: the Serial port hint warns that Auto-detect gives it no port.
 *
 * A port picked by hand needs no Serial port hint. The backend enforces the same rules
 * (`plan_start`): Auto never gives an EverDrive a port, SC64's included.
 */
function renderCartAndAuto() {
  const cart = knownCart(document.getElementById("cart").value);
  const everdrive = cart === "ed64" || cart === "ed64pro";
  const { auto, autoWarning, ambiguous, everdriveHint } = lastAuto;
  const sel = document.getElementById("serial-port");
  const onAuto = sel.value === "";
  const optAuto = sel.options[0];
  if (optAuto) {
    optAuto.textContent = "Auto-detect";
  }

  // Under Cart: what Auto-detect found, or the experimental warning for a chosen EverDrive.
  let cartText = CART_HINTS[cart] || "";
  if (cart === "auto" && !onAuto) {
    // A port picked by hand is the only one Start looks at, whatever is on USB elsewhere.
    cartText = `Start bridge identifies the cart on ${sel.value}.`;
  } else if (cart === "auto") {
    // More than one SummerCart64 is said under Serial port, where the user picks one.
    cartText = auto
      ? `Found a SummerCart64 on ${auto}.`
      : ambiguous
        ? ""
        : "No SummerCart64 found by its USB IDs. Start bridge tests each serial port until a cart answers.";
  }
  const cartHint = document.getElementById("cart-hint");
  cartHint.textContent = cartText;
  cartHint.hidden = !cartText;
  cartHint.classList.toggle("hint-warning", everdrive);

  // Under Serial port: only on Auto. It warns when Start would refuse, and names the port Auto
  // picked when Cart is set to SummerCart64 (on Auto-detect, the Cart hint names it). Cart on
  // Auto-detect with no SummerCart64 gets nothing here: Start probes each port, so nothing is wrong.
  let portText = "";
  let portWarn = false;
  if (onAuto) {
    if (everdrive) {
      portText = everdriveHint;
      portWarn = true;
    } else if (ambiguous) {
      portText = autoWarning || "";
      portWarn = true;
    } else if (cart === "sc64") {
      portText = auto
        ? `Uses ${auto}.`
        : autoWarning || "No SummerCart64 found. Plug it in, or pick its serial port.";
      portWarn = !auto;
    }
  }
  const hint = document.getElementById("auto-hint");
  hint.textContent = portText;
  hint.hidden = !portText;
  hint.classList.toggle("hint-warning", portWarn);
}

function applySettingsToForm(s) {
  // Anything unrecognised reads as the proven default rather than leaving the select blank.
  document.getElementById("cart").value = knownCart(s.cart);
  document.getElementById("baud").value = String(s.baud ?? 115200);
  document.getElementById("listen").value = s.listen || "127.0.0.1:38765";
  document.getElementById("auto-start-daemon").checked = s.autoStartDaemon !== false;
  document.getElementById("autostart-app").checked = !!s.autostartApp;
  document.getElementById("tray-enabled").checked = s.trayEnabled !== false;
  document.getElementById("minimize-tray").checked = s.minimizeToTrayOnClose !== false;
  document.getElementById("start-minimized").checked = !!s.startMinimized;
  document.getElementById("developer-mode").checked = !!s.developerMode;
  const preset = s.multi64dLogPreset || "default";
  const pr = document.getElementById("multi64d-log-preset");
  if ([...pr.options].some((o) => o.value === preset)) {
    pr.value = preset;
  } else {
    pr.value = "default";
  }
}

function readSettingsFromForm() {
  const serialSel = document.getElementById("serial-port");
  const raw = serialSel.value;
  return {
    serialPort: raw === "" ? null : raw,
    cart: knownCart(document.getElementById("cart").value),
    baud: parseInt(document.getElementById("baud").value, 10) || 115200,
    listen: document.getElementById("listen").value.trim() || "127.0.0.1:38765",
    autoStartDaemon: document.getElementById("auto-start-daemon").checked,
    autostartApp: document.getElementById("autostart-app").checked,
    trayEnabled: document.getElementById("tray-enabled").checked,
    minimizeToTrayOnClose: document.getElementById("minimize-tray").checked,
    startMinimized: document.getElementById("start-minimized").checked,
    developerMode: document.getElementById("developer-mode").checked,
    multi64dLogPreset: document.getElementById("multi64d-log-preset").value || "default",
  };
}

/**
 * Fill the form from the saved settings and one fresh port enumeration. Both are fetched before
 * anything is written, so when `isCurrent()` says Settings was opened or closed again while the
 * backend answered, this returns false having touched nothing; otherwise it returns true.
 */
async function loadSettings(isCurrent = () => true) {
  const s = await invoke("get_settings");
  const options = await invoke("get_serial_port_options");
  if (!isCurrent()) return false;
  applySettingsToForm(s);
  applyPortOptions(options);
  const saved = s.serialPort;
  const sel = document.getElementById("serial-port");
  if (saved && [...sel.options].some((o) => o.value === saved)) {
    sel.value = saved;
  } else {
    sel.value = "";
  }
  // The hint depends on whether Auto is selected, which is only known now.
  renderCartAndAuto();
  updateDevPanel();
  updateLogPresetVisibility();
  updateStartMinimizedGate();
  return true;
}

function updateDevPanel() {
  const dev = document.getElementById("developer-mode").checked;
  document.getElementById("dev-panel").hidden = !dev;
}

function updateLogPresetVisibility() {
  const dev = document.getElementById("developer-mode").checked;
  for (const el of document.querySelectorAll(".dev-only-setting")) {
    el.hidden = !dev;
  }
}

/** Start-minimized only makes sense when the tray is enabled (otherwise there is no way to show the window). */
function updateStartMinimizedGate() {
  const trayOn = document.getElementById("tray-enabled").checked;
  const sm = document.getElementById("start-minimized");
  sm.disabled = !trayOn;
  if (!trayOn) {
    sm.checked = false;
  }
}

/**
 * Show `message` in an inline error line beside what failed, or hide the line when it is empty.
 *
 * Errors used to go to Status → Note, which sits behind the open Settings dialog and is
 * overwritten by the next status poll two seconds later.
 */
function showInlineError(id, message) {
  const el = document.getElementById(id);
  if (!el) return;
  el.textContent = message || "";
  el.hidden = !message;
}

/** Every dialog on the page; while any is open, `dialog-open` on html/body stops the page scrolling. */
const DIALOG_IDS = ["settings-panel", "help-panel", "discard-panel"];

function syncDialogOpen() {
  const open = DIALOG_IDS.some((id) => !document.getElementById(id).hidden);
  document.documentElement.classList.toggle("dialog-open", open);
  document.body.classList.toggle("dialog-open", open);
}

/**
 * `readSettingsFromForm()` as JSON, taken when Settings finished loading on open; null while
 * Settings is closed or still loading. Appearance controls are not in that form (they apply
 * immediately), so they never count as unsaved.
 */
let settingsSnapshot = null;

function settingsHaveUnsavedEdits() {
  return settingsSnapshot !== null && JSON.stringify(readSettingsFromForm()) !== settingsSnapshot;
}

/** Controls a keyboard user can reach inside `root`: shown, enabled, and not `tabindex="-1"`. */
function focusableIn(root) {
  return [...root.querySelectorAll("button, [href], input, select, textarea, [tabindex]")].filter(
    (el) => !el.disabled && el.tabIndex >= 0 && !el.closest("[hidden]") && el.getClientRects().length > 0,
  );
}

/** The open dialog on top. The discard confirm opens above Settings; Help and Settings never stack. */
function topDialog() {
  return (
    ["discard-panel", "help-panel", "settings-panel"]
      .map((id) => document.getElementById(id))
      .find((el) => !el.hidden) || null
  );
}

/** Tab and Shift+Tab cycle inside the top dialog, so nothing behind it can take focus. */
function trapDialogTab(e) {
  const dialog = topDialog();
  if (!dialog) return;
  const items = focusableIn(dialog);
  if (items.length === 0) {
    e.preventDefault();
    return;
  }
  const first = items[0];
  const last = items[items.length - 1];
  const inside = dialog.contains(document.activeElement);
  if (e.shiftKey && (!inside || document.activeElement === first)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && (!inside || document.activeElement === last)) {
    e.preventDefault();
    first.focus();
  }
}

/** Give focus back to what had it before a dialog opened, or to `fallbackId` if that is gone. */
function returnFocus(saved, fallbackId) {
  const usable = saved instanceof HTMLElement && saved.isConnected && focusableIn(document.body).includes(saved);
  (usable ? saved : document.getElementById(fallbackId)).focus();
}

/** What had focus before Settings opened. */
let settingsReturnFocus = null;

/**
 * Bumped on every open and close. Each opens or closes with a reload that finishes later; one
 * whose generation has moved on must not refill the form or move focus.
 */
let settingsGeneration = 0;

/** True for the whole of a Save: closing is locked so the form cannot close or reopen mid-save. */
let settingsSaving = false;

function setSettingsSaving(saving) {
  settingsSaving = saving;
  for (const id of ["btn-save", "btn-cancel-settings", "btn-close-settings"]) {
    document.getElementById(id).disabled = saving;
  }
  // Disabling Save dropped its focus; a failed Save leaves Settings open, so give it back.
  const panel = document.getElementById("settings-panel");
  if (!saving && !panel.hidden && !panel.contains(document.activeElement)) {
    document.getElementById("btn-save").focus();
  }
}

function setSettingsOpen(open) {
  const panel = document.getElementById("settings-panel");
  const backdrop = document.getElementById("settings-backdrop");
  const opener = document.getElementById("btn-open-settings");
  const generation = ++settingsGeneration;
  const isCurrent = () => generation === settingsGeneration;
  if (open && panel.hidden) settingsReturnFocus = document.activeElement;
  if (!open && !document.getElementById("discard-panel").hidden) {
    // The confirm asks about this Settings form; it must not outlive it.
    setDiscardOpen(false);
    discardReturnFocus = null;
  }
  showInlineError("settings-error", "");
  panel.hidden = !open;
  backdrop.hidden = !open;
  opener.setAttribute("aria-expanded", open ? "true" : "false");
  settingsSnapshot = null;
  syncDialogOpen();
  if (open) {
    // Start from last saved values; do not apply draft toggles until Save.
    void loadSettings(isCurrent).then((applied) => {
      // Closed (or reopened) before loading finished: nothing to snapshot or focus.
      if (!applied || !isCurrent() || panel.hidden) return;
      settingsSnapshot = JSON.stringify(readSettingsFromForm());
      document.getElementById("btn-close-settings").focus();
    });
  } else {
    // Discard unsaved edits; only "Save settings" calls set_settings on the backend.
    void loadSettings(isCurrent).then((applied) => {
      // Reopened before this reload finished: the form and focus belong to the new open now.
      if (!applied || !isCurrent()) return;
      returnFocus(settingsReturnFocus, opener.id);
      settingsReturnFocus = null;
    });
  }
}

/**
 * Close icon, backdrop and Esc: ask first when the form has unsaved edits. Save closes directly.
 * Does nothing while a Save is running; that Save closes Settings itself when it succeeds.
 */
function requestCloseSettings() {
  if (settingsSaving) return;
  if (settingsHaveUnsavedEdits()) {
    setDiscardOpen(true);
  } else {
    setSettingsOpen(false);
  }
}

/** Where focus was in Settings when the discard confirm opened, to return to on "Keep editing". */
let discardReturnFocus = null;

function setDiscardOpen(open) {
  if (open) discardReturnFocus = document.activeElement;
  document.getElementById("discard-panel").hidden = !open;
  document.getElementById("discard-backdrop").hidden = !open;
  syncDialogOpen();
  if (open) document.getElementById("btn-discard-keep").focus();
}

function keepEditingSettings() {
  setDiscardOpen(false);
  const settings = document.getElementById("settings-panel");
  const back =
    discardReturnFocus instanceof HTMLElement && settings.contains(discardReturnFocus)
      ? discardReturnFocus
      : document.getElementById("btn-close-settings");
  discardReturnFocus = null;
  back.focus();
}

function discardSettingsEdits() {
  setDiscardOpen(false);
  discardReturnFocus = null;
  setSettingsOpen(false);
}

/** What had focus before Help opened. */
let helpReturnFocus = null;

function setHelpOpen(open) {
  const panel = document.getElementById("help-panel");
  if (open && panel.hidden) helpReturnFocus = document.activeElement;
  panel.hidden = !open;
  document.getElementById("help-backdrop").hidden = !open;
  syncDialogOpen();
  if (open) {
    document.getElementById("btn-close-help").focus();
  } else {
    returnFocus(helpReturnFocus, "btn-open-help");
    helpReturnFocus = null;
  }
}

const EMPTY_LOG_HINT =
  "(No log lines yet. Output from multi64d appears here only when Multi64 starts the bridge. If the log stays empty after Start bridge, check Note on the Status card.)";

/** Remove ANSI CSI sequences (e.g. tracing SGR `[2m` / `[33m` / `[0m`) so the log shows plain text. */
function stripAnsi(s) {
  return String(s).replace(/\u001b\[[\d;]*[A-Za-z]/g, "");
}

async function refreshLog() {
  const el = document.getElementById("daemon-log");
  try {
    const lines = await invoke("get_daemon_logs");
    const arr = Array.isArray(lines) ? lines : [];
    el.textContent = arr.length
      ? arr.map((line) => stripAnsi(line)).join("\n")
      : EMPTY_LOG_HINT;
  } catch (e) {
    el.textContent = `Failed to load log: ${e}`;
  }
}

async function updateXfer64Button() {
  const btn = document.getElementById("btn-open-explorer");
  try {
    const st = await invoke("get_xfer64_state");
    btn.textContent = st.installed ? "Open Xfer64" : "Install Xfer64…";
    btn.disabled = !st.installed && !st.installerAvailable;
    btn.title =
      btn.disabled && !st.installed
        ? "The Xfer64 installer was not bundled. Build Xfer64 before building Multi64."
        : "";
  } catch (e) {
    btn.textContent = "Xfer64";
    btn.disabled = false;
    btn.title = String(e);
  }
}

window.addEventListener("DOMContentLoaded", async () => {
  await loadSettings();
  await refreshStatus();
  await updateXfer64Button();
  if (document.getElementById("developer-mode").checked) {
    await refreshLog();
  }

  document.getElementById("btn-refresh").addEventListener("click", refreshStatus);

  document.getElementById("btn-open-explorer").addEventListener("click", async () => {
    showInlineError("xfer64-error", "");
    try {
      await invoke("launch_or_install_xfer64");
      await updateXfer64Button();
    } catch (e) {
      console.error("launch_or_install_xfer64:", e);
      showInlineError("xfer64-error", String(e));
    }
  });

  document.getElementById("btn-open-settings").addEventListener("click", () => {
    setSettingsOpen(true);
  });
  document.getElementById("btn-close-settings").addEventListener("click", requestCloseSettings);
  document.getElementById("btn-cancel-settings").addEventListener("click", requestCloseSettings);
  document.getElementById("settings-backdrop").addEventListener("click", requestCloseSettings);

  document.getElementById("btn-discard-keep").addEventListener("click", keepEditingSettings);
  document.getElementById("discard-backdrop").addEventListener("click", keepEditingSettings);
  document.getElementById("btn-discard-confirm").addEventListener("click", discardSettingsEdits);

  document.getElementById("btn-open-help").addEventListener("click", () => setHelpOpen(true));
  document.getElementById("btn-close-help").addEventListener("click", () => setHelpOpen(false));
  document.getElementById("btn-help-done").addEventListener("click", () => setHelpOpen(false));
  document.getElementById("help-backdrop").addEventListener("click", () => setHelpOpen(false));

  // Esc closes the top dialog only: the discard confirm, else Help, else Settings. Tab stays inside it.
  document.addEventListener("keydown", (e) => {
    if (e.key === "Tab") {
      trapDialogTab(e);
      return;
    }
    if (e.key !== "Escape") return;
    if (!document.getElementById("discard-panel").hidden) {
      keepEditingSettings();
    } else if (!document.getElementById("help-panel").hidden) {
      setHelpOpen(false);
    } else if (!document.getElementById("settings-panel").hidden) {
      requestCloseSettings();
    }
  });

  document.getElementById("btn-ports").addEventListener("click", async () => {
    await refreshPorts();
  });
  document.getElementById("cart").addEventListener("change", renderCartAndAuto);
  document.getElementById("serial-port").addEventListener("change", renderCartAndAuto);
  document.getElementById("btn-start").addEventListener("click", async () => {
    showInlineError("status-error", "");
    try {
      await invoke("daemon_start");
      await refreshStatus();
      await refreshLog();
    } catch (e) {
      const msg = String(e);
      console.error("daemon_start:", e);
      await refreshStatus();
      await refreshLog();
      document.getElementById("status-running").textContent = "Stopped";
      document.getElementById("status-healthy").textContent = "—";
      // Its own line: Note is rewritten by the next status poll, which would erase the error.
      showInlineError("status-error", msg);
    }
  });
  document.getElementById("btn-stop").addEventListener("click", async () => {
    showInlineError("status-error", "");
    await invoke("daemon_stop");
    await refreshStatus();
    await refreshLog();
  });
  document.getElementById("tray-enabled").addEventListener("change", updateStartMinimizedGate);
  document.getElementById("developer-mode").addEventListener("change", () => {
    // In-form hints only; main-window dev panel updates after Save (or on load / close reload).
    updateLogPresetVisibility();
  });

  document.getElementById("btn-save").addEventListener("click", async () => {
    if (settingsSaving) return;
    showInlineError("settings-error", "");
    setSettingsSaving(true);
    try {
      const settings = readSettingsFromForm();
      await invoke("set_settings", { settings });
      await refreshStatus();
      if (settings.developerMode) {
        await refreshLog();
      }
      setSettingsOpen(false);
    } catch (e) {
      console.error("set_settings:", e);
      showInlineError("settings-error", `Settings were not saved: ${e}`);
    } finally {
      setSettingsSaving(false);
    }
  });
  document.getElementById("btn-log-clear").addEventListener("click", async () => {
    try {
      await invoke("clear_daemon_logs");
      await refreshLog();
    } catch (e) {
      console.error("clear_daemon_logs:", e);
      document.getElementById("daemon-log").textContent = `Failed to clear log: ${e}`;
    }
  });
  document.getElementById("btn-log-refresh").addEventListener("click", () => {
    refreshLog();
  });

  // Both timers skip work while the window is not visible. Closing the window hides it to the
  // tray by default rather than exiting, so without this the app keeps issuing a blocking health
  // check every two seconds -- tens of thousands a day -- for a window nobody is looking at.
  // `statusInFlight` stops a slow check from queueing more of itself behind it.
  let statusInFlight = false;
  setInterval(async () => {
    if (document.visibilityState !== "visible" || statusInFlight) return;
    statusInFlight = true;
    try {
      await refreshStatus();
    } finally {
      statusInFlight = false;
    }
  }, 2000);
  setInterval(() => {
    if (document.visibilityState !== "visible") return;
    if (!document.getElementById("dev-panel").hidden) {
      refreshLog();
    }
  }, 1500);
  // Refresh on the way back so a hidden window is never showing stale status when it reappears.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void refreshStatus();
  });

  if (window.__TAURI__?.event?.listen) {
    await window.__TAURI__.event.listen("daemon-changed", () => {
      refreshStatus();
      refreshLog();
    });
  }

  window.addEventListener("focus", () => {
    void updateXfer64Button();
  });
});
