const { invoke } = window.__TAURI__.core;

async function refreshStatus() {
  try {
    const s = await invoke("get_daemon_status");
    document.getElementById("status-running").textContent = s.running
      ? "Running"
      : "Stopped";
    document.getElementById("status-healthy").textContent = s.healthy
      ? "OK"
      : s.running
        ? "Waiting…"
        : "—";
    document.getElementById("status-listen").textContent = s.listen || "—";
    document.getElementById("status-msg").textContent = s.message || "—";
    // Match the tray, which offers only the action that applies. Leaving both live means
    // "Start daemon" on a running daemon, which reports a failure for a no-op.
    document.getElementById("btn-start").disabled = s.running;
    document.getElementById("btn-stop").disabled = !s.running;
  } catch (e) {
    document.getElementById("status-msg").textContent = String(e);
    // Status is unknown, so neither action can be ruled out; leave both usable.
    document.getElementById("btn-start").disabled = false;
    document.getElementById("btn-stop").disabled = false;
  }
}

async function refreshPorts() {
  const ports = await invoke("get_serial_ports");
  const auto = await invoke("get_auto_serial");
  const sel = document.getElementById("serial-port");
  sel.innerHTML = "";
  const optAuto = document.createElement("option");
  optAuto.value = "";
  optAuto.textContent = auto ? `Auto (${auto})` : "Auto (no port found)";
  sel.appendChild(optAuto);
  for (const p of ports) {
    const o = document.createElement("option");
    o.value = p;
    o.textContent = p;
    sel.appendChild(o);
  }
  document.getElementById("auto-hint").textContent = auto
    ? `Default selection uses USB when possible: ${auto}`
    : "No serial ports detected.";
  return { ports, auto };
}

function applySettingsToForm(s) {
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

async function loadSettings() {
  const s = await invoke("get_settings");
  applySettingsToForm(s);
  await refreshPorts();
  const saved = s.serialPort;
  const sel = document.getElementById("serial-port");
  if (saved && [...sel.options].some((o) => o.value === saved)) {
    sel.value = saved;
  } else {
    sel.value = "";
  }
  updateDevPanel();
  updateLogPresetVisibility();
  updateStartMinimizedGate();
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

function setSettingsOpen(open) {
  const panel = document.getElementById("settings-panel");
  const backdrop = document.getElementById("settings-backdrop");
  const opener = document.getElementById("btn-open-settings");
  panel.hidden = !open;
  backdrop.hidden = !open;
  opener.setAttribute("aria-expanded", open ? "true" : "false");
  backdrop.setAttribute("aria-hidden", open ? "false" : "true");
  document.documentElement.classList.toggle("settings-open", open);
  document.body.classList.toggle("settings-open", open);
  if (open) {
    // Start from last saved values; do not apply draft toggles until Save.
    void loadSettings().then(() => {
      document.getElementById("btn-close-settings").focus();
    });
  } else {
    // Discard unsaved edits; only "Save settings" calls set_settings on the backend.
    void loadSettings().then(() => opener.focus());
  }
}

const EMPTY_LOG_HINT =
  "(No log lines yet. Output from multi64d appears here only when this app starts the daemon; if the log stays empty after Start, check Status and Note.)";

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
    btn.textContent = st.installed ? "Open Xfer64" : "Install Xfer64";
    btn.disabled = !st.installed && !st.installerAvailable;
    btn.title =
      btn.disabled && !st.installed
        ? "Xfer64 installer was not bundled; build xfer64 before building the main GUI."
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
    try {
      await invoke("launch_or_install_xfer64");
      await updateXfer64Button();
    } catch (e) {
      console.error("launch_or_install_xfer64:", e);
      document.getElementById("status-msg").textContent = String(e);
    }
  });

  document.getElementById("btn-open-settings").addEventListener("click", () => {
    setSettingsOpen(true);
  });
  document.getElementById("btn-close-settings").addEventListener("click", () => {
    setSettingsOpen(false);
  });
  document.getElementById("settings-backdrop").addEventListener("click", () => {
    setSettingsOpen(false);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !document.getElementById("settings-panel").hidden) {
      setSettingsOpen(false);
    }
  });

  document.getElementById("btn-ports").addEventListener("click", async () => {
    await refreshPorts();
  });
  document.getElementById("btn-start").addEventListener("click", async () => {
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
      document.getElementById("status-msg").textContent = msg;
    }
  });
  document.getElementById("btn-stop").addEventListener("click", async () => {
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
      document.getElementById("status-msg").textContent = String(e);
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

  setInterval(refreshStatus, 2000);
  setInterval(() => {
    if (!document.getElementById("dev-panel").hidden) {
      refreshLog();
    }
  }, 1500);

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
