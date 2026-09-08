# Installer images

Regenerates the Windows installer bitmaps for Multi64 and Xfer64 from the brand masters
(`branding/multi64.svg`, `branding/xfer64.svg`) into each app's `src-tauri/windows/`.

| File | Size | Used by |
|------|------|---------|
| `nsis-header.bmp`  | 150×57  | NSIS `headerImage` — strip across the top of the wizard |
| `nsis-sidebar.bmp` | 164×314 | NSIS `sidebarImage` — left panel of the welcome/finish pages |
| `wix-banner.bmp`   | 493×58  | WiX `bannerPath` — top of the MSI dialogs |
| `wix-dialog.bmp`   | 493×312 | WiX `dialogImagePath` — MSI welcome/exit background |

## Why a browser

Neither ImageMagick, Inkscape, rsvg nor a Python SVG library is available on the machine this was
built on, and adding a native image dependency for four static files is not worth it. So the
browser rasterises: `gen.html` draws each SVG onto a canvas at the target size over the backdrop,
and POSTs the raw RGBA to `bmpgen.js`, which writes a 24-bit uncompressed BMP. No image library on
either side — BMP is simple enough to emit directly.

## Running it

```sh
node branding/installer-images/bmpgen.js <output-dir>
```

Then open <http://127.0.0.1:8792/> and wait for "done: 8 files". Copy each `<app>-<kind>.bmp` to
`crates/<app>/src-tauri/windows/<kind>.bmp`. The page renders every canvas on screen, so the
placements can be checked before anything is copied in.

## Placement

The backdrop is `#14161c`, matching the apps' dark UI so the installer and the app read as
continuous. BMPs cannot carry transparency, which is why a backdrop is needed at all.

The mark is right-aligned on the two wide strips and centred on the sidebar. On `wix-dialog` it
sits in the **left ~164px band**: WixUI draws the welcome text over the right of that bitmap, so
anything centred there would end up behind the text.
