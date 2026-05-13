function invoke(name, args) {
  const core = window.__TAURI__?.core;
  if (!core?.invoke) {
    return Promise.reject(new Error("Tauri IPC not ready"));
  }
  return core.invoke(name, args);
}

async function refreshLog() {
  const el = document.getElementById("dev-shell-log");
  try {
    const lines = await invoke("explorer_dev_log_get");
    const arr = Array.isArray(lines) ? lines : [];
    el.textContent = arr.length
      ? arr.join("\n")
      : "(Log empty — enable Developer mode in Settings and use the cart.)";
  } catch (e) {
    el.textContent = `Failed to load log: ${e}`;
  }
}

function stripAnsi(s) {
  return String(s).replace(/\u001b\[[\d;]*[A-Za-z]/g, "");
}

/** @param {string} text */
async function writeTextToClipboard(text) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.left = "-9999px";
  document.body.appendChild(ta);
  ta.select();
  document.execCommand("copy");
  document.body.removeChild(ta);
}

async function copyLogToClipboard() {
  const el = document.getElementById("dev-shell-log");
  const btn = document.getElementById("dev-shell-copy");
  const text = el?.textContent ?? "";
  const label = "Copy";
  try {
    await writeTextToClipboard(text);
    if (btn) {
      btn.textContent = "Copied";
      setTimeout(() => {
        if (btn) btn.textContent = label;
      }, 1500);
    }
  } catch {
    if (btn) {
      btn.textContent = "Copy failed";
      setTimeout(() => {
        if (btn) btn.textContent = label;
      }, 2000);
    }
  }
}

async function init() {
  await refreshLog();

  if (window.__TAURI__?.event?.listen) {
    await window.__TAURI__.event.listen("explorer-dev-log", (e) => {
      const el = document.getElementById("dev-shell-log");
      const line =
        typeof e.payload === "string"
          ? e.payload
          : stripAnsi(JSON.stringify(e.payload));
      if (el.textContent && !el.textContent.startsWith("(")) {
        el.textContent += "\n" + line;
      } else {
        el.textContent = line;
      }
      el.scrollTop = el.scrollHeight;
    });
  }

  document.getElementById("dev-shell-refresh").addEventListener("click", () => {
    void refreshLog();
  });
  document.getElementById("dev-shell-copy").addEventListener("click", () => {
    void copyLogToClipboard();
  });
  document.getElementById("dev-shell-clear").addEventListener("click", async () => {
    await invoke("explorer_dev_log_clear");
    await refreshLog();
  });
  document.getElementById("dev-shell-close").addEventListener("click", async () => {
    try {
      await invoke("explorer_close_dev_shell");
    } catch (e) {
      document.getElementById("dev-shell-log").textContent += `\n[invoke error] ${e}`;
    }
  });
}

window.addEventListener("DOMContentLoaded", () => {
  void init();
});
