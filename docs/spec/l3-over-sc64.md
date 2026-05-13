# L3 over SummerCart64 (normative mapping)

**Spec-Revision:** 1  

This document defines how **L3** octets ([l3-bridge-protocol-v1.md](./l3-bridge-protocol-v1.md)) are carried on the **SummerCart64** USB serial protocol described in the upstream **`docs/03_usb_interface.md`** / **`docs/02_n64_commands.md`**. It does not redefine L3 or the vendor packet shells (`CMD` / `CMP` / `PKT`).

---

## 1. Datatype tag

Multi64 uses a single logical **application pipe** for L3:

| Value | Name | Use |
|-------|------|-----|
| `0x01` | `MULTI64_L3` | L3 frame octets (opaque to SC64) |

Reserved: `0x00` and `0x02`–`0xFF` (future Multi64 channels or implementation-specific use). N64 software **SHOULD** only emit `MULTI64_L3` for this pipe unless an extension is negotiated elsewhere.

---

## 2. Host → N64 (PC `USB_WRITE`)

SC64 command **`U` (`USB_WRITE`)** (PC → cart):

- **`arg0`:** lower 8 bits = **`MULTI64_L3` (`0x01`)**; upper bits zero unless a future spec uses them.
- **`arg1`:** byte length of this chunk (**big-endian `uint32`** per vendor `CMD` layout).
- **`data`:** contiguous **raw L3 octets** for this chunk (no extra Multi64 header inside `data`).

**Fragmentation:** An L3 frame MAY be split across **multiple** `USB_WRITE` commands. Octets **MUST** be delivered in order; the N64 side **MUST** reassemble in reception order before feeding its L3 parser.

**No `CMP`:** Per vendor docs, `USB_WRITE` does **not** produce a `CMP`/`ERR` response. Host software **MUST NOT** wait for `CMP` after each `USB_WRITE`.

**Chunk size:** Implementations **SHOULD** keep each `USB_WRITE` payload ≤ **8192** bytes unless a future profile raises the limit. This matches typical L3 `MAX_PAYLOAD` defaults and avoids oversized single transfers.

---

## 3. N64 → Host (async `PKT` id `U` **DATA**)

When the running program issues the N64-side **`USB_WRITE`** command, the PC receives an asynchronous **`PKT`** with packet id **`U`** (**DATA**). Vendor layout of `PKT` + `data` applies.

Per SC64 USB documentation, the **`data`** field for id **`U`** has the structure:

| Offset | Type | Meaning |
|--------|------|--------|
| `0` | `uint8` | Datatype ( **MUST** be `MULTI64_L3` (`0x01`) for this pipe ) |
| `1`–`3` | `uint24` **big-endian** | Length in bytes of the following payload |
| `4` | `uint8[length]` | **Raw L3 octets** |

The host **MUST** validate `datatype == 0x01` for the Multi64 L3 stream, parse the 24-bit length, then append the payload bytes **in order** to the L2 receive stream consumed by the L3 decoder.

**Other `PKT` ids** (`X`, `B`, `G`, …) are **not** part of the L3 byte stream; the host **MAY** handle them in parallel (logging, UI). **`PKT` `G` (`DATA_FLUSHED`)** indicates an `USB_WRITE` from the PC was discarded because the N64 did not acknowledge in time; Multi64 implementations **SHOULD** surface this as a session/transport error toward the L3 layer.

---

## 4. L2 byte stream semantics

- **Transmit:** Concatenation of all `USB_WRITE` **`data`** payloads with `datatype = MULTI64_L3`, in issue order.
- **Receive:** Concatenation of all `PKT` **`U`** payloads after removing the 4-byte prefix (`datatype` + 3-byte length) when `datatype = MULTI64_L3`, in reception order.

L3 **MUST** tolerate fragmentation: frame boundaries may span chunk boundaries.

---

## 5. Relationship to [l2-link-adapter.md](./l2-link-adapter.md)

This document is the **SummerCart64-specific** realization of L2 for the **Multi64** stack: it maps the abstract **ordered byte stream** to **`USB_WRITE`** + **`PKT` `U`**. **EverDrive-64 X7** uses a different host mapping — see [**l3-over-everdrive-x7.md**](./l3-over-everdrive-x7.md) (draft) — with the **same L3** octet stream at the codec boundary.

---

## 6. Revision

**Spec-Revision** counts edits to **this** document only. It is **independent** of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | SummerCart64 L3 mapping (`USB_WRITE` / `PKT` `U`); pre-release tree. |

**Normative compatibility (preserved):** The **`USB_WRITE`** / **`PKT` `U`** rules in §1–4 keep a single **L3 octet stream** at the codec boundary; implementations MUST NOT change that stream’s semantics without coordinating with [`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md).

**Spec-Revision policy:** All normative docs use **Spec-Revision** **1** for now; edits **accumulate** under **1** until maintainers announce a bump. This is independent of L3 **Protocol-Major** / **Protocol-Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).
