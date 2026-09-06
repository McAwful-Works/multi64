# `multi64-ed64-l2` (EverDrive 64 X7)

**Stub** — no USB I/O yet. Planned mapping: [`l3-over-everdrive-x7.md`](../../docs/spec/l3-over-everdrive-x7.md). Context: [`docs/README.md` — Flash carts](../../docs/README.md#flash-carts-l2-backends).

## Build

```sh
cargo check -p multi64-ed64-l2
cargo test -p multi64-ed64-l2
```

## Implementation notes

1. Lock **host wire** in the spec (§4) using **`usb64`** / X7 traces — see [`multi64-ed64-link`](../../crates/multi64-ed64-link), [`ed64-sd-usb-host.md`](../../docs/spec/ed64-sd-usb-host.md).
2. Implement **`Ed64L2Pipe`** (`read` / `write`) aligned with **`Sc64L2Pipe`** where sensible.
3. **`ed64-smoke`** — `usb64` `cmd`/`t` probe ([spec §8](../../docs/spec/l3-over-everdrive-x7.md)) — not L3.
4. **`ed64-echo-test`** / **`ed64-l3-framing-e2e`** — same roles as SC64 e2e tools; ROM in **RAW_ECHO** today.
5. Optional: **`multi64d`** backend switch once the pipe is stable.

**Hardware:** **X7** USB models only (X5 has no USB).
