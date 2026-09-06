---
name: implement-ed64-l2
description: Guide for finishing EverDrive 64 X7 support - the DMA@ framing is implemented in Ed64L2Pipe but has never touched hardware, so this covers what to validate first, the ROM-side cart gate, and what would make the spec normative. Use when asked to implement, validate, debug, or estimate ED64 L2 or L3-over-EverDrive support.
---

# Finishing `multi64-ed64-l2`

`Ed64L2Pipe` (`crates/ed64-l2/src/lib.rs`) implements the framing in `docs/spec/l3-over-everdrive-x7.md` §4: a symmetric `DMA@` header carrying `(datatype << 24) | size`, payload padded to 2 bytes, `CMPH` trailer. Its unit tests cover encode/decode, resync, the datatype filter, and trailer errors.

**None of it has touched a cart.** The framing is transcribed from UNFLoader and libdragon's `usb.c` — a working reference, not observation. Everything below assumes that distinction matters, because the previous EverDrive attempt in this repo failed precisely by trusting a plausible vendor source that did not apply to the hardware (spec §1.1).

## Do not claim support

Until someone runs this against an X7:

- Do not describe EverDrive as supported anywhere user-facing.
- A successful `Ed64L2Pipe::open` means **the serial port opened**. There is no identity handshake in the data path. Use `ed64-smoke` (spec §8) to confirm a port is really an EverDrive.
- Keep spec §4 non-normative and the document **Draft**.

## What to check first, in order

1. **`ed64-smoke` against the cart.** Confirms the port, driver and baud before any L2 work. If this fails, nothing downstream is meaningful — try another `--baud`, `--flush`, and confirm the EverDrive OS has USB active.
2. **Alignment.** §4.5 records this as resolved from libdragon source (`USBPROTOCOL_VERSION 2`, 2-byte alignment) and `SEND_ALIGN` matches. Confirm the cart's firmware agrees; a mismatch mis-frames *every* message, so it will look like total failure rather than corruption.
3. **VCP vs D2XX.** UNFLoader uses FTDI D2XX and purges its queues directly; this crate uses `serialport` (VCP). Whether `clear_serial_buffers` gives equivalent behaviour under load is unverified, and is the most likely source of *intermittent* rather than total failure.
4. **Chunk size.** `DEFAULT_ED64_CHUNK` is 512 — one `REG_USB_DATA` window, deliberately conservative. The ROM's own cap is `TEST_USB_WRITE_MAX` (8192). Raise via `write_l3_stream_with_max` only after the link is proven.

## The N64 side

`n64/test-rom` already links libdragon's `<usb.h>`, which abstracts both carts, and already emits `usb_write(MULTI64_L3, ...)`. The only thing blocking EverDrive is an explicit gate in `main.c`:

```c
if (usb_getcart() != CART_SC64) { printf("Need SummerCart64\n"); while (1) { } }
```

Relax it to accept `CART_EVERDRIVE` **together with** hardware validation, not before. A ROM that advertises support it has never demonstrated is worse than one that refuses to boot.

## Then

`ed64-echo-test` and `ed64-l3-framing-e2e` already link `Ed64L2Pipe` and need no changes — they start working when the link does. Both want the ROM in **RAW_ECHO** and real X7 hardware; X5 has no USB.

Wiring `multi64d` to select an ED64 backend is a later, optional step, not part of proving the pipe.

## When it actually works

Drop **Draft** from `docs/spec/l3-over-everdrive-x7.md` and make §4 normative only once §4.5 is answered by observation. Record what was tested and on which OS version. Bump L3 Protocol-Major/Minor only if the L3 byte contract changed — adding a backend does not. Leave **Spec-Revision** alone; maintainer-controlled.

Several files still describe the crate as unvalidated — `README.md`, `CONTRIBUTING.md`, `docs/README.md`, `docs/spec/README.md`, `docs/connectors/test-rom.md`, `crates/ed64-l2/README.md`, and CLAUDE.md. Update them in the same change, and run `/check-docs`.
