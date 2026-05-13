# L3 over EverDrive 64 X7 (draft mapping)

**Spec-Revision:** 1  
**Status:** **Draft** — normative **L3-over-USB host mapping** is still **TBD**. **`multi64-ed64-l2`** (`Ed64L2Pipe`) is a **stub**. In-tree EverDrive tooling: **`ed64-smoke`** (§8), **`ed64-echo-test`**, **`ed64-l3-framing-e2e`** (blocked on L2).

This document will define how **L3** octets ([l3-bridge-protocol-v1.md](./l3-bridge-protocol-v1.md)) are carried over the **EverDrive-64 X7** USB path. It does **not** redefine L3.

Until the **host↔USB** byte model for the **L3 stream** is locked and implemented in **`crates/ed64-l2`**, treat §4 as **design notes**; §8 documents a **non-normative** USB64 **`cmd`/`t`** smoke handshake (Krikzz **`usb64`** style) and notes legacy **uppercase `CMD` + `T`** probes.

---

## 1. Scope

| Item | Detail |
|------|--------|
| **Hardware** | **EverDrive-64 X7** (USB models). **X5** and carts **without USB** are **out of scope** for this mapping. |
| **Goal** | Same **ordered L3 byte stream** on the PC as [l2-link-adapter.md](./l2-link-adapter.md) — differences stay inside L2/L1. |
| **Reference backend** | [l3-over-sc64.md](./l3-over-sc64.md) shows the pattern: datatype tag + length + payload chunks. |

---

## 2. References (external)

Implementors should start from vendor and community sources:

| Resource | Notes |
|----------|--------|
| [Krikzz — EverDrive-64 X-series dev files](https://krikzz.com/pub/support/everdrive-64/x-series/dev/) | `usb64` sources, `usbio-sample.zip`, etc. |
| [krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub) (GitHub) | Reference code: **`usb64/usb64/`** (Windows `usb64` tool, C# `CommandProcessor`), **`ED64-XIO`** sample, **`docs/`** (TOC, hardware ID; some USB wire notes may be **WIP**) |
| [N64brew — EverDrive-64 X7](https://n64brew.dev/wiki/EverDrive-64_X7) | **N64-side** registers (`REG_USB_CFG`, `REG_USB_DATA` 512-byte buffer), `bi_usb_rd` / `bi_usb_wr` behavior |
| [jsdf/webserial-ed64log](https://github.com/jsdf/webserial-ed64log) | Example of host serial/WebUSB usage |
| [krikzz/edlink](https://github.com/krikzz/edlink) | PC tool for **PRO / CORE** (and other carts). **`edlink/Device/Link.cs`** `TxCMD`: bytes `0x2B ('+')`, `0xD4 ('+' ^ 0xFF)`, `cmd_code`, `cmd_code ^ 0xFF`, optional **subcmd**; then length-prefixed payloads / `Tx32` / `Rx32` per **`DeviceIO_V2`**. Default **921600** baud in `OpenConnection`. This framing is **not** the X-series **`usb64`** ASCII **`cmd`** + 16-byte packet (§8); do not assume edlink bytes match **`Ed64Link::command_packet`**. |

**Normative for Multi64:** once this spec defines the **PC-side** framing, the **`ed64-l2`** crate MUST match it; the N64 ROM MUST use compatible `bi_usb_*` (or equivalent) so L3 bytes round-trip.

---

## 3. L2 constraints (from [l2-link-adapter.md](./l2-link-adapter.md) §5)

EverDrive USB paths often use **fixed 512-byte** data areas on the cart. The host adapter MAY buffer, pad, or split so that the **L3 codec** still sees a single continuous stream (padding MUST NOT appear inside L3 payloads).

Public documentation (N64brew) notes practical limits on the **N64** side, including:

- Data moved through **`REG_USB_DATA`** in up to **512-byte** chunks.
- Reads may require **at least 16 bytes** in some flows; hosts should pad outbound data accordingly when the final mapping requires it.

Exact **host** packet layout (CDC framing, headers, alignment) is **TBD** here pending alignment with **usb64** / measured traces.

---

## 4. Host-side mapping (TBD)

The following MUST be specified before this document is **non-draft**:

1. **Device discovery** — COM port / `/dev/tty*` / USB IDs on Windows, Linux, macOS.
2. **Wire format** — How raw L3 octets are wrapped for the PC driver (if any); whether a **datatype byte** (e.g. `0x01` = `MULTI64_L3`, matching SC64) prefixes each chunk or the stream is raw.
3. **Fragmentation** — How L3 frames larger than one USB transaction are split and reassembled on host and N64.
4. **Errors** — Stall, timeout, buffer flush — mapping to `std::io::Error` / L3 session behavior.

---

## 5. N64-side expectations

Homebrew should follow Krikzz-style initialization (`REG_KEY`, `REG_SYS_CFG`, USB cfg) and use **`bi_usb_rd` / `bi_usb_wr`** (or equivalent) so that the byte stream seen by the game matches what the **host adapter** emits.

Games using **libdragon** may integrate via the same L3 helpers as the SC64 path once the **host** side is compatible.

---

## 6. Implementation in this repository

| Component | Role |
|-----------|------|
| [`crates/multi64-ed64-link`](../../crates/multi64-ed64-link) | Rust **`multi64-ed64-link`**: **edlink** Gen3 (**PRO/CORE**) + legacy X7 **`usb64`** **`cmd`** framing, `RomRead`, cart **`probe_ed64_serial_cart`** — **not** the L3 stream adapter (**`ed64-l2`**). |
| [`crates/ed64-l2`](../../crates/ed64-l2/README.md) | Stub crate; will provide an L2 handle similar to `multi64-sc64-l2::Sc64L2Pipe` when implemented. |
| [`crates/ed64-smoke`](../../crates/ed64-smoke) | **`ed64-smoke`** binary: host **`cmd`/`t`** smoke test (§8, `usb64`-style), not L3. |
| [`crates/ed64-echo-test`](../../crates/ed64-echo-test) | **`ed64-echo-test`**: same role as `sc64-echo-test` over **`Ed64L2Pipe`** (blocked until L2). |
| [`crates/ed64-l3-framing-e2e`](../../crates/ed64-l3-framing-e2e) | **`ed64-l3-framing-e2e`**: same role as `sc64-l3-framing-e2e` over **`Ed64L2Pipe`** (blocked until L2). |
| `multi64d` | Future: optional backend selection (`--link ed64` or similar) once `ed64-l2` is functional. |

---

## 7. Contributor testing checklist (hardware)

For developers **with** an X7:

1. Build and run **`n64/test-rom`** → **`multi64_test.z64`** with ED64-specific USB bring-up (may require a small ED64 init layer — to be shared in-repo when available).
2. Confirm **serial device** appears on the host when the ROM uses USB.
3. Capture **host↔device** traces (optional) to help finalize §4.
4. Open a PR updating this spec + **`ed64-l2`** with measured behavior.
5. Run **`ed64-smoke`** (`crates/ed64-smoke`) if the cart exposes EverDrive-style USB serial — see §8.

---

## 8. Host USB64 `cmd`/`t` smoke handshake (non-normative)

This is **not** the future normative L3 byte pipe from §4. It documents the host **test connection** probe used by Krikzz’s reference **`usb64`** (`CommandProcessor` in **`ed64-x-pub`**) and implemented as **`ed64-smoke`**. Some **older community** host code sent only a **short uppercase** form (`CMD` + `T`); **firmware** may or may not accept that.

| Field | Value |
|-------|--------|
| PC → device (reference) | **16 bytes:** ASCII **`cmd`** (lowercase) + single-byte command **`t`** (`0x74`, test connection), then three **big-endian `uint32`** fields: **address**, **length** (in 512-byte blocks in the vendor tool), **argument** — all **zero** for this probe. Matches **`CommandPacketTransmit(TransmitCommand.TestConnection)`** in **`usb64/usb64/CommandProcessor.cs`**. |
| PC → device (legacy) | **4 bytes:** ASCII **`CMD`** + **`T`** — seen in some community USB loaders; **not** interchangeable with the reference layout on all builds. |
| Success pattern | Response buffer has **`k`** (legacy reply) or **`r`** (reply) at **byte index 3** (0-based), per vendor receive parsing. Some firmware builds also send **`3`** at index 4 and a region hint at index 5 (`p` / `n` / `m`). |

**X7 OS builds and USB drivers differ.** If `ed64-smoke` fails, try another baud rate, **`--flush`**, or confirm the EverDrive menu / OS has USB serial active. Capture traces to help finalize §4.

---

## 9. Revision

**Spec-Revision** counts edits to **this** document only. It is **independent** of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | EverDrive X7 draft mapping: **`usb64`** smoke (**§8**), **`ed64-x-pub`** references, **`multi64_test.z64`** checklist; pre-release tree. |

**Normative compatibility (preserved):** The goal remains **one L3 octet stream** at the codec boundary (see [l2-link-adapter.md](./l2-link-adapter.md)); ED64 host details in §4–§5 must not break that once normative.

**Spec-Revision policy:** All normative docs use **Spec-Revision** **1** for now; edits **accumulate** under **1** until maintainers announce a bump. This is independent of L3 **Protocol-Major** / **Protocol-Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).
