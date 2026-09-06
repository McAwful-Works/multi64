---
name: implement-ed64-l2
description: Guide for turning Ed64L2Pipe from an Unsupported stub into a working EverDrive 64 X7 L2 backend - what the draft spec still leaves undefined, which surface to mirror from Sc64L2Pipe, and what unblocks downstream. Use when asked to implement, design, or estimate ED64 L2 or L3-over-EverDrive support.
---

# Implementing `multi64-ed64-l2`

`Ed64L2Pipe` (`crates/ed64-l2/src/lib.rs`) is a deliberate stub: every method returns `io::ErrorKind::Unsupported` with a pointer to the spec. It is not broken code, and it should not be made to "work" by loosening those errors.

## The blocker is the spec, not the code

`docs/spec/l3-over-everdrive-x7.md` is **Draft**, and §4 is explicit that four things MUST be specified before the host mapping is normative:

1. **Device discovery** — COM / `/dev/tty*` / USB IDs per platform.
2. **Wire format** — whether raw L3 octets are wrapped, and whether a datatype byte (`0x01` = `MULTI64_L3`, as SC64 uses) prefixes each chunk.
3. **Fragmentation** — how L3 frames larger than one USB transaction split and reassemble on both sides.
4. **Errors** — stall / timeout / flush mapped onto `std::io::Error` and L3 session behavior.

**Write the spec first.** §4 exists precisely so the crate has something normative to match, and §6 says the crate MUST follow it once written. Implementing against guessed framing and backfilling the spec inverts the project's contract. If you cannot answer all four from vendor sources or measured traces, say what is still unknown rather than picking a plausible-looking layout.

Useful inputs: `ed64-smoke` is a working `usb64` `cmd`/`t` probe (spec §8, explicitly non-normative and **not** L3), `multi64-ed64-link` holds the X-series `usb64` serial plumbing, and N64brew documents the cart side (`REG_USB_CFG`, `REG_USB_DATA`).

## Constraints that will shape the design

§3 records that EverDrive USB paths move data through `REG_USB_DATA` in **512-byte** chunks, and some flows need **at least 16 bytes** per read. The host adapter may buffer, pad, or split — but padding **MUST NOT** appear inside L3 payloads. The codec above must still see one continuous octet stream, exactly as `l2-link-adapter.md` requires of every backend.

## Mirror the SC64 surface

`Sc64L2Pipe` (`crates/sc64-l2/src/lib.rs`) is the reference shape. Match it where the carts genuinely agree:

`open(port_name, baud)`, `set_timeout`, `clear_serial_buffers` (which must also reset internal parse state, not just the port), `write_l3_stream`, `write_l3_stream_with_max`, `read_l3_bytes` (returns `0` on timeout with an empty queue), `read_l3_bytes_exact`.

Note how SC64 separates the wire buffer from the decoded L3 queue (`WireBuffer` + `VecDeque<u8>`) and drains events in `process_wire_events`. An ED64 implementation needs the same split if its framing carries anything besides payload — do not decode L3 inside the L2 crate.

## What this unblocks

`ed64-echo-test` and `ed64-l3-framing-e2e` already compile and are written against `Ed64L2Pipe`; they start working once `open` succeeds. Both need the test ROM in **RAW_ECHO** and real X7 hardware — X5 has no USB. Wiring `multi64d` to select an ED64 backend is a later, optional step (`crates/ed64-l2/README.md` step 5), not part of making the pipe work.

## Finishing

Update `docs/spec/l3-over-everdrive-x7.md` §9 and drop the Draft status only when §4 is genuinely answered. Bump L3 Protocol-Major/Minor only if the L3 byte contract itself changed — adding a backend normally does not change it. Leave **Spec-Revision** alone; that is maintainer-controlled. `docs/README.md` and `crates/ed64-l2/README.md` both describe the crate as a stub and will need updating in the same change.
