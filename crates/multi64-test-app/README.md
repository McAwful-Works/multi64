# Multi64 Test

A window with a **Run tests** button that exercises the whole L3 bridge against a real cart and
reports **PASS**/**FAIL** per check.

```sh
cargo build --release -p multi64-test-app     # target/release/multi64-test-app.exe
```

## What a tester needs

Three things, and nothing else:

1. the **Multi64 installer** — it carries `multi64d`, and the Xfer64 it installs can put the ROM on
   the card;
2. **`multi64_test.z64`** on the cart's SD card, booted;
3. this **`multi64-test-app.exe`**.

No Rust, no Node, no `bash`, no loose helper binaries. **The controller is not needed**: the ROM
boots into `RAW_ECHO`, which parses nothing, and the suite drives it out with `REQ_SET_MODE`.

## What it does not contain

The checks. Those live in [`multi64-test-connector`](../multi64-test-connector)'s `suite` module, so
this app and `multi64-test-connector suite` run exactly the same ones — there is deliberately no
second implementation to drift. This crate is the window: it starts a run, renders each result as it
arrives, and hands back the summary.

Everything is **linked, not spawned**. The direct-serial checks talk to `Sc64L2Pipe` directly rather
than shelling out to `sc64-echo-test` and `sc64-l3-framing-e2e`, which is what collapses the
handover to a single file. (The older `multi64-test-connector-gui` does shell out, with the path
baked in at compile time, and so only works on a machine that has built the repo.)

## Things worth knowing before changing it

- **The expected ROM version is baked in at build time** by `build.rs`, read from
  `n64/test-rom/test_proto.h`. A tester has no way to know a version string, and that check is the
  one that stops a whole run being a test of some *other* ROM. When it cannot be read the check is
  **skipped, not passed** — a check that cannot fail is worse than no check.
- **The serial port comes from the daemon**, not from a default. A compiled-in port is a port that
  can disagree with the machine it runs on, and disagreeing means the direct-serial checks open some
  other device entirely.
- **The page's structure is built once and never grows.** Every phase section and the tally exist
  before the first run; a run only changes the rows inside them. Chrome that appears part-way
  through moves everything below it, which is exactly when someone is reading a result. The phase
  list comes from `suite::PHASES` rather than being repeated here, so renaming a phase cannot leave
  an empty section beside an orphaned one.
- **The styling is the shared base**, byte-identical with Multi64 and Xfer64 and enforced by tests in
  the `multi64` crate. See [`docs/frontend-appearance.md`](../../docs/frontend-appearance.md): the
  palette lives in `:root`, no rule outside it may name a colour, and app-only rules go in
  `app.css`.

## Related

- [`docs/connectors/test-rom.md`](../../docs/connectors/test-rom.md) — what the run covers, and the
  command-line equivalent.
- [`n64/README.md`](../../n64/README.md#hardware-record) — the hardware runs.
