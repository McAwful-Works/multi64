

# Contributing to multi64

Thanks for helping improve the bridge stack. This document is for **developers** working in this repository.

## Prerequisites


|                                |                                                                                                                                                                                                                                                                                                                   |
| ------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Rust**                       | `rust-version` in the workspace `[Cargo.toml](Cargo.toml)` (currently **1.74+**). Install via [rustup](https://rustup.rs/).                                                                                                                                                                                       |
| **SummerCart64** (optional)    | Hardware runs: USB serial, [vendor USB docs](https://github.com/Polprzewodnikowy/SummerCart64).                                                                                                                                                                                                                   |
| **EverDrive-64 X7** (optional) | `**ed64-l2`** / `[docs/spec/l3-over-everdrive-x7.md](docs/spec/l3-over-everdrive-x7.md)`: USB model differs from SC64; see [Krikzz dev files](https://krikzz.com/pub/support/everdrive-64/x-series/dev/) and [N64brew ED64 X7](https://n64brew.dev/wiki/EverDrive-64_X7). **CI does not** exercise ED64 hardware. |
| **N64 toolchain** (optional)   | Build `[n64/test-rom](n64/test-rom)` → `**multi64_test.z64`**: **libdragon** and `**N64_INST`** per `[n64/README.md](n64/README.md)`.                                                                                                                                                                             |


## Quick checks

From the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
python -m pip install -r oot-poc/pc/requirements.txt
python -m unittest discover -s oot-poc/pc -p "test_*.py" -v
```

CI runs the same steps on **Ubuntu** and **Windows** (see `[.github/workflows/ci.yml](.github/workflows/ci.yml)`), including **oot-poc** Python tests.

With **multi64d** running, you can manually run **`python oot-poc/pc/multi64d_smoke.py`** (HTTP + WebSocket ping; no cart). **`--with-l3`** needs hardware and a ROM that speaks **`rom-to-pc`** on L3 (see **`[oot-poc/pc/README.md](oot-poc/pc/README.md)`**).

### `multi64-sc64-sd` (FAT / exFAT)

[`crates/multi64-sc64-sd`](crates/multi64-sc64-sd) includes **RAM-disk unit tests** (no SummerCart64 hardware): `cargo test -p multi64-sc64-sd` covers FAT32 and exFAT list/read, FAT32 write roundtrip, exFAT streaming write (`write_file_exfat_streaming` with `ExfatVolumeSource::Ram`), **GPT vs legacy MBR** partition detection (`detect_partition_start`), and a **SHA-256 golden** of the first FAT32 boot sector (fixed `volume_id` in the test). Hardware e2e tests are not in CI yet.

## Project layout

### Documentation


| Path                                       | Contents                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[docs/README.md](docs/README.md)`         | **Documentation hub** — specs, [flash cart comparison](docs/README.md#flash-carts-l2-backends), connectors                                                                                                                                                                                                                                                                                                                                                                                      |
| `[docs/spec](docs/spec)`                   | Normative protocol documents                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `[docs/connectors](docs/connectors)`       | Connector usage docs                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `[n64](n64)`                               | libdragon **test ROM** (`test-rom/` → `multi64_test.z64`)                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `[crates/multi64](crates/multi64)`       | **Tauri** end-user GUI (Windows) — manages `multi64d`; needs **Node + npm** for `tauri dev` / `tauri build`. **`tauri build`** produces **MSI** and **NSIS** under `target/release/bundle/`; both embed the same `bundle.resources` (e.g. `multi64d.exe`, `xfer64-setup.exe` copied by `src-tauri/build.rs`). For **Start daemon**, build `**multi64d`** first (`cargo build -p multi64d`) with the **same** profile as the GUI. If lookup still fails, set `**MULTI64D_EXE`** to the full path of `multi64d.exe`. You also need a **COM port** (device plugged in or a port selected in the UI). See [`crates/multi64/README.md`](crates/multi64/README.md) for **Xfer64** build order. |
| `[scripts](scripts)`                       | Python WebSocket smoke test, hardware e2e shell scripts                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `[oot-poc](oot-poc)`                       | **OoT proof of concept** — JSON schemas, [`oot-poc/rom`](oot-poc/rom) `oot_poc.z64`, [`validate_ws.py`](oot-poc/pc/validate_ws.py) / [`bidirectional_poc.py`](oot-poc/pc/bidirectional_poc.py), [`connector/README.md`](oot-poc/connector/README.md), [`mapping/README.md`](oot-poc/mapping/README.md); not normative core protocol (see [`oot-poc/README.md`](oot-poc/README.md))                                                                                                                                                                                                                                                                                          |


### Rust crates (`crates/`)


| Path                                                             | Contents                                                                                                  |
| ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `[crates/l3](crates/l3)`                                         | `**multi64-l3`** — L3 framing, `StreamDecoder`, session helpers                                           |
| `[crates/sc64-link](crates/sc64-link)`                           | `**multi64-sc64-link**` — SC64 `CMD` / `CMP` / `PKT` wire                                                 |
| `[crates/sc64-l2](crates/sc64-l2)`                               | `**multi64-sc64-l2**` — L3 byte stream over SC64 serial                                                   |
| `[crates/ed64-l2](crates/ed64-l2)`                               | `**multi64-ed64-l2**` — EverDrive X7 L2 (**stub**; see spec)                                              |
| `[crates/multi64d](crates/multi64d)`                             | Reference daemon (`multi64d`) + library API                                                               |
| `[crates/multi64-test-connector](crates/multi64-test-connector)` | CLI for test ROM / M64T                                                                                   |
| `[crates/multi64-sc64-sd](crates/multi64-sc64-sd)`               | **SC64 SD over USB** — `Sc64SdSession`, FAT32 + exFAT (Xfer64 / `multi64d`); RAM-disk tests in `cargo test` |
| `[crates/sc64-smoke](crates/sc64-smoke)`                         | `**sc64-smoke`** — SC64 vendor `IDENTIFIER` / `VERSION`                                                   |
| `[crates/sc64-echo-test](crates/sc64-echo-test)`                 | `**sc64-echo-test**` — raw L3 loopback e2e over `**Sc64L2Pipe**` (test ROM **RAW_ECHO**)                  |
| `[crates/sc64-l3-framing-e2e](crates/sc64-l3-framing-e2e)`       | `**sc64-l3-framing-e2e`** — L3 framing e2e over `**Sc64L2Pipe**`                                          |
| `[crates/ed64-smoke](crates/ed64-smoke)`                         | `**ed64-smoke**` — EverDrive `**usb64`-style `cmd`/`t**` smoke (not L3; see `l3-over-everdrive-x7.md` §8) |
| `[crates/ed64-echo-test](crates/ed64-echo-test)`                 | `**ed64-echo-test**` — raw L3 loopback e2e over `**Ed64L2Pipe**` (blocked until `ed64-l2`)                |
| `[crates/ed64-l3-framing-e2e](crates/ed64-l3-framing-e2e)`       | `**ed64-l3-framing-e2e**` — L3 framing e2e over `**Ed64L2Pipe**` (blocked until `ed64-l2`)                |


## Documentation

- **Human docs:** `[docs/README.md](docs/README.md)` is the map to all Markdown.
- **Rust API:** run `cargo doc --workspace --no-deps --open` for `//!` comments on public items.
- **Wire formats:** edit `[docs/spec](docs/spec)`. **Spec-Revision** remains **1** until announced otherwise; bump L3 **Protocol-Major** / **Protocol-Minor** when the **binary** L3 contract changes (`[l3-bridge-protocol-v1.md](docs/spec/l3-bridge-protocol-v1.md)` §12).

## Style

- Match existing **rustfmt** output; do not fight the formatter.
- Prefer **clear names** and **small functions** over heavy abstraction.
- New protocol or connector behavior should be reflected in `**docs/spec`** or `**docs/connectors**` in the same change when possible.

## License

By contributing, you agree that your contributions are licensed under the same terms as the project: **MIT OR Apache-2.0** (see `[LICENSE-MIT](LICENSE-MIT)` and `[LICENSE-APACHE](LICENSE-APACHE)`).