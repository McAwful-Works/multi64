# L3 APPLICATION — RDRAM peek/poke (M64P, v0)

**Spec-Revision:** 1  

Payloads carried in L3 **`DATA`** frames on **`CHANNEL = APPLICATION` (`0x00`)** that let a host read and write **console RDRAM** while a ROM runs.

**Magic:** ASCII **`M64P`** — bytes `0x4D 0x36 0x34 0x50`.

Deliberately **game-agnostic**: it exposes bytes at addresses and nothing else. The cart is a PI-bus slave and cannot reach RDRAM on its own, so every request is serviced by code running on the console. Where that code lives is out of scope here — [`../../n64/README.md`](../../n64/README.md) implements it as a mode of the test ROM; a game-resident agent would implement the same messages.

M64P shares the APPLICATION channel with **`M64T`** ([`test-l3-application-v0.md`](./test-l3-application-v0.md)); receivers dispatch on the 4-byte magic.

---

## 1. Payload layout

| Offset | Size | Description |
|--------|------|-------------|
| `0`–`3` | 4 | Magic **`M64P`** |
| `4` | 1 | `msg` — request or response code (§2–§3) |
| `5`– | * | Body; length = `L3_PAYLOAD_LEN - 5` |

`L3_PAYLOAD_LEN` is the APPLICATION payload length from the L3 header ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md)). All multi-byte integers are **big-endian**.

Every request carries a **`rid`** (`uint16`), echoed in its response. A host may therefore match a reply after an L3 resync; `rid` is opaque to the cart.

---

## 2. Host → cart (requests)

| `msg` | Name | Body |
|-------|------|------|
| `0x01` | `HELLO` | Empty |
| `0x02` | `PEEKV` | `rid:uint16`, `n:uint8`, then `n` × (`addr:uint32`, `len:uint16`) |
| `0x03` | `POKEV` | `rid:uint16`, `n:uint8`, then `n` × (`addr:uint32`, `len:uint16`, `len` bytes) |

`HELLO` has no `rid`; `HELLO_ACK` carries none either.

---

## 3. Cart → host (responses)

| `msg` | Name | Body |
|-------|------|------|
| `0x81` | `HELLO_ACK` | `proto:uint8`, `agent_ver:uint16`, `rdram_bytes:uint32`, `flags:uint8` |
| `0x82` | `PEEKV_RESP` | `rid:uint16`, `n:uint8`, then `n` × (`len:uint16`, `len` bytes) — same order as the request |
| `0x83` | `POKE_ACK` | `rid:uint16`, `applied:uint8` — count of regions written |
| `0xE0` | `ERR` | `rid:uint16`, `code:uint8` (§5) |

`proto` is **0** for this revision. `flags` bit `0` set means writes are accepted; a read-only agent clears it and answers `POKEV` with `ERR`/`E_READONLY`.

---

## 4. Addressing and limits

Addresses are **RDRAM physical offsets** — `0` is the start of RDRAM, not a KSEG0 virtual address. This is the convention N64 debuggers and memory tools already use, so a host needs no translation layer.

| Limit | Value | Why |
|-------|-------|-----|
| `n` per request | **32** | Enough to gather scattered state in one round trip |
| Bytes per request or response | **7936** | Keeps the whole frame inside one 8192-byte L3 payload, and so one `usb_write` |
| `len` per region | **4096** | Bounds a single copy inside a frame; also fits a whole save context in one region |

The 7936 comes from the worst case, which is a `POKEV` **request**: its per-region
header is 6 bytes against the 2 in a `PEEKV` response, so the wire cost is
`8 + 6n + total`. At `n = 32` that is `200 + total`, leaving 7992 under an 8192-byte
payload; 7936 takes the round number below it.

Round-trip latency dominates transfer time by orders of magnitude — measured at ~67 ms
per exchange on SC64 hardware, near-flat in payload size, because it is set by the
ROM's per-frame polling rather than the link. Requests should therefore be packed as
full as these limits allow; a caller that splits work across more exchanges than
necessary pays ~67 ms for each one.

A request that exceeds any limit is answered with `ERR`, not truncated. `addr + len` beyond RDRAM is `E_RANGE` — the cart must range-check rather than fault, since a bad address from the host would otherwise bus-error the console.

`n = 0` is legal and returns an empty `PEEKV_RESP` / `POKE_ACK` with `applied = 0`.

### 4.1 Consistency

All regions in one request are serviced **in a single pass, from the ROM's per-frame hook**, never from an interrupt. Every read in a `PEEKV` therefore observes one consistent point in the frame, and a `POKEV` lands entirely between frames rather than mid-update.

A host may therefore treat one request as atomic with respect to the running ROM. That property comes from the **hook site**, not the transport: an implementation that services M64P from an interrupt does not satisfy this section even though its wire format is identical.

Access is through **cached KSEG0**. The game manipulates its own structures with the CPU, so cached access is what stays coherent; uncached reads can return data the CPU has not written back.

---

## 5. Error codes

| Code | Name | Meaning |
|------|------|---------|
| `0x01` | `E_MALFORMED` | Body shorter than the declared regions require |
| `0x02` | `E_TOO_MANY` | `n` exceeds 32 |
| `0x03` | `E_TOO_LARGE` | Region or total exceeds the §4 limits |
| `0x04` | `E_RANGE` | `addr + len` outside RDRAM |
| `0x05` | `E_READONLY` | `POKEV` on an agent that does not accept writes |

---

## 6. Revision

| Spec-Revision | Change |
|---------------|--------|
| **1** | M64P v0: `HELLO`, `PEEKV`, `POKEV`. No change to the L3 byte contract — this is an APPLICATION payload, so **Protocol-Major/Minor are unaffected**. Per-request byte cap set to 7936 (§4) after hardware measurement showed latency is per-exchange, not per-byte. |
