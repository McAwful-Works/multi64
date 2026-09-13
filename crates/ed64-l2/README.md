# `multi64-ed64-l2` (EverDrive 64 X7)

**Implemented, unvalidated.** Speaks the `DMA@` framing in [`l3-over-everdrive-x7.md`](../../docs/spec/l3-over-everdrive-x7.md) §4. Never run against a cart — see §4.0 and §4.5 before trusting it. Context: [`docs/README.md` — Flash carts](../../docs/README.md#flash-carts-l2-backends).

## Build

```sh
cargo check -p multi64-ed64-l2
cargo test -p multi64-ed64-l2
```

## Implementation notes

1. **Host wire is now written up** in [spec §4](../../docs/spec/l3-over-everdrive-x7.md), derived from UNFLoader — `DMA@` header + payload + `CMPH`, symmetric both ways. Unvalidated on hardware; check §4.5 first.
2. ~~Implement **`Ed64L2Pipe`**~~ — **done**; the surface mirrors **`Sc64L2Pipe`** where the carts agree.
3. **`ed64-smoke`** — `usb64` `cmd`/`t` probe ([spec §8](../../docs/spec/l3-over-everdrive-x7.md)) — not L3.
4. **`ed64-echo-test`** / **`ed64-l3-framing-e2e`** — same roles as SC64 e2e tools; ROM in **RAW_ECHO** today.
5. Optional: **`multi64d`** backend switch once the pipe is stable.

**Hardware:** **X7** (and probably 3.0) USB models only. X5 has no USB. The **EverDrive-64 PRO** has USB but speaks edlink, not this `DMA@` mapping, so this crate does not apply to it — see [spec §1.1](../../docs/spec/l3-over-everdrive-x7.md#11-which-everdrives-this-can-apply-to).
