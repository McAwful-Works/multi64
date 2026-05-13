# Contributing to Multi64

For **developers** working in this repository.

## Prerequisites

| | |
|--|--|
| **Rust** | `rust-version` in [Cargo.toml](Cargo.toml) (currently **1.74+**). [rustup](https://rustup.rs/) |
| **SummerCart64** (optional) | USB serial; [vendor docs](https://github.com/Polprzewodnikowy/SummerCart64) |
| **EverDrive-64 X7** (optional) | Stub L2: [l3-over-everdrive-x7.md](docs/spec/l3-over-everdrive-x7.md). [Krikzz X-series dev files](https://krikzz.com/pub/support/everdrive-64/x-series/dev/), [N64brew](https://n64brew.dev/wiki/EverDrive-64_X7). Not exercised in CI |
| **N64 / libdragon** (optional) | [n64/test-rom](n64/test-rom) → `multi64_test.z64`; **`N64_INST`** — [n64/README.md](n64/README.md) |

## Checks (from repo root)

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

**`multi64-sc64-sd`:** `cargo test -p multi64-sc64-sd` runs FAT/exFAT and partition logic on RAM disks (no hardware).

## Layout

| Path | Role |
|------|------|
| [docs/README.md](docs/README.md) | Doc hub, [flash cart table](docs/README.md#flash-carts-l2-backends) |
| [docs/spec](docs/spec) | Normative protocols |
| [docs/connectors](docs/connectors) | Connector docs |
| [n64](n64) | `multi64_test.z64` |
| [crates/multi64](crates/multi64) | **Multi64** Tauri app — Node + npm; build **`multi64d`** first. [crates/multi64/README.md](crates/multi64/README.md) |
| [crates/xfer64](crates/xfer64) | **Xfer64** Tauri app — [crates/xfer64/README.md](crates/xfer64/README.md) |
| [scripts](scripts) | Python smoke + shell e2e |

### Rust crates (`crates/`)

| Path | Package / role |
|------|------------------|
| [l3](crates/l3) | `multi64-l3` |
| [sc64-link](crates/sc64-link) | `multi64-sc64-link` |
| [sc64-l2](crates/sc64-l2) | `multi64-sc64-l2` |
| [ed64-l2](crates/ed64-l2) | `multi64-ed64-l2` (stub) |
| [multi64d](crates/multi64d) | Daemon + library |
| [multi64-test-connector](crates/multi64-test-connector) | Test ROM CLI |
| [multi64-sc64-sd](crates/multi64-sc64-sd) | SC64 / ED SD host session |
| [multi64-ed64-link](crates/multi64-ed64-link) | EverDrive **edlink** + **`usb64`** serial |
| [sc64-smoke](crates/sc64-smoke) | SC64 smoke |
| [sc64-echo-test](crates/sc64-echo-test), [sc64-l3-framing-e2e](crates/sc64-l3-framing-e2e) | SC64 e2e |
| [ed64-smoke](crates/ed64-smoke) | ED `usb64` smoke |
| [ed64-echo-test](crates/ed64-echo-test), [ed64-l3-framing-e2e](crates/ed64-l3-framing-e2e) | ED e2e (blocked on `ed64-l2`) |
| [multi64](crates/multi64), [xfer64](crates/xfer64) | Tauri apps |
| [multi64-test-connector-gui](crates/multi64-test-connector-gui) | Optional GUI |

## Docs & API

- Specs: [docs/README.md](docs/README.md)
- Rust API: `cargo doc --workspace --no-deps --open`
- Changing L3 wire rules: bump **Protocol-Major** / **Protocol-Minor** per [l3-bridge-protocol-v1.md §12](docs/spec/l3-bridge-protocol-v1.md)

## Style

- **rustfmt** defaults; small, clear functions.
- Protocol or connector behavior: update **`docs/spec`** or **`docs/connectors`** in the same change when practical.

## License

Contributions are accepted under the **same dual license** as **Multi64**: **MIT OR Apache-2.0** ([LICENSE](LICENSE), [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). By contributing, you agree your work may be distributed under those terms.
