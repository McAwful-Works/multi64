/**
 * Human-friendly error text when Rust/Tauri returns stack-y or internal-looking strings.
 * @typedef {"cart" | "pc" | "general"} UserErrorContext
 */

const FALLBACK_BY_CONTEXT = {
  cart:
    "Couldn't refresh the SD card. Check the USB connection and try again, or press F5.",
  pc: "Couldn't refresh this folder. Try again, or press F5.",
  general: "Something went wrong. Try again, or press F5.",
};

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
