// The session log window: the lines the backend kept, then whatever arrives while it is open.
// Nothing is stored here — this window can be closed and opened again mid-session.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

/** Matches the backend's own cap, so this never holds more than it was sent. */
const MAX_LINES = 500;

let lines = [];

function render() {
  const log = $("log");
  // Only follow the tail when the reader is already at it; scrolling up to read must hold.
  const atEnd = log.scrollHeight - log.scrollTop - log.clientHeight < 24;
  log.textContent = lines.join("\n");
  $("log-count").textContent = lines.length
    ? `${lines.length} line${lines.length === 1 ? "" : "s"}`
    : "No lines yet.";
  if (atEnd) log.scrollTop = log.scrollHeight;
}

async function init() {
  $("btn-copy").addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(lines.join("\n"));
      $("btn-copy").textContent = "Copied";
      setTimeout(() => ($("btn-copy").textContent = "Copy"), 1200);
    } catch {
      // No clipboard: the text is selectable, which is the fallback anyone reaches for.
    }
  });
  // What happened before this window opened, then what happens after. The listener goes on
  // first and holds what arrives while the backlog is in flight: taking the backlog as the
  // whole truth afterwards would drop whatever the session logged in between.
  let backlog = null;
  const waiting = [];
  listen("play://log", (event) => {
    const line = String(event.payload);
    if (backlog === null) {
      waiting.push(line);
      return;
    }
    lines = [...lines, line].slice(-MAX_LINES);
    render();
  });
  // A failure here is worth saying: an empty window is indistinguishable from a quiet session,
  // and that is how a broken fetch hid once already.
  try {
    backlog = (await invoke("play_log")) || [];
  } catch (e) {
    backlog = [`AP64 could not read the session log: ${e}`];
  }
  // A line logged as the backlog was taken can appear in both; twice in a log costs nothing,
  // missing costs the thing this window is for.
  lines = [...backlog, ...waiting].slice(-MAX_LINES);
  render();
}

init();
