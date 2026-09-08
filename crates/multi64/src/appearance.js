/**
 * Appearance preferences: theme, UI scale, motion.
 *
 * Applied to <html> as data attributes and a custom property; all the actual styling lives in
 * styles.css. Runs before first paint (loaded non-deferred in <head>) so the window never shows a
 * flash of the wrong theme when the stored preference is not the default.
 *
 * Persistence is localStorage rather than the app's settings file: this must be readable
 * synchronously at load, and a Tauri `invoke` is async. The settings file remains the source of
 * truth across installs; the app mirrors it here whenever it changes.
 */
const KEY = "multi64.appearance";

/** Values accepted for each preference; anything else falls back to the first entry. */
export const THEMES = ["dark", "light", "system", "contrast"];
export const MOTION = ["system", "reduced"];
export const SCALES = [0.9, 1, 1.15, 1.3];

// "system" by default: an accessibility feature should follow the OS unless told otherwise.
// Existing installs are unaffected in practice -- a stored preference always wins, and a user
// on a dark OS sees no change either way.
const DEFAULTS = { theme: "system", motion: "system", scale: 1 };

function coerce(raw) {
  const p = raw && typeof raw === "object" ? raw : {};
  return {
    theme: THEMES.includes(p.theme) ? p.theme : DEFAULTS.theme,
    motion: MOTION.includes(p.motion) ? p.motion : DEFAULTS.motion,
    // Compare numerically: a value round-tripped through JSON may be a string.
    scale: SCALES.includes(Number(p.scale)) ? Number(p.scale) : DEFAULTS.scale,
  };
}

export function readAppearance() {
  try {
    return coerce(JSON.parse(localStorage.getItem(KEY)));
  } catch {
    // Private windows and cleared site data both throw; defaults are the right answer.
    return { ...DEFAULTS };
  }
}

export function applyAppearance(prefs) {
  const p = coerce(prefs);
  const root = document.documentElement;
  root.setAttribute("data-theme", p.theme);
  root.setAttribute("data-motion", p.motion);
  root.style.setProperty("--ui-scale", String(p.scale));
  return p;
}

export function saveAppearance(prefs) {
  const p = coerce(prefs);
  try {
    localStorage.setItem(KEY, JSON.stringify(p));
  } catch {
    // Not fatal: the preference still applies for this session.
  }
  return applyAppearance(p);
}

// Apply immediately on import so there is no unstyled or wrong-themed first paint.
applyAppearance(readAppearance());

/**
 * Bind the settings controls, if the page has them.
 *
 * This lives here rather than in each app's main script because those import Tauri APIs at load
 * and cannot run outside the app shell — which would make appearance untestable in a browser and
 * would need the same code written twice. This module has no Tauri dependency, so both apps get
 * the behaviour by loading it and adding the three controls to their markup.
 */
function bindAppearanceControls() {
  const fields = [
    ["pref-theme", "theme", (v) => v],
    ["pref-scale", "scale", (v) => Number(v)],
    ["pref-motion", "motion", (v) => v],
  ];
  const current = readAppearance();
  for (const [id, key, parse] of fields) {
    const el = document.getElementById(id);
    if (!el) continue;
    el.value = String(current[key]);
    el.addEventListener("change", () => {
      // Re-read rather than closing over `current`: another control may have changed since load.
      saveAppearance({ ...readAppearance(), [key]: parse(el.value) });
    });
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", bindAppearanceControls);
} else {
  bindAppearanceControls();
}
