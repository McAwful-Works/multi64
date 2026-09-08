# Frontend appearance: themes, tokens and preferences

How Multi64 and Xfer64 style themselves, and the constraints that are easy to break. User-facing
behaviour is in each app's README; this is the maintainer view.

---

## 1. Every colour is a token

`:root` in each app's `styles.css` holds the whole palette — about 30 custom properties. **No rule
outside `:root` may contain a colour literal.** A hardcoded hex or `rgba()` is invisible to the
theme switch, so it survives into Light and High contrast unchanged and usually becomes
unreadable there.

Hues used at more than one alpha are stored as **RGB triples**, with the alpha at the call site:

```css
/* right */  background: rgba(var(--accent-rgb), 0.12);
/* wrong */  background: rgba(138, 180, 248, 0.12);
```

`--accent-rgb`, `--danger-rgb`, `--success-rgb`, `--warning-rgb`, `--shadow-rgb` and
`--highlight-rgb` exist for this. `--highlight-rgb` is white in dark themes and **black in light** —
a hairline that lightens a dark surface must darken a light one.

To check the rule still holds, look for literals outside `:root`:

```sh
grep -nE '(#[0-9a-fA-F]{3,8}\b|rgba?\((?!var)[0-9])' crates/*/src/*.css
```

The only expected hits are `var(--muted, #9aa0a6)` fallbacks in `explorer.css`, which are dead
(`--muted` is always defined) and harmless.

### A foreground that does not follow the surface needs its own token

`--control-primary-text` exists because primary buttons keep a saturated background in every
theme. Using `--text` on one works in dark by accident — near-white on dark blue — and fails in
light, where `--text` flips to near-black over the same blue (2.5:1, against a 4.5:1 floor). Any
new element with a background that does not track the surface needs the same treatment.

## 2. Theme structure

Themes set `data-theme` on `<html>`; **no rule selects on it except the palette blocks.** Everything
else reads tokens and is theme-agnostic.

| `data-theme` | Palette |
|--------------|---------|
| absent / `dark` | the base `:root` block |
| `light` | `:root[data-theme="light"]` |
| `system` | base, plus the light block under `@media (prefers-color-scheme: light)` |
| `contrast` | `:root[data-theme="contrast"]` |

Dark needs no block of its own: it *is* the base, and every other theme overrides from there. A
token added to `:root` but not to the light and contrast blocks silently inherits the dark value —
which is the most likely way to reintroduce a contrast failure.

## 3. `appearance.js`

One module, **duplicated verbatim in both apps** (`crates/multi64/src/` and `crates/xfer64/src/`).
There is no shared frontend directory: `frontendDist` points at each app's own `src`, so a file
cannot be referenced across crates. **Edit one, copy to the other.** Nothing enforces this.

Three constraints, each of which has already caused a bug:

- **It must not import Tauri APIs.** Each app's main script begins with
  `const { invoke } = window.__TAURI__.core`, which throws outside the app shell and kills the
  whole file. Appearance binding lived there once and silently did nothing in a browser.
  Keeping this module Tauri-free is also what makes it testable without building the app.
- **It must load non-deferred in `<head>`,** ahead of the app scripts. It applies the stored theme
  before the first paint; deferring it produces a visible flash of the wrong theme.
- **Preferences live in `localStorage`, not the settings file.** They must be readable
  *synchronously* at load, and a Tauri `invoke` is async. `readAppearance` treats a throw as
  "use defaults" — private windows and cleared site data both throw.

Values are validated against `THEMES`, `MOTION` and `SCALES` on both read and write, so a hand-edited
or stale stored value degrades to the default rather than applying an unknown `data-theme`.

## 4. Verifying a change

Neither CI nor `check-docs` covers any of this — no job runs `tauri build`, and the CSS, HTML and JS
are served as-is. Two browser checks carry the weight, both run by serving `crates/<app>/src` over
plain HTTP and driving it:

- **Computed-style diff.** Walk the DOM recording each element's resolved colour properties, before
  and after. A refactor that is meant to change no rendering must produce a byte-identical
  snapshot; this is how the tokenisation of ~99 literals was shown faithful across 426 elements.
- **Contrast audit.** For every visible text node, compare its computed colour against the nearest
  opaque ancestor background and require 4.5:1 (3:1 for large text). Run it against **each** theme.
  This found six real AA failures in the first light palette that reading the CSS did not.

Note that a hidden browser pane does not lay out, so measurements taken while it is hidden are
meaningless — element sizes come back unchanged no matter what you set.
