# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

The four checks CI runs (`.github/workflows/ci.yml`, on Ubuntu and Windows) — run these before proposing a change is done:

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

Targeted testing:

```sh
cargo test -p multi64-sc64-sd                  # one crate
cargo test -p multi64-l3 stream_decoder_resync  # one test by substring
cargo test -p multi64-sc64-sd -- --exact partition::tests::detect_partition_legacy_mbr_first_lba
```

Tests live in `crates/{l3,sc64-link,sc64-l2,ed64-l2,multi64-sc64-sd,multi64-ed64-link,ed64-smoke,xfer64}`. Everything in CI is host-only — the SD/FAT logic is covered by RAM-disk tests, and no test touches hardware.

### Tauri apps (`multi64`, `xfer64`, `multi64-test-connector-gui`)

`npm install` inside `crates/<app>/` first — it supplies the Tauri CLI. `npm run build` maps to `tauri build`. `frontendDist` points at `../src`, so the frontend is plain JS served as-is; there is no bundler output step to run.

Build `multi64d` **before** anything that compiles the Multi64 Tauri crate, and with the **same** profile. `crates/multi64/src-tauri/build.rs` copies the daemon into `resources/`, and `tauri.conf.json` declares `resources/multi64d.exe` under `bundle.resources`. A declared resource that is missing is a **hard error** in `tauri-build`: the `cargo:warning` from `build.rs` is not the whole story — the build then fails anyway. `resources/` is gitignored, so this bites a clean checkout running `cargo clippy --workspace` or `cargo build --workspace`, not just packaging.

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

**SC64 is the only real backend.** `Ed64L2Pipe` is a deliberate stub: every method returns `io::ErrorKind::Unsupported` until `docs/spec/l3-over-everdrive-x7.md` is implemented. `ed64-echo-test` and `ed64-l3-framing-e2e` compile but cannot run. Treat `Unsupported` from an ED64 L2 call as designed, not as a bug to fix in passing.

`multi64-ed64-link` is *not* part of the L3 stack despite the name — it is EverDrive USB serial plumbing for the SD path, plus cart detection.

### SD access layering

`SdCardTransport` (`crates/multi64-sc64-sd/src/link.rs`) is the sector-level seam: SC64 implements it with vendor SD ops, EverDrive with a linear `RomRead` map at a configured base address. `CartSession` sits above it and is what UI code uses (`list_dir`, `copy_cart_entry_to_host_with_progress`, `mkdir_cart`, …). EverDrive support is behind the crate's `ed64` feature, enabled only by `crates/xfer64/src-tauri`. The EverDrive SD path is experimental and read-only.

## Conventions

- **`docs/spec/` is normative.** Wire behavior changes must land with the spec edit in the same change. Bump L3 **Protocol-Major**/**Protocol-Minor** only when the byte contract changes (`l3-bridge-protocol-v1.md` §12); **Spec-Revision** is maintainer-controlled — do not bump it on your own.
- `docs/README.md` is the spec map and states the intended reading order for implementors.
- `hadris-fat` is pinned to a git rev in the workspace `[patch.crates-io]` because the 1.1.0 release fails to build with `--features exfat`. Do not unpin it to resolve a dependency conflict.
- MSRV is 1.74 and edition 2021, set once in `[workspace.package]`.
- Licensing is `MIT OR Apache-2.0`; new crates should inherit `license.workspace = true`.

## Repo tooling

Committed under `.claude/`, so they apply for anyone working on this repo:

- **`/preflight`** — runs the four CI checks in order and reports the first failure. Not every machine holding this repo has a Rust toolchain; when `cargo` is absent, say the change is unverified rather than implying otherwise.
- **`/check-docs`** — validates markdown links, heading anchors, backtick-wrapped links, and `docs/spec/` paths cited from Rust/JS. Run after any spec rename or file move; nothing in CI covers this.
- **`/implement-ed64-l2`** — the ED64 L2 backend walkthrough. The blocker is `l3-over-everdrive-x7.md` §4, not the code.
- **`spec-reviewer`** subagent — reviews a diff against `docs/spec/` as normative. Worth running on changes to `crates/l3`, any `*-l2` or `*-link` crate, `multi64d`'s WebSocket path, or the specs themselves.
