# Multi64 frontend checks (headless)

Multi64's window is plain JS in [`../src/main.js`](../src/main.js), and no Rust test reaches it. These
checks cover the part that is the frontend's own: what the window shows for what the backend
answers, and what it sends back.

`../src/index.html` is served as-is to headless Chromium with `window.__TAURI__` replaced by a stub
that records every `invoke` and answers from a small table. No bridge, no serial port, no Tauri.

What they check:

- **The Status card:** a running bridge shows its cart and serial port (`SummerCart64 on COM3`), and
  only the action that applies (Start or Stop) is enabled. Health reads **OK** while the bridge holds
  the cart and **No cart link** when it answers without it, with the backend's reason in Note.
- **A start in progress:** after a Start click, and whenever the backend reports a start under way,
  the card reads **Starting…** and offers no Start, through a status poll too, so a second click
  never reaches `daemon_start`; Start comes back once the start has answered.
- **Settings → Cart:** the four options, the saved cart selected on open, and the experimental warning
  under a chosen EverDrive.
- **Settings → Serial port:** Auto-detect first, then the ports the backend listed.
- **An unplugged saved port (#168):** it stays listed as `COMx (not connected)` and stays selected
  through opening Settings, the status poll's port refresh, Save (which writes it back rather than
  Auto-detect), opening Settings again, and Refresh ports.
- **The window fits the page:** the height the page asks for (`fit_window_height`) is the page's own,
  grows by the Developer card in Developer mode, is at least 640 px while a dialog is open and
  shrinks back when it closes, and a long log scrolls inside its box rather than growing it.
- **Settings → Listen:** a warning under the field for a saved or typed address other PCs could
  reach (a LAN address, a wildcard, a host name other than `localhost`), none for loopback, and Save
  still saves it.
- **A Save whose restart fails:** Settings closes, and the reason `set_settings` gave shows under the
  Status card rather than only in the log; a Save with nothing to report shows nothing there.
- **No console errors** from any of the pages above.

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
