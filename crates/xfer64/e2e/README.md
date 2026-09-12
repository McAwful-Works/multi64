# Xfer64 frontend checks (headless)

Drag and drop is the one part of Xfer64 with no Rust test to cover it: the gestures live in
[`../src/explorer.js`](../src/explorer.js), and the thing they drive is an OS the CI runners
cannot rehearse. These checks cover the half that *is* ours — **which backend command each gesture
reaches, and with what arguments**.

`../src/index.html` is served as-is to headless Chromium with `window.__TAURI__` replaced by a stub
that records every `invoke`. So a drag is judged by the calls it produces: no cart, no COM port, no
Tauri. A drop on the SD card pane has to reach `build_cart_import_plan` with the hovered folder as
its parent; the first cart drag-out has to open a staging directory and *not* start an OS drag; the
second has to drag the staged copy and not export again.

```sh
npm install
npx playwright install chromium   # once per machine
npm test
```

`XFER64_E2E_CHROMIUM=/path/to/chrome` overrides the browser, for environments that already have one
where `playwright install` did not put it.

## What this cannot tell you

Everything the OS owns. Whether Windows accepts the drag we hand it, whether `tauri://drag-*` fires
at all, whether a real Explorer drop carries the paths we expect, whether the staged export survives
a 64 MB ROM over serial. **Green here means the frontend wiring holds, not that the feature works.**
That still needs a Windows box and a cart.

## Keeping it running

The stub answers the commands a boot and a copy need; anything else resolves to `null` rather than
throwing, so a command added elsewhere in the app will not break these checks. Only a command the
checks assert on has to be kept in step — if you rename one, this file is where it shows up.
