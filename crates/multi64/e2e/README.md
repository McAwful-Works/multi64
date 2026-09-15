# Multi64 frontend checks (headless)

Multi64's window is plain JS in [`../src/main.js`](../src/main.js), and no Rust test reaches it. These
checks cover the part that is the frontend's own: **what the window shows for what the backend
answers, and what it sends back**.

`../src/index.html` is served as-is to headless Chromium with `window.__TAURI__` replaced by a stub
that records every `invoke` and answers from a small table. No bridge, no serial port, no Tauri.

What they check:

- **The Status card:** a running bridge shows its cart and serial port (`SummerCart64 on COM3`), and
  only the action that applies (Start or Stop) is enabled.
- **Settings → Cart:** the four options, the saved cart selected on open, and the experimental warning
  under a chosen EverDrive.
- **Settings → Serial port:** Auto-detect first, then the ports the backend listed.
- **An unplugged saved port (#168):** it stays listed as `COMx (not connected)` and stays selected
  through opening Settings, the status poll's port refresh, Save (which writes it back rather than
  Auto-detect), opening Settings again, and Refresh ports.

```sh
npm ci
npx playwright install --with-deps chromium   # once per machine
npm test
```

`MULTI64_E2E_CHROMIUM=/path/to/chrome` overrides the browser, for environments that already have one
where `playwright install` did not put it.

## What this cannot tell you

Everything the backend owns. Whether the ports listed are the ports plugged in, whether Save reaches
the settings file, whether the bridge starts, and how WebView2 renders any of it. **Green here means
the frontend wiring holds, not that the feature works.** The Rust tests in `../src-tauri` cover the
backend's side, and the rest still needs Windows and a cart.

## Keeping it running

The stub answers the commands the window uses at startup and in Settings; anything else resolves to
`null` rather than throwing, so a command added elsewhere will not break these checks. Only a command
the checks assert on has to be kept in step. If you rename one, this file is where it shows up.
