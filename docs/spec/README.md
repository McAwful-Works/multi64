# Specifications (`docs/spec/`)

Normative **Multi64** wire and behavior. Each spec file carries its own **Spec-Revision** until maintainers bump it.

**SC64** L2 is complete in-tree; **EverDrive X7** L2 is **draft** / **stub** until the host mapping is finalized.

---

## Documents

| Document | Description |
|----------|-------------|
| [**l3-bridge-protocol-v1.md**](l3-bridge-protocol-v1.md) | **L3 v1**: `M64B` framing, handshake, session layer (`FEATURES`), channels, errors |
| [**l2-link-adapter.md**](l2-link-adapter.md) | **L2**: responsibilities of a host-side link and constraints for backends |
| [**l3-over-sc64.md**](l3-over-sc64.md) | **SC64**: L3 octets over `USB_WRITE` / `PKT` `U` |
| [**sc64-sd-usb-host.md**](sc64-sd-usb-host.md) | **SC64**: host SD / FAT over USB (`SD_CARD_OP`); normative for `multi64-sc64-sd` |
| [**ed64-sd-usb-host.md**](ed64-sd-usb-host.md) | **EverDrive**: experimental X-series SD via **`RomRead`**; **`usb64`** vs **edlink** notes |
| [**l3-over-everdrive-x7.md**](l3-over-everdrive-x7.md) | **EverDrive X7** (draft): L3 over ED64 USB — host mapping TBD |
| [**daemon-api-v1.md**](daemon-api-v1.md) | **`multi64d`**: HTTP + WebSocket bridge |
| [**test-l3-application-v0.md**](test-l3-application-v0.md) | **M64T**: **`multi64_test.z64`** (`n64/test-rom`) host↔cart messages |
| [**xfer64-cart-serial.md**](xfer64-cart-serial.md) | **Xfer64**: SD sessions, `multi64d` COM yield, Rust module map (maintainers) |

---

## Implementation map (Rust)

| Spec area | Crate(s) |
|-----------|----------|
| L3 framing / decode | `multi64-l3` (`crates/l3`) |
| SC64 wire (`CMD` / `CMP` / `PKT`) | `multi64-sc64-link` (`crates/sc64-link`) |
| L3 stream over SC64 | `multi64-sc64-l2` (`crates/sc64-l2`) |
| L3 stream over EverDrive X7 | `multi64-ed64-l2` (`crates/ed64-l2`) — **stub** |
| Daemon API | `multi64d` (`crates/multi64d`) |
| SC64 SD / FAT host (`SD_CARD_OP`, …) | `multi64-sc64-sd` (`crates/multi64-sc64-sd`) — see [`sc64-sd-usb-host.md`](sc64-sd-usb-host.md) |
| EverDrive **`usb64`** + **edlink** serial helpers | `multi64-ed64-link` (`crates/multi64-ed64-link`) — see [`ed64-sd-usb-host.md`](ed64-sd-usb-host.md) |

### Host binaries (hardware / CLI)

These are **not** alternate L3 specs; they hit **USB serial** on a cart. **`multi64d`** is the long-lived bridge; the rest are one-shot tools.

| Role | SummerCart64 | EverDrive X7 |
|------|----------------|--------------|
| Vendor / link smoke (not L3) | `sc64-smoke` | `ed64-smoke` ([§8](l3-over-everdrive-x7.md)) |
| Raw L3 vs ROM **RAW_ECHO** | `sc64-echo-test` | `ed64-echo-test` (needs `ed64-l2`) |
| L3 framing vs **RAW_ECHO** | `sc64-l3-framing-e2e` | `ed64-l3-framing-e2e` (needs `ed64-l2`) |

---

## Maintenance

- Routine edits keep each file’s **Spec-Revision** until a coordinated bump.
- L3 **Protocol-Major** / **Protocol-Minor**: change only when the **normative byte contract** changes ([`l3-bridge-protocol-v1.md`](l3-bridge-protocol-v1.md) §12).

---

## More documentation

- [Documentation hub](../README.md) — map + [flash carts](../README.md#flash-carts-l2-backends)  
- [Repository README](../../README.md) — quick start, crate list
