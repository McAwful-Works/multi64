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

The rule is checked by a test, so it holds whether or not anyone remembers it:

```sh
cargo test -p multi64 no_colour_literals_outside_the_palette
```

It reads every `.css` file under the `src/` of each app in `SHARED_BASE_APPS` — Multi64, Xfer64 and
Multi64 Test (`crates/multi64-test-app`) — and fails on any
declaration outside a `:root` palette block whose value names a colour: a hex literal, a colour
function called with numbers rather than a token (`rgba(138, 180, 248, 0.12)` is reported,
`rgba(var(--accent-rgb), 0.12)` is not), or one of the CSS named colours. `transparent` and
`currentColor` take their colour from the surface, so they are allowed. Each hit is reported with
its file, line and rule. The test sits beside the two that keep the shared files identical (§3, §5).

`crates/multi64-test-connector-gui/src/styles.css` is deliberately outside all of this: that window
has a single hardcoded palette, no theme switch and no `appearance.js`.

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

One script, **duplicated verbatim in every app that carries the shared base** — `crates/multi64/src/`, `crates/xfer64/src/` and `crates/multi64-test-app/src/`.
There is no shared frontend directory: `frontendDist` points at each app's own `src`, so a file
cannot be referenced across crates. **Edit one, copy to the other.** `appearance_js_is_identical_in_both_apps` in the `multi64` crate fails when they differ.

Three constraints, each of which has already caused a bug:

- **It must not import Tauri APIs.** Each app's main script begins with
  `const { invoke } = window.__TAURI__.core`, which throws outside the app shell and kills the
  whole file. Appearance binding lived there once and silently did nothing in a browser.
  Keeping this module Tauri-free is also what makes it testable without building the app.
- **It must load non-deferred in `<head>`,** ahead of the app scripts, as a classic `<script>`: `type="module"` is always deferred, however it is placed. It applies the stored theme
  before the first paint; deferring it produces a visible flash of the wrong theme.
- **Preferences live in `localStorage`, not the settings file.** They must be readable
  *synchronously* at load, and a Tauri `invoke` is async. `readAppearance` treats a throw as
  "use defaults" — private windows and cleared site data both throw.

Values are validated against `THEMES`, `MOTION` and `SCALES` on both read and write, so a hand-edited
or stale stored value degrades to the default rather than applying an unknown `data-theme`.

## 4. Verifying a change

Neither CI nor `check-docs` covers any of this — the §1 test reads the CSS but renders nothing, no
job runs `tauri build`, and the CSS, HTML and JS are served as-is. Two browser checks carry the
weight, both run by serving `crates/<app>/src` over plain HTTP and driving it:

- **Computed-style diff.** Walk the DOM recording each element's resolved colour properties, before
  and after. A refactor that is meant to change no rendering must produce a byte-identical
  snapshot; this is how the tokenisation of ~99 literals was shown faithful across 426 elements.
- **Contrast audit.** For every visible text node, compare its computed colour against the nearest
  opaque ancestor background and require 4.5:1 (3:1 for large text). Run it against **each** theme.
  This found six real AA failures in the first light palette that reading the CSS did not.

Note that a hidden browser pane does not lay out, so measurements taken while it is hidden are
meaningless — element sizes come back unchanged no matter what you set.

## 5. The shared base: `styles.css` and the size tokens

`crates/multi64/src/styles.css`, `crates/xfer64/src/styles.css` and `crates/multi64-test-app/src/styles.css` are the **same file**. `shared_styles_css_is_identical_in_both_apps` in the `multi64` crate fails when any of them differ, so edit one and copy it to the others. It holds everything the apps must agree on:

- **The palette** (§1–2).
- **The scale tokens:** `--font-xs`, `--font-sm`, `--font-md`, `--font-lg` and `--font-title` for text; `--radius-sm`, `--radius-md` and `--radius-lg` for corners; `--z-dialog` and `--z-dialog-top` for stacking.
- **The single focus ring, the reduced-motion rules and the `[hidden]` rule.**
- **The shared components:**
  - buttons: `.btn`, `.btn.primary`, `.btn-icon`;
  - fields: `.field`, `.input`, `.check`, `.hint`;
  - the header brand block: `.app-brand…`, `.app-actions`;
  - the dialog used by Settings, Help and confirmations: `.dialog…`. A markup sketch sits beside its rules.

App-only rules live in the app's own sheet: Multi64's `app.css`, Xfer64's `explorer.css` (which imports `styles.css`) and `upload-picker.css`, and Multi64 Test's `app.css`.

Three rules keep it that way:

- **No `font-size` or `border-radius` literal in an app sheet.** Use a token. Sizes relative to the parent (`em`) are fine.
- **No component-level focus styles.** The global ring is the only one. Where `overflow` would clip it, adjust `outline-offset` and nothing else.
- **A shared component changes in `styles.css`, in both copies.** Never override it in one app's sheet to make that app look different.

