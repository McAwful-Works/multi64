# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

CI's Rust job (`.github/workflows/ci.yml`, on Ubuntu and Windows) is these six steps, in this order — run them before proposing a change is done:

```sh
cargo fmt --all -- --check
cargo build -p multi64d                                    # see below: not optional
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p multi64d --release
cargo build --workspace --release
```

The `multi64d` builds are not optional and are not a packaging step — anything that compiles `crates/multi64` needs `resources/multi64d.exe` to exist, so on a clean clone `cargo clippy --workspace` fails without them. Build it once per profile, which is what CI does.

Clippy runs with `-D warnings`, so an unused import or a stray `mut` fails CI the same as a type error.

CI runs four more jobs, all on Ubuntu only. The first is the Xfer64 frontend checks, which nothing above covers:

```sh
cd crates/xfer64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

These drive `crates/xfer64/src/index.html` in headless Chromium with `window.__TAURI__` stubbed, and
assert which backend command each drag gesture reaches. They prove the frontend wiring, not the
feature: everything the OS owns (whether Windows accepts a drag, whether `tauri://drag-*` fires) is
still Windows-and-a-cart territory. See [`crates/xfer64/e2e/README.md`](crates/xfer64/e2e/README.md).

The Multi64 frontend checks are a job of the same kind:

```sh
cd crates/multi64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

They drive `crates/multi64/src/index.html` the same way and check the Status card and the Settings
Cart and Serial port selects, including a saved port that is unplugged. See
[`crates/multi64/e2e/README.md`](crates/multi64/e2e/README.md).

AP64's frontend checks are a third job of the same kind, for its Patch and Play cards:

```sh
cd crates/ap64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

And the last — the cart agent's host tests, the only CI job that compiles N64 code
(for the PC, not the console). It needs a host `gcc` or `clang` and `make`, nothing from the N64 toolchain:

```sh
make -C n64/agent host-test
```

All of it runs under ASan and UBSan, in three parts:

- It builds `n64/agent/agent.c` once per EverDrive cart against a fake driver, and checks that L3 frames arriving in pieces are reassembled and that a lost piece is never spliced into the next request.
- It builds the real SC64 driver, `n64/agent/sc64.c`, against a fake cart and bus (`tests/sc64_test.c`, `SC64_HOST_TEST`), and checks that every bus wait is bounded and every failed register write is reported.
- It builds the real X7 driver, `n64/agent/ed64.c`, with fake `pi_io_*` functions in place of `pi_io.c` (`tests/ed64_test.c`), and checks that a received `DMA@` message is delivered in order, including the part that did not fit the space offered, or reported lost; that a send reads everything the host has sent first, since an X7 will not finish a write while host data waits unread; and that every wait is bounded. This driver has still never run on a cart: the one X7 run so far used the test ROM, which goes through libdragon instead.

No part of it shows that a driver works on a cart.

Targeted testing:

```sh
cargo test -p multi64-sc64-sd                  # one crate
cargo test -p multi64-l3 stream_decoder_resync  # one test by substring
cargo test -p multi64-sc64-sd -- --exact partition::tests::detect_partition_legacy_mbr_first_lba
```

Tests live in `crates/{l3,sc64-link,sc64-l2,ed64-l2,ed64pro-link,ed64pro-l2,cart-probe,multi64-sc64-sd,multi64-ed64-link,ed64-smoke,xfer64,multi64d,multi64,sc64-sd-e2e,ap64-core,ap64-cart,ap64-connector}` — note `multi64d` has an HTTP integration suite in `tests/http.rs` (origin guard, faulted `serialActive`, resume), and `crates/multi64/src-tauri` unit-tests the settings and tray-menu label logic. Everything in CI is host-only — the SD/FAT logic is covered by RAM-disk tests, and no test touches hardware.

### Tauri apps (`multi64`, `xfer64`, `multi64-test-app`, `ap64`, `multi64-test-connector-gui`)

`npm install` inside `crates/<app>/` first — it supplies the Tauri CLI. `npm run build` maps to `tauri build`. `frontendDist` points at `../src`, so the frontend is plain JS served as-is; there is no bundler output step to run.

Styling is tokenised: `:root` in each app's `styles.css` holds the palette and **no rule outside it may contain a colour literal**, or the change survives unchanged into the Light and High-contrast themes. `appearance.js` is duplicated verbatim in every app carrying the shared base — Multi64, Xfer64, Multi64 Test and AP64 — must stay Tauri-free, and must load non-deferred in `<head>`. `styles.css` is duplicated verbatim too: it is the shared base (palette, size tokens, buttons, fields, dialogs), app-only rules go in the app's own sheet, and a test fails if any copy differs. `multi64-test-connector-gui` is deliberately outside all of this. See [`docs/frontend-appearance.md`](docs/frontend-appearance.md) before editing any CSS or frontend JS.

Build `multi64d` **before** anything that compiles the Multi64 Tauri crate, and with the **same** profile. `crates/multi64/src-tauri/build.rs` copies the daemon into `resources/`, and `tauri.conf.json` declares `resources/multi64d.exe` under `bundle.resources`. A declared resource that is missing is a **hard error** in `tauri-build`: the `cargo:warning` from `build.rs` is not the whole story — the build then fails anyway. The copied `resources/multi64d.exe` is gitignored, so this bites a clean checkout running `cargo clippy --workspace` or `cargo build --workspace`, not just packaging.

Multi64, Xfer64 and AP64 are each installed with their own installer; none bundles, installs or launches another. `multi64d.exe` is Multi64's only bundled resource.

### Hardware paths (never in CI)

```sh
cargo run -p multi64d --release -- --serial COM3        # daemon, 127.0.0.1:38765
cargo run -p sc64-smoke -- --port COM5                  # vendor IDENTIFIER/VERSION
cargo run -p sc64-l3-framing-e2e --release -- --port COM3   # needs ROM in RAW_ECHO
```

## Architecture

### Two independent USB stacks over one COM port

This is the single most important thing to understand, because the crate names blur it:

1. **The L3 bridge stack** — `multi64-l3` (framing) → an L2 pipe (`multi64-sc64-l2` / `multi64-ed64-l2`) → `multi64d`, which exposes raw L3 octets over HTTP + WebSocket. This is the project's actual protocol.
2. **The SD/FAT stack** — `multi64-sc64-sd` reads and writes the cart's SD card as a block device and layers FAT32/exFAT on top. Consumed by Xfer64. It shares no framing with L3 at all; it speaks vendor SD commands directly.

A cart exposes one serial device, so the two stacks contend. `multi64d` resolves this with `POST /v1/serial/release` and `POST /v1/serial/resume` (`crates/multi64d/src/lib.rs`): Xfer64 calls release before touching the cart and resume afterward (`crates/xfer64/src-tauri/src/daemon.rs`). WebSocket writes are dropped while released. Any new process that opens the cart port must participate in this handshake.

### L3 is cart-agnostic; L2 is where carts differ

`multi64-l3` owns the `M64B` wire format, `StreamDecoder`, sessions and channels — no serial code. Each cart gets an L2 crate exposing the same conceptual handle (`open`, `write_l3_stream`, `read_l3_bytes`, `set_timeout`, `clear_serial_buffers`). Adding a cart means writing that handle, not touching L3.

**SC64 is the only backend proven on hardware.** `Ed64L2Pipe` implements the EverDrive `DMA@` framing from `docs/spec/l3-over-everdrive-x7.md` §4, derived from UNFLoader and libdragon's `usb.c`, and **has run on one X7**: L3 through `multi64d` worked, but the cart could not send while the host was sending, and cut a message off part-way (§4.5 item 6). One cart is not validation, so the spec stays Draft. Do not describe EverDrive as supported, and do not treat a successful `open` as evidence: the data path has no identity handshake, so `open` only means the serial port opened. §4.5 lists what must be checked on hardware first. `multi64d --cart ed64` selects it (default `sc64`), and Multi64's Settings → **Cart** passes that flag, or on its default **Auto-detect** whichever cart answered `multi64-cart-probe`; that is wiring only and makes EverDrive no more proven.

The **EverDrive-64 PRO** has its own pipe, `Ed64ProL2Pipe` (`multi64-ed64pro-l2`, `multi64d --cart ed64pro`, or Multi64's Settings → **Cart**): L3 written into the cart FIFO and read back raw, per `docs/spec/l3-over-everdrive-pro.md`. It is less proven than the X7 pipe — libdragon and UNFLoader do not support the PRO, so there was no reference to transcribe and the mapping is this repo's own design. Its ROM side is `n64/test-rom/ed64pro.c`, which must detect the PRO before libdragon's `usb_initialize` runs. Its `open` does run an identity handshake, unlike the X7's, but that proves only that a PRO answered.

`multi64-ed64-link` is *not* part of the L3 stack despite the name — it is EverDrive USB serial plumbing for the SD path, plus cart detection.

### SD access layering

`SdCardTransport` (`crates/multi64-sc64-sd/src/link.rs`) is the sector-level seam: SC64 implements it with vendor SD ops, EverDrive with `RomRead` at a configured base address — an experiment that reads cart ROM memory rather than the SD card, so it is not expected to list the card (`docs/spec/ed64-sd-usb-host.md`). `CartSession` sits above it and is what UI code uses (`list_dir`, `copy_cart_entry_to_host_with_progress`, `mkdir_cart`, …). EverDrive support is behind the crate's `ed64` feature, enabled only by `crates/xfer64/src-tauri`. The X-series EverDrive SD path is experimental and read-only. The **EverDrive-64 PRO** is different: its cart serves files, so `Ed64ProSdSession` (feature `ed64pro`) implements `CartSession` at file level with no `SdCardTransport`. It is experimental, never run against a cart, needs the user's consent before writing, and renames by copying then deleting, since its link has no rename command.

## Conventions

- **`main` and `release` are the only long-lived branches.** Everything else is a topic branch — delete it, local and remote, as soon as its PR merges. Dependabot deletes its own.
- **`docs/spec/` is normative.** Wire behavior changes must land with the spec edit in the same change. Bump L3 **Protocol-Major**/**Protocol-Minor** only when the byte contract changes (`l3-bridge-protocol-v1.md` §12); **Spec-Revision** is maintainer-controlled — do not bump it on your own.
- `docs/README.md` is the spec map and states the intended reading order for implementors.
- `hadris-fat` is pinned to a git rev in the workspace `[patch.crates-io]` because the 1.1.0 release fails to build with `--features exfat`. Do not unpin it to resolve a dependency conflict.
- MSRV is 1.80 and edition 2021, set once in `[workspace.package]`.
- Licensing is `MIT OR Apache-2.0`; new crates should inherit `license.workspace = true`.

## Repo tooling

Committed under `.claude/`, so they apply for anyone working on this repo:

- **`/preflight`** — runs the six steps of CI's Rust job in order and reports the first failure. It does not run CI's four Ubuntu-only jobs (the three frontend suites and the agent host test). Not every machine holding this repo has a Rust toolchain; when `cargo` is absent, say the change is unverified rather than implying otherwise.
- **`/check-docs`** — validates markdown links, heading anchors, backtick-wrapped links, and `docs/spec/` paths cited from Rust/JS. Run after any spec rename or file move; nothing in CI covers this.
- **`/implement-ed64-l2`** — the ED64 L2 backend walkthrough. The blocker is `l3-over-everdrive-x7.md` §4, not the code.
- **`/add-game`** — adding a game to AP64: checking its Archipelago world is one the generic connector can drive, building a seed, finding RAM and a hook site by measurement, and verifying a profile before it reaches a console. The ROM side stays normative in `docs/integration/placing-the-agent.md`; the skill is the spine around it, including the Archipelago half.
- **`spec-reviewer`** subagent — reviews a diff against `docs/spec/` as normative. Worth running on changes to `crates/l3`, any `*-l2` or `*-link` crate, `multi64d`'s WebSocket path, or the specs themselves.
