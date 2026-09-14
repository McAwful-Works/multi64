/**
 * User-facing text shared by the main window and the upload picker: friendly error messages when
 * Rust/Tauri returns stack-y or internal-looking strings, and counted nouns ("1 file", "3 files").
 * @typedef {"cart" | "pc" | "general"} UserErrorContext
 */

/**
 * "Press F5" only where F5 refreshes: the Cart and This PC panes of the main window. `general` is
 * also used by the upload picker and by dialogs, where F5 does nothing.
 */
const FALLBACK_BY_CONTEXT = {
  cart: "Couldn't refresh the cart. Check the USB connection and try again, or press F5.",
  pc: "Couldn't refresh this folder. Try again, or press F5.",
  general: "Something went wrong. Try again.",
};

/**
 * A count with its noun: `countNoun(1, "file")` is "1 file", `countNoun(3, "file")` is "3 files".
 * Use "item" when files and folders may be mixed. Never "file(s)".
 * @param {number} n
 * @param {string} singular
 * @param {string} [plural] defaults to `singular + "s"`
 * @returns {string}
 */
export function countNoun(n, singular, plural = `${singular}s`) {
  return `${n} ${n === 1 ? singular : plural}`;
}

/** @param {unknown} err */
function errorToString(err) {
  if (err == null) return "";
  if (typeof err === "string") return err;
  if (typeof err === "object" && err !== null && "message" in err && err.message != null) {
    return String(err.message);
  }
  return String(err);
}

/**
 * Stack traces, Rust module paths, and other log-style noise.
 * @param {string} s
 */
function isLikelyTechnicalError(s) {
  if (s.length > 480) return true;
  /* Tauri internal (e.g. managed state not ready) */
  if (/\bstate not managed\b/i.test(s)) return true;
  if (/\.rs:\d+/.test(s)) return true;
  if (/\b[a-z_][a-z0-9_]*::[a-z_]/i.test(s)) return true;
  if (
    /\b(panicked|unwrap failed|expect failed|JoinError|spawn_blocking|backtrace|serde_json:|tauri::|std::io::Error)\b/i.test(
      s,
    )
  ) {
    return true;
  }
  if (/\n\s+at\s/.test(s)) return true;
  if (/\\src\\|\\cargo\\registry\\/i.test(s)) return true;
  return false;
}

/**
 * @param {unknown} err
 * @param {{ context?: UserErrorContext }} [opts]
 * @returns {string}
 */
export function userFacingErrorMessage(err, opts = {}) {
  const raw = errorToString(err).trim();
  const ctx = opts.context || "general";
  const fallback = FALLBACK_BY_CONTEXT[ctx] || FALLBACK_BY_CONTEXT.general;
  if (!raw) return fallback;
  if (raw.includes("Cancelled")) return raw;
  if (isLikelyTechnicalError(raw)) return fallback;
  return raw;
}
