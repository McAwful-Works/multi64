---
name: preflight
description: Run the four checks CI enforces (cargo fmt, clippy with -D warnings, test, release build) in order and report the first failure with enough context to act on it. Use before handing work back, before opening a PR, or whenever asked whether a change will pass CI.
---

# Pre-flight: the checks CI actually runs

`.github/workflows/ci.yml` runs exactly these four, in this order, on `ubuntu-latest` and `windows-latest`. Nothing else gates a merge, and no CI job touches hardware.

```sh
cargo fmt --all -- --check
cargo build -p multi64d                                    # required, see below
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p multi64d --release
cargo build --workspace --release
```

The `multi64d` builds are not packaging steps. `crates/multi64` declares `resources/multi64d.exe` in `bundle.resources`, and `tauri-build` treats a missing declared resource as a hard error — so on a clean clone `cargo clippy --workspace` fails before it lints anything. Build the daemon once per profile first, as CI does.

Run them in order and stop at the first failure — a fmt failure usually makes the clippy output noise, and a clippy failure often means the test run is compiling the same broken code twice.

## Before you start

Check that a toolchain exists: `cargo --version`. Some machines that hold this repo have no Rust installed at all, in which case **say so plainly and do not claim the change is verified** — an unverified change is a normal outcome to report, not a problem to paper over. Offer the commands so the user can run them where a toolchain exists.

## Reading the failures

- **`cargo fmt`** — run `cargo fmt --all` to fix, then re-check. Never hand-format to satisfy it.
- **`cargo clippy`** — `-D warnings` promotes every rustc lint too, so an unused import, an unused `mut`, or dead code fails the build exactly like a type error. Fix the cause; do not add `#[allow(...)]` without saying why in the code.
- **`cargo test`** — host-only. The SD/FAT logic is covered by RAM-disk tests in `multi64-sc64-sd`; if those fail, suspect the `hadris-fat` pin in the workspace `[patch.crates-io]` before suspecting the test.
- **`cargo build --release`** — can surface errors debug builds miss, and builds the Tauri crates. Needs the Linux WebView dev packages on Ubuntu (the CI workflow lists them).

## Not covered here

Anything needing a cart: `multi64d`, the `*-smoke` binaries, and the `*-echo-test` / `*-l3-framing-e2e` crates. Those never run in CI, so passing pre-flight says nothing about whether a serial change actually works on hardware. Say that explicitly when a change touches the serial path.
