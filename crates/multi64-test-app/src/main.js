// Multi64 Test — drives multi64_test_connector::suite through Tauri commands and fills the page in
// as each check lands. All the checking lives in Rust; this file only renders.
//
// The page's structure is built once, at load, and never grows: every phase section exists before
// the first run, the tally is always on screen, and a run only changes the rows inside those
// sections. Chrome that appears part-way through a run moves everything below it, which is
// exactly when someone is trying to read a result.

const invoke = () => window.__TAURI__.core.invoke;
const listen = () => window.__TAURI__.event.listen;

const el = (id) => document.getElementById(id);

const ui = {
  port: el("port"),
  expectedRom: el("expectedRom"),
  baseUrl: el("baseUrl"),
  wsUrl: el("wsUrl"),
  skipSerial: el("skipSerial"),
  run: el("run"),
  copy: el("copy"),
  status: el("status"),
  nPass: el("nPass"),
  nFail: el("nFail"),
  nSkip: el("nSkip"),
  results: el("results"),
};

/** Every result of the current run, in order, for the report the tester sends back. */
let collected = [];
/** Phase name -> the <ul> its rows go into, built once at load. */
const phaseLists = new Map();
const tally = { passed: 0, failed: 0, skipped: 0 };

function setStatus(text, isError = false) {
  ui.status.textContent = text;
  ui.status.classList.toggle("hint-error", isError);
}

function outcomeOf(result) {
  // Serde tags the enum with "kind"; the payload field differs per variant.
  const kind = result.outcome.kind;
  if (kind === "pass") return { mark: "PASS", cls: "mark-pass", detail: result.note || "" };
  if (kind === "fail") return { mark: "FAIL", cls: "mark-fail", detail: result.outcome.detail };
  return { mark: "SKIP", cls: "mark-skip", detail: result.outcome.reason };
}

/** Build one section per phase, in the order the suite reports them. Called once. */
function buildSections(phases) {
  ui.results.replaceChildren();
  phaseLists.clear();
  for (const phase of phases) {
    const h = document.createElement("h2");
    h.className = "phase";
    h.textContent = phase;

    const ul = document.createElement("ul");
    ul.className = "checks";
    ul.dataset.phase = phase;

    ui.results.append(h, ul);
    phaseLists.set(phase, ul);
  }
}

/**
 * The list a result belongs in. A phase the page did not know about still gets somewhere to go,
 * rather than the result vanishing: the Rust side owns the phase list, and a mismatch should be
 * visible rather than silently dropped.
 */
function listFor(phase) {
  let ul = phaseLists.get(phase);
  if (!ul) {
    const h = document.createElement("h2");
    h.className = "phase";
    h.textContent = phase;
    ul = document.createElement("ul");
    ul.className = "checks";
    ul.dataset.phase = phase;
    ui.results.append(h, ul);
    phaseLists.set(phase, ul);
  }
  return ul;
}

function showTally() {
  ui.nPass.textContent = tally.passed;
  ui.nFail.textContent = tally.failed;
  ui.nSkip.textContent = tally.skipped;
}

function addResult(result) {
  collected.push(result);

  const { mark, cls, detail } = outcomeOf(result);
  if (mark === "PASS") tally.passed += 1;
  else if (mark === "FAIL") tally.failed += 1;
  else tally.skipped += 1;
  showTally();

  const li = document.createElement("li");

  const m = document.createElement("span");
  m.className = `mark ${cls}`;
  m.textContent = mark;

  const name = document.createElement("span");
  name.textContent = result.name;
  if (detail) {
    const d = document.createElement("span");
    d.className = "detail";
    d.textContent = detail;
    name.append(d);
  }

  const ms = document.createElement("span");
  ms.className = "ms";
  // Checks that never touched the wire report 0 ms; showing "0 ms" would suggest they ran fast
  // rather than not at all.
  ms.textContent = result.millis > 0 ? `${result.millis} ms` : "";

  li.append(m, name, ms);
  listFor(result.phase).append(li);
  li.scrollIntoView({ block: "nearest" });
}

function reportText() {
  const lines = [
    `Multi64 test run — ${new Date().toISOString()}`,
    `daemon ${ui.baseUrl.value}   port ${ui.port.value}   expected ROM ${
      ui.expectedRom.value || "(not asserted)"
    }`,
    "",
  ];
  let phase = null;
  for (const r of collected) {
    if (r.phase !== phase) {
      phase = r.phase;
      lines.push(`== ${phase} ==`);
    }
    const { mark, detail } = outcomeOf(r);
    lines.push(`${mark}  ${r.name}${detail ? `  ${detail}` : ""}`);
  }
  lines.push(
    "",
    `${tally.passed} passed, ${tally.failed} failed, ${tally.skipped} skipped`,
  );
  return lines.join("\n");
}

async function runSuite() {
  collected = [];
  tally.passed = 0;
  tally.failed = 0;
  tally.skipped = 0;
  showTally();
  // Empty the rows, keep the sections: the structure is the page's, not the run's.
  for (const ul of phaseLists.values()) ul.replaceChildren();

  ui.run.disabled = true;
  ui.copy.disabled = true;
  setStatus("Running. Do not touch the controller.");

  try {
    const summary = await invoke()("run", {
      wsUrl: ui.wsUrl.value.trim(),
      baseUrl: ui.baseUrl.value.trim(),
      port: ui.port.value.trim(),
      expectedRom: ui.expectedRom.value.trim(),
      skipSerial: ui.skipSerial.checked,
    });
    // The summary is authoritative; the running tally should already agree, and disagreeing would
    // mean a result never reached the page.
    tally.passed = summary.passed;
    tally.failed = summary.failed;
    tally.skipped = summary.skipped;
    showTally();
    setStatus(
      summary.failed === 0
        ? "All checks passed."
        : `${summary.failed} check${summary.failed === 1 ? "" : "s"} failed.`,
      summary.failed !== 0,
    );
  } catch (e) {
    // The suite returns Err only when the run could not start at all, which is a different thing
    // from a failing check and is worth saying differently.
    setStatus(`Could not start: ${e}`, true);
  } finally {
    ui.run.disabled = false;
    ui.copy.disabled = collected.length === 0;
  }
}

/**
 * Fill the serial port in from the running daemon rather than leaving a compiled-in guess.
 *
 * The default is only a default: on a machine whose cart is not on that port, running with it
 * would mean the direct-serial checks open some other device. The daemon knows which port it was
 * told to use, so ask it.
 */
async function probeDaemon() {
  const base = ui.baseUrl.value.trim();
  if (!base) return;
  try {
    const info = await invoke()("probe_daemon", { baseUrl: base });
    if (info.serial) ui.port.value = info.serial;
    const held = info.serialActive ? "holding the port" : "not holding the port yet";
    setStatus(`Multi64 is on ${info.serial || "an unnamed port"} (${info.cart}), ${held}.`);
  } catch {
    // Not an error worth colouring red: the daemon may simply not be running yet, and the run
    // itself will say so far more clearly than a field would.
    setStatus(
      `No daemon answering at ${base}. Start Multi64, then press Run — the port below is a default, not a detection.`,
    );
  }
}

async function main() {
  const d = await invoke()("defaults");
  ui.wsUrl.value = d.wsUrl;
  ui.baseUrl.value = d.baseUrl;
  ui.port.value = d.port;
  ui.expectedRom.value = d.expectedRom;

  buildSections(d.phases);
  showTally();

  if (!d.expectedRom) {
    setStatus(
      "Ready. No expected ROM version was built in, so that check will be skipped rather than passed.",
    );
  }
  await probeDaemon();
  ui.baseUrl.addEventListener("change", probeDaemon);

  await listen()("suite-check", (e) => addResult(e.payload));

  ui.run.addEventListener("click", runSuite);
  ui.copy.addEventListener("click", async () => {
    await navigator.clipboard.writeText(reportText());
    setStatus("Report copied to the clipboard.");
  });
}

main();
