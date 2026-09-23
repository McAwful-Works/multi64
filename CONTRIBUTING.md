# Contributing to multi64

Thanks for helping improve the bridge stack. This document is for **developers** working in this repository.

## Prerequisites

| | |
| --- | --- |
| **Rust** | `rust-version` in the workspace [Cargo.toml](Cargo.toml) (currently **1.80+**). Install via [rustup](https://rustup.rs/). |
| **SummerCart64** (optional) | Hardware runs: USB serial, [vendor USB docs](https://github.com/Polprzewodnikowy/SummerCart64). |
| **EverDrive-64 X7** (optional) | `multi64-ed64-l2` / [docs/spec/l3-over-everdrive-x7.md](docs/spec/l3-over-everdrive-x7.md): USB model differs from SC64; see [Krikzz dev files](https://krikzz.com/pub/support/everdrive-64/x-series/dev/) and [N64brew ED64 X7](https://n64brew.dev/wiki/EverDrive-64_X7). **CI does not** exercise ED64 hardware. |
| **EverDrive-64 PRO** (optional) | `multi64-ed64pro-l2` / [docs/spec/l3-over-everdrive-pro.md](docs/spec/l3-over-everdrive-pro.md): edlink Gen3, unrelated to the X7's USB model; start from spec §9. **CI does not** exercise it either. |
| **N64 toolchain** (optional) | Build [n64/test-rom](n64/test-rom) → `multi64_test.z64`: **libdragon** and `N64_INST` per [n64/README.md](n64/README.md). |

## Quick checks

From the repository root:

```sh
cargo fmt --all -- --check
cargo build -p multi64d                                    # not optional; see below
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p multi64d --release
cargo build --workspace --release
```

CI's Rust job runs these same six steps, in this order, on **Ubuntu** and **Windows** (see [.github/workflows/ci.yml](.github/workflows/ci.yml)).

The two `multi64d` builds are not a packaging step. `crates/multi64` declares `resources/multi64d.exe` under `bundle.resources` and its `src-tauri/build.rs` copies the daemon there; `tauri-build` treats a declared resource that is missing as a **hard error**. On a clean clone, skipping them makes `cargo clippy --workspace` fail before it lints anything. Build the daemon once per profile, as CI does — more detail in [CLAUDE.md](CLAUDE.md).

Five further jobs run on Ubuntu only, and nothing above covers them.

Three are frontend checks. Each drives one app's `index.html` in headless Chromium with `window.__TAURI__` stubbed ([Xfer64](crates/xfer64/e2e/README.md), [Multi64](crates/multi64/e2e/README.md), [AP64](crates/ap64/README.md#checks)):

```sh
cd crates/xfer64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

```sh
cd crates/multi64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

```sh
cd crates/ap64/e2e && npm ci && npx playwright install --with-deps chromium && npm test
```

The documentation check validates links, `<img>` targets and anchors, and enforces American English. It needs only `sh`, `git` and `grep`, and takes about a minute ([details](.claude/skills/check-docs/SKILL.md)):

```sh
sh .claude/skills/check-docs/check-docs.sh
```

The cart agent's host test compiles the agent for the PC, under ASan and UBSan, and needs only a host `gcc` or `clang` and `make` — nothing from the N64 toolchain:

```sh
make -C n64/agent host-test
```

With **multi64d** running, `python scripts/multi64_ws_test.py --http-only` checks the HTTP surface with no cart attached (`pip install -r scripts/requirements.txt` first). The full `--e2e --assert-echo` run needs hardware and `multi64_test.z64` in **RAW_ECHO** mode. These are not part of CI.

### `multi64-sc64-sd` (FAT / exFAT)

[crates/multi64-sc64-sd](crates/multi64-sc64-sd) includes **RAM-disk unit tests** (no SummerCart64 hardware): `cargo test -p multi64-sc64-sd` covers FAT32 and exFAT list/read, FAT32 write roundtrip, exFAT streaming write (`write_file_exfat_streaming` with `ExfatVolumeSource::Ram`), **GPT vs legacy MBR** partition detection (`detect_partition_start`), and a **SHA-256 golden** of the first FAT32 boot sector (fixed `volume_id` in the test). Hardware e2e tests are not in CI yet.

## Project layout

### Documentation and assets

| Path | Contents |
| --- | --- |
| [docs/README.md](docs/README.md) | **Documentation hub** — specs, [flash cart comparison](docs/README.md#flash-carts-l2-backends), connectors |
| [docs/spec](docs/spec) | Normative protocol documents |
| [docs/connectors](docs/connectors) | Connector usage docs |
| [docs/integration](docs/integration) | ROM integration guides (not normative): putting the M64P agent in a game |
| [n64](n64) | libdragon **test ROM** (`test-rom/` → `multi64_test.z64`); game-resident **M64P agent** (`agent/`) |
| [scripts](scripts) | Python WebSocket smoke test, hardware e2e shell scripts |

### Rust crates (`crates/`)

| Path | Contents |
| --- | --- |
| [crates/l3](crates/l3) | `multi64-l3` — L3 framing, `StreamDecoder`, session helpers |
| [crates/sc64-link](crates/sc64-link) | `multi64-sc64-link` — SC64 `CMD` / `CMP` / `PKT` wire |
| [crates/sc64-l2](crates/sc64-l2) | `multi64-sc64-l2` — L3 byte stream over SC64 serial |
| [crates/ed64-l2](crates/ed64-l2) | `multi64-ed64-l2` — EverDrive X7 L2 (implemented; **unvalidated on hardware**, see spec §4.0) |
| [crates/multi64d](crates/multi64d) | Reference daemon (`multi64d`) + library API |
| [crates/multi64-test-connector](crates/multi64-test-connector) | CLI for test ROM / M64T, and the end-to-end **suite** (`suite.rs`) the test app also runs |
| [crates/multi64-test-app](crates/multi64-test-app) | **Multi64 Test** — the suite in a window, as one portable exe |
| [crates/multi64-test-connector-gui](crates/multi64-test-connector-gui) | Optional per-command GUI; same WebSocket contract as the CLI. Developer-only: runs binaries from `target/` |
| [crates/ap64](crates/ap64) | **AP64** — Archipelago N64 seeds on a console (Tauri app); the only game-specific code in the repo, see its [README](crates/ap64/README.md) |
| [crates/ap64-core](crates/ap64-core) | `ap64-core` — seed detection, per-game profiles, splicing the cart agent in; the prebuilt agent images and their build |
| [crates/ap64-cli](crates/ap64-cli) | `ap64-cli` — `ap64-patch`, the patcher from the command line |
| [crates/ap64-cart](crates/ap64-cart) | `ap64-cart` — RDRAM and cart ROM over M64P through `multi64d` |
| [crates/ap64-connector](crates/ap64-connector) | `ap64-connector` — forked Archipelago connector scripts in embedded Lua, and the TCP side their client connects to |
| [crates/multi64-sc64-sd](crates/multi64-sc64-sd) | **SC64 SD over USB** — `Sc64SdSession`, FAT32 + exFAT (Xfer64 and the `sc64-sd-e2e` tool; **not** `multi64d`, which speaks L3 only); RAM-disk tests in `cargo test` |
| [crates/multi64-ed64-link](crates/multi64-ed64-link) | EverDrive X-series `usb64` serial (`RomRead` / `RamRead`) |
| [crates/ed64pro-link](crates/ed64pro-link) | `multi64-ed64pro-link` — EverDrive-64 PRO host link over edlink Gen3 ([spec](docs/spec/ed64-pro-usb-host.md)); scripted-transport tests only, **never run against a cart** |
| [crates/ed64pro-l2](crates/ed64pro-l2) | `multi64-ed64pro-l2` — EverDrive-64 PRO L2 over the cart FIFO and USB link ([spec](docs/spec/l3-over-everdrive-pro.md)); fake-cart tests only, **never run against a cart** |
| [crates/cart-probe](crates/cart-probe) | `multi64-cart-probe` — tells SC64, EverDrive-64 PRO and X7 apart: USB descriptors first, then identity probes (Multi64 and Xfer64 **Auto**) |
| [crates/sc64-smoke](crates/sc64-smoke) | `sc64-smoke` — SC64 vendor `IDENTIFIER` / `VERSION` |
| [crates/sc64-echo-test](crates/sc64-echo-test) | `sc64-echo-test` — raw L3 loopback e2e over `Sc64L2Pipe` (test ROM **RAW_ECHO**) |
| [crates/sc64-l3-framing-e2e](crates/sc64-l3-framing-e2e) | `sc64-l3-framing-e2e` — L3 framing e2e over `Sc64L2Pipe` |
| [crates/sc64-sd-e2e](crates/sc64-sd-e2e) | `sc64-sd-e2e` — SD/FAT e2e over `CartSession`: the only hardware coverage of the stack Xfer64 uses. Writes to the card; read-only `--list` / `--verify` modes do not |
| [crates/ed64-smoke](crates/ed64-smoke) | `ed64-smoke` — EverDrive `usb64`-style `cmd`/`t` smoke (not L3; see `l3-over-everdrive-x7.md` §8) |
| [crates/ed64-echo-test](crates/ed64-echo-test) | `ed64-echo-test` — raw L3 loopback e2e over `Ed64L2Pipe` (runs; framing **unvalidated on hardware**) |
| [crates/ed64-l3-framing-e2e](crates/ed64-l3-framing-e2e) | `ed64-l3-framing-e2e` — L3 framing e2e over `Ed64L2Pipe` (runs; framing **unvalidated on hardware**) |
| [crates/ed64pro-echo-test](crates/ed64pro-echo-test) | `ed64pro-echo-test` — raw L3 loopback e2e over `Ed64ProL2Pipe` (runs; **never run against a cart**) |
| [crates/ed64pro-l3-framing-e2e](crates/ed64pro-l3-framing-e2e) | `ed64pro-l3-framing-e2e` — L3 framing e2e over `Ed64ProL2Pipe` (runs; **never run against a cart**) |

### Tauri apps

Every Tauri app here needs **Node + npm** for `tauri dev` / `tauri build`. Two are listed below;
**AP64**, **Multi64 Test** and the developer-only connector GUI are in the crate table above.

| Path | Contents |
| --- | --- |
| [crates/multi64](crates/multi64) | **Multi64** — end-user GUI (Windows); manages `multi64d`. `tauri build` produces **MSI** and **NSIS** under `target/release/bundle/`; both embed `multi64d.exe`, copied into `bundle.resources` by `src-tauri/build.rs`. See [crates/multi64/README.md](crates/multi64/README.md). |
| [crates/xfer64](crates/xfer64) | **Xfer64** — dual-pane cart SD ↔ Windows file manager. See [crates/xfer64/README.md](crates/xfer64/README.md). |

Multi64, Xfer64 and AP64 each have their own installer; none bundles another.

For **Start bridge** in Multi64, build `multi64d` first (`cargo build -p multi64d`) with the **same** profile as the GUI. If lookup still fails, set `MULTI64D_EXE` to the full path of `multi64d.exe`. You also need a **serial port** (device plugged in or a port selected in the UI).

## Documentation

- **Human docs:** [docs/README.md](docs/README.md) is the map to all Markdown, and
  [docs/documentation-style.md](docs/documentation-style.md) is how it is written — who each file
  serves, and how it should read for them.
- **Rust API:** run `cargo doc --workspace --no-deps --open` for `//!` comments on public items.
- **Wire formats:** edit [docs/spec](docs/spec). **Spec-Revision** remains **1** until announced otherwise; bump L3 **Protocol-Major** / **Protocol-Minor** when the **binary** L3 contract changes ([l3-bridge-protocol-v1.md](docs/spec/l3-bridge-protocol-v1.md) §12).
- **Documentation describes the repo as it is.** A change that makes an existing sentence false
  corrects it in the same change; one that adds behavior nothing describes adds the description,
  in whichever place already owns that subject — a README, a module `//!`, a `docs/` page. Stale
  documentation is a defect, not debt: a reader cannot tell a sentence that was never true from
  one that stopped being true, so the whole page stops being trustworthy.
- **American English**, in prose and in the names that get read as prose — test names, error
  messages, log lines. `sh .claude/skills/check-docs/check-docs.sh` checks this against
  [`.claude/skills/check-docs/british-spellings.txt`](.claude/skills/check-docs/british-spellings.txt).
  Extend that list rather than loosening how it matches. `aria-labelledby` is an ARIA attribute,
  not a word, and is exempt.

## Style

- Match existing **rustfmt** output; do not fight the formatter.
- Prefer **clear names** and **small functions** over heavy abstraction.
- New protocol or connector behavior should be reflected in `docs/spec` or `docs/connectors` in the same change when possible.
- Run `sh .claude/skills/check-docs/check-docs.sh` after touching Markdown, renaming a spec, moving a file, or writing prose anywhere. CI runs it too, so a failure here is a failure there.

## License

By contributing, you agree that your contributions are licensed under the same terms as the project: **MIT OR Apache-2.0** (see [LICENSE](LICENSE), [LICENSE-MIT](LICENSE-MIT), and [LICENSE-APACHE](LICENSE-APACHE)).
