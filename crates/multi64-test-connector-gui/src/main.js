const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

function initTabs() {
  const buttons = document.querySelectorAll(".tab-btn");
  const panels = document.querySelectorAll(".tab-panel");
  buttons.forEach((btn) => {
    btn.addEventListener("click", () => {
      const id = btn.getAttribute("data-tab");
      buttons.forEach((b) => {
        const on = b.getAttribute("data-tab") === id;
        b.classList.toggle("active", on);
        b.setAttribute("aria-selected", on ? "true" : "false");
      });
      panels.forEach((p) => {
        p.hidden = p.getAttribute("data-tab") !== id;
      });
    });
  });
}

initTabs();

function logLine(s) {
  const el = document.getElementById("log");
  el.value += s + "\n";
  el.scrollTop = el.scrollHeight;
}

function getUrl() {
  return document.getElementById("url").value.trim() || "ws://127.0.0.1:38765/ws";
}

function getRecvTimeout() {
  const v = parseFloat(document.getElementById("recv-timeout").value, 10);
  return Number.isFinite(v) && v > 0 ? v : 5;
}

async function runRpc(command) {
  try {
    const out = await invoke("run_command", {
      url: getUrl(),
      recvTimeoutSecs: getRecvTimeout(),
      command,
    });
    logLine(out.trimEnd());
  } catch (e) {
    logLine(String(e));
  }
}

document.querySelectorAll("[data-cmd]").forEach((btn) => {
  btn.addEventListener("click", () => {
    const kind = btn.getAttribute("data-cmd");
    if (kind === "session-open") {
      const hex_challenge =
        document.getElementById("session-challenge").value.trim() ||
        "0000000000000000";
      runRpc({ kind: "SessionOpen", data: { hex_challenge } });
      return;
    }
    const map = {
      ping: { kind: "Ping", data: null },
      version: { kind: "Version", data: null },
      "req-controller": { kind: "ReqController", data: null },
      "session-close": { kind: "SessionClose", data: null },
      "eeprom-info": { kind: "EepromInfo", data: null },
      "sram-info": { kind: "SramInfo", data: null },
    };
    runRpc(map[kind]);
  });
});

document.getElementById("btn-echo").addEventListener("click", () => {
  const mode = document.querySelector('input[name="echo-mode"]:checked').value;
  let command;
  if (mode === "text") {
    const text = document.getElementById("echo-payload").value;
    command = { kind: "Echo", data: { hex: null, text } };
  } else if (mode === "hex") {
    const hex = document.getElementById("echo-payload").value.trim() || null;
    command = { kind: "Echo", data: { hex, text: null } };
  } else {
    command = { kind: "Echo", data: { hex: null, text: null } };
  }
  runRpc(command);
});

document.getElementById("btn-eeprom-read").addEventListener("click", () => {
  const offset = parseInt(document.getElementById("eeprom-roff").value, 10) || 0;
  const len = parseInt(document.getElementById("eeprom-rlen").value, 10) || 1;
  runRpc({ kind: "EepromRead", data: { offset, len } });
});

document.getElementById("btn-eeprom-write").addEventListener("click", () => {
  const offset = parseInt(document.getElementById("eeprom-woff").value, 10) || 0;
  const hex = document.getElementById("eeprom-whex").value.trim();
  runRpc({ kind: "EepromWrite", data: { offset, hex } });
});

document.getElementById("btn-sram-read").addEventListener("click", () => {
  const offset = parseInt(document.getElementById("sram-roff").value, 10) || 0;
  const len = parseInt(document.getElementById("sram-rlen").value, 10) || 1;
  runRpc({ kind: "SramRead", data: { offset, len } });
});

document.getElementById("btn-sram-write").addEventListener("click", () => {
  const offset = parseInt(document.getElementById("sram-woff").value, 10) || 0;
  const hex = document.getElementById("sram-whex").value.trim();
  runRpc({ kind: "SramWrite", data: { offset, hex } });
});

document.getElementById("btn-rumble").addEventListener("click", () => {
  const port = parseInt(document.getElementById("rumble-port").value, 10) || 0;
  const frames = parseInt(document.getElementById("rumble-frames").value, 10) || 0;
  runRpc({ kind: "Rumble", data: { port, frames } });
});

document.getElementById("btn-display-text").addEventListener("click", () => {
  const text = document.getElementById("display-text").value;
  runRpc({ kind: "DisplayText", data: { text } });
});

let unlistenListen = null;
let unlistenControllerPoll = null;

document.getElementById("btn-controller-poll-start").addEventListener("click", async () => {
  if (unlistenControllerPoll) {
    unlistenControllerPoll();
    unlistenControllerPoll = null;
  }
  unlistenControllerPoll = await listen("controller-poll-log", (e) => {
    logLine(e.payload);
  });
  const intervalMs = parseInt(
    document.getElementById("controller-poll-interval").value,
    10,
  );
  const intervalMsClamped = Number.isFinite(intervalMs) && intervalMs > 0 ? intervalMs : 50;
  try {
    await invoke("controller_poll_start", {
      url: getUrl(),
      recvTimeoutSecs: getRecvTimeout(),
      intervalMs: intervalMsClamped,
    });
  } catch (err) {
    logLine(String(err));
  }
});

document.getElementById("btn-controller-poll-stop").addEventListener("click", () => {
  invoke("controller_poll_stop").catch((e) => logLine(String(e)));
});

document.getElementById("btn-listen-start").addEventListener("click", async () => {
  if (unlistenListen) {
    unlistenListen();
    unlistenListen = null;
  }
  unlistenListen = await listen("listen-log", (e) => {
    logLine(e.payload);
  });
  const duration = parseFloat(document.getElementById("listen-duration").value, 10);
  const durationSecs = Number.isFinite(duration) ? duration : 0;
  try {
    await invoke("listen_start", {
      url: getUrl(),
      durationSecs,
    });
  } catch (err) {
    logLine(String(err));
  }
});

document.getElementById("btn-listen-stop").addEventListener("click", () => {
  invoke("listen_stop").catch((e) => logLine(String(e)));
});

document.getElementById("btn-sc64-echo-test").addEventListener("click", async () => {
  const port = document.getElementById("serial-port").value.trim() || "COM3";
  const baud = parseInt(document.getElementById("serial-baud").value, 10) || 115200;
  const payload = document.getElementById("serial-echo-payload").value;
  const timeoutSecs = parseInt(document.getElementById("serial-echo-timeout").value, 10) || 10;
  try {
    const out = await invoke("sc64_echo_test", {
      port,
      baud,
      payload,
      timeoutSecs,
    });
    logLine(out.trimEnd());
  } catch (e) {
    logLine(String(e));
  }
});

document.getElementById("btn-sc64-l3-framing-e2e").addEventListener("click", async () => {
  const port = document.getElementById("l3fe-port").value.trim() || "COM3";
  const baud = parseInt(document.getElementById("l3fe-baud").value, 10) || 115200;
  const timeoutSecs = parseInt(document.getElementById("l3fe-timeout").value, 10) || 10;
  const large = document.getElementById("l3fe-large").checked;
  try {
    const out = await invoke("sc64_l3_framing_e2e", {
      port,
      baud,
      timeoutSecs,
      large,
    });
    logLine(out.trimEnd());
  } catch (e) {
    logLine(String(e));
  }
});

document.getElementById("btn-ws-raw-echo").addEventListener("click", async () => {
  const url =
    document.getElementById("ws-raw-url").value.trim() || "ws://127.0.0.1:38765/ws";
  const recvTimeoutSecs = parseFloat(document.getElementById("ws-raw-timeout").value, 10) || 10;
  const payloadText = document.getElementById("ws-raw-payload").value;
  const jsonPing = document.getElementById("ws-raw-ping").checked;
  try {
    const out = await invoke("ws_raw_echo_run", {
      url,
      recvTimeoutSecs,
      payloadText,
      jsonPing,
    });
    logLine(out.trimEnd());
  } catch (e) {
    logLine(String(e));
  }
});

document.getElementById("btn-clear").addEventListener("click", () => {
  document.getElementById("log").value = "";
});
