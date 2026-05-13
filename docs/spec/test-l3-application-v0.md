# L3 APPLICATION — Multi64 test ROM (v0)

**Spec-Revision:** 1  

Experimental payloads carried in L3 **`DATA`** frames on **`CHANNEL = APPLICATION` (`0x00`)** for the official **`n64/test-rom`** (see [`../../n64/README.md`](../../n64/README.md)). This is **not** game-specific; it is for **host ↔ cart** bring-up, controller snapshots, echo tests, stress paths, **session handshake**, **save hardware** checks, **host-triggered rumble**, and **host text on the ROM HUD**.

**Magic:** ASCII **`M64T`** — bytes `0x4D 0x36 0x34 0x54`.

---

## 1. Payload layout

| Offset | Size | Description |
|--------|------|-------------|
| `0`–`3` | 4 | Magic **`M64T`** |
| `4` | 1 | `msg` — request or response code (see §2–§3) |
| `5`– | * | Optional body; length = `L3_PAYLOAD_LEN - 5` |

`L3_PAYLOAD_LEN` is the APPLICATION payload length from the L3 header (see [`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md)).

---

## 2. Host → cart (requests)

| `msg` | Name | Body |
|-------|------|------|
| `0x01` | `PING` | Empty or ignored |
| `0x02` | `ECHO` | Opaque bytes to echo back |
| `0x03` | `REQ_VERSION` | Empty |
| `0x04` | `REQ_CONTROLLER` | Empty — cart replies with latest controller state |
| `0x05` | `SESSION_OPEN` | Optional **8-byte opaque challenge** (shorter bodies are zero-padded for the echo in `SESSION_ACK`) |
| `0x06` | `SESSION_CLOSE` | Empty — cart clears session and sends `SESSION_END` |
| `0x07` | `REQ_EEPROM_INFO` | Empty |
| `0x08` | `REQ_EEPROM_READ` | `uint16` BE offset, `uint16` BE length (max **256** bytes per read) |
| `0x09` | `REQ_EEPROM_WRITE` | `uint16` BE offset, `uint16` BE length, then **length** bytes — **requires active session** |
| `0x0A` | `REQ_SRAM_INFO` | Empty |
| `0x0B` | `REQ_SRAM_READ` | `uint32` BE offset, `uint16` BE length (max **512**; **offset** and **length** must be **even**; PI SRAM) |
| `0x0C` | `REQ_SRAM_WRITE` | `uint32` BE offset, `uint16` BE length, then **length** bytes — **requires active session**; same alignment rules as read |
| `0x0D` | `REQ_RUMBLE` | `uint8` port `0`–`3`, `uint8` duration in VI frames (**`0`** = default **60**; cart clamps to **600** frames) |

---

## 3. Cart → host (responses and unsolicited)

| `msg` | Name | Body |
|-------|------|------|
| `0x81` | `PONG` | Empty |
| `0x82` | `ECHO_REPLY` | Same bytes as `ECHO` request body (bytes `5..` of request) |
| `0x83` | `VERSION` | UTF-8 product string (e.g. `multi64-test-rom 1.4`) |
| `0x84` | `CONTROLLER` | See §4 (9 bytes) |
| `0x85` | `SESSION_ACK` | 8-byte challenge echo + `uint32` BE **session_id** (non-zero while active) + `uint32` BE **flags** (protocol version **1** in low bits) |
| `0x86` | `SESSION_END` | Empty (after `SESSION_CLOSE`) |
| `0x87` | `EEPROM_INFO` | `uint8` eeprom type (libdragon `eeprom_type_t`), `uint16` BE total 8-byte **blocks**, `uint8` `0` |
| `0x88` | `EEPROM_DATA` | `uint16` BE offset, `uint16` BE length, then **length** bytes |
| `0x89` | `EEPROM_STATUS` | `uint8` **status** (see §5) |
| `0x8A` | `SRAM_INFO` | `uint32` BE size in bytes (**0** if this ROM build has no SRAM window), `uint32` BE PI base (**0x08000000**) |
| `0x8B` | `SRAM_DATA` | `uint32` BE offset, `uint16` BE length, then **length** bytes |
| `0x8C` | `SRAM_STATUS` | `uint8` **status** (see §5) |
| `0x8D` | `RUMBLE_ACK` | `uint8` port, `uint8` duration echoed (capped at **255** in this byte; actual duration ≤ **600**), `uint8` **status** (rumble table in §5) |
| `0x8E` | `DISPLAY_TEXT_ACK` | `uint8` **status** (see §5) |
| `0xE1` | `STRESS_LARGE` | Pattern-filled body up to the maximum APPLICATION payload size (stress / fragmentation testing; cart-originated) |
| `0xF0` | `BENCH_TICK` | Optional: `uint32_t` BE frame counter (stress mode) |

---

## 4. `CONTROLLER` body (9 bytes)

| Offset | Type | Meaning |
|--------|------|---------|
| `0` | `uint32` BE | `joypad_buttons_t` raw bitmask (libdragon) |
| `4` | `int8` | Stick X |
| `5` | `int8` | Stick Y |
| `6`–`7` | `0` | Reserved / pad |
| `8` | `uint8` | Active port `0`–`3` (which controller was read) |

---

## 5. Save status codes (`EEPROM_STATUS` / `SRAM_STATUS` body byte)

| Code | Meaning |
|------|---------|
| `0x00` | OK |
| `0x01` | Not allowed — **session required** (writes only) |
| `0x02` | Bad parameters (range, alignment, length) |
| `0x03` | No save hardware / not available for this ROM build |

Third byte of **`RUMBLE_ACK`** (not the same semantics as save status codes above):

| Code | Meaning |
|------|---------|
| `0x00` | OK — rumble started |
| `0x01` | Rumble not supported on this port (controller / pak) |
| `0x02` | Bad parameters (port not `0`–`3`) |

**`DISPLAY_TEXT_ACK`** body byte (distinct from save/rumble tables above):

| Code | Meaning |
|------|---------|
| `0x00` | OK — text accepted (or cleared if request body was empty) |
| `0x01` | Body too long — exceeds **120** bytes; cart **does not** change the displayed string |

The cart may sanitize control characters (except newline and tab) for safe console output; UTF-8 code units **`0x20`** and above (except **`0x7F`**) are preserved.

---

## 6. Session rules

- **`SESSION_OPEN`** assigns a new non-zero **session_id** and echoes the **8-byte challenge** in **`SESSION_ACK`**.
- **`REQ_EEPROM_WRITE`** and **`REQ_SRAM_WRITE`** are rejected with status **`0x01`** until a session is opened; **`SESSION_CLOSE`** clears the session.
- Reads and **`REQ_EEPROM_INFO` / `REQ_SRAM_INFO` / `REQ_*_READ`** do **not** require a session.

---

## 7. `STRESS_LARGE` (`0xE1`)

Cart-originated stress frame: body is a deterministic byte pattern (`body[i] = i & 0xFF` for `i = 0 .. max-1`) filling the maximum M64T body allowed by the L3 APPLICATION payload size. The ROM may send the resulting wire in one USB write or split it across multiple writes to exercise host reassembly and fragmentation handling.

---

## 8. EEPROM / SRAM (hardware)

- **EEPROM** uses libdragon **`eeprom_*`**; the ROM image **save type** in the header must match EEPROM for hardware to respond (default **`multi64_test`** build uses **`eeprom4k`** in the Makefile).
- **SRAM** uses PI DMA at **`0x08000000`** with a size fixed at **build time** (`TEST_SRAM_BYTES`). Default release is **0** (no SRAM). Build with e.g. **`N64_ROM_SAVETYPE=sram256k`** to enable a **32 KiB** window — see **`n64/test-rom/Makefile`**.

---

## 9. Optional non-APPLICATION L3 (ROM diagnostics)

The test ROM may also emit **non-APPLICATION** L3 frames (e.g. `HEARTBEAT` on Control, small `DATA` on Log) for stack verification. Those frames are **not** `M64T` and are **out of band** for this document; hosts that only decode APPLICATION `M64T` may ignore them.

---

## 10. Relationship to **RAW_ECHO** mode

- The same **`multi64_test.z64`** binary implements **M64T** (this document) and a separate **RAW_ECHO** mode (default at boot): verbatim `MULTI64_L3` loopback with **no** `M64B` application framing. Use **RAW_ECHO** with serial L2 e2e tools such as **`sc64-l3-framing-e2e`** / **`sc64-echo-test`** (**SC64** reference backend in the crate names).

---

## 11. Revision

**Spec-Revision** counts edits to **this** document only. It is **independent** of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | **M64T** / **`multi64_test.z64`** application profile (includes **RAW_ECHO** vs **M64T_PROTO** in §10); pre-release tree. |

**Normative compatibility (preserved):** `M64T` magic bytes, `msg` opcodes, and payload layouts in §1–8 are **stable for interop** between the official test ROM and hosts; clarifications MUST NOT change wire bytes without a coordinated bump called out in text.

**Spec-Revision policy:** All normative docs use **Spec-Revision** **1** for now; edits **accumulate** under **1** until maintainers announce a bump. This is independent of L3 **Protocol-Major** / **Protocol-Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).
