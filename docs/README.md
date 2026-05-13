# Documentation

Normative wire rules live in **`spec/`**. Each spec file carries its own **Spec-Revision** until maintainers bump it.

---

## Start here

| If you want… | Read |
|--------------|------|
| Repo overview | [Root README](../README.md) |
| **L3** | [l3-bridge-protocol-v1.md](spec/l3-bridge-protocol-v1.md) |
| **L2** (any cart) | [l2-link-adapter.md](spec/l2-link-adapter.md) |
| **SC64** L3 over USB | [l3-over-sc64.md](spec/l3-over-sc64.md) |
| **SC64** SD / FAT host | [sc64-sd-usb-host.md](spec/sc64-sd-usb-host.md) |
| **EverDrive X7** L3 (draft) | [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md), [ed64-l2 README](../crates/ed64-l2/README.md) |
| **EverDrive** SD notes | [ed64-sd-usb-host.md](spec/ed64-sd-usb-host.md) |
| Cart comparison | [Flash carts (L2)](#flash-carts-l2-backends) below |
| **Xfer64** (SD / COM, maintainers) | [xfer64-cart-serial.md](spec/xfer64-cart-serial.md) |
| **`multi64d`** | [daemon-api-v1.md](spec/daemon-api-v1.md), [run-as-service.md](run-as-service.md) |
| Test ROM + connector | [connectors/test-rom.md](connectors/test-rom.md) |
| Build **`multi64_test.z64`** | [n64/README](../n64/README.md) |

---

## Flash carts (L2 backends)

L3 is cart-agnostic; **L2** is per device. **SC64** is the reference implementation here; **EverDrive X7** is draft / partial.

| | **SummerCart64** | **EverDrive-64 X7** |
|---|------------------|---------------------|
| **L2 spec** | [l3-over-sc64.md](spec/l3-over-sc64.md) | [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md) |
| **Rust L2** | `multi64-sc64-l2` | `multi64-ed64-l2` (**stub**) |
| **USB smoke** (not L3) | `sc64-smoke` | `ed64-smoke` ([§8](spec/l3-over-everdrive-x7.md)) |
| **Serial e2e** | `sc64-echo-test`, `sc64-l3-framing-e2e` | `ed64-echo-test`, `ed64-l3-framing-e2e` (needs **`Ed64L2Pipe`**) |
| **`multi64d`** | SC64 L2 | Not wired |

---

## Specifications (`spec/`)

Bump L3 **Protocol-Major** / **Protocol-Minor** only when the **byte** contract changes ([l3-bridge-protocol-v1.md](spec/l3-bridge-protocol-v1.md) §12).

| Document | Purpose |
|----------|---------|
| [l3-bridge-protocol-v1.md](spec/l3-bridge-protocol-v1.md) | L3: `M64B`, handshake, sessions, channels |
| [l2-link-adapter.md](spec/l2-link-adapter.md) | L2 host responsibilities |
| [l3-over-sc64.md](spec/l3-over-sc64.md) | SC64 L3 path |
| [sc64-sd-usb-host.md](spec/sc64-sd-usb-host.md) | SC64 SD over USB |
| [ed64-sd-usb-host.md](spec/ed64-sd-usb-host.md) | EverDrive SD / serial notes |
| [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md) | EverDrive X7 L3 (draft) |
| [xfer64-cart-serial.md](spec/xfer64-cart-serial.md) | Xfer64: SD sessions, `multi64d` COM yield, Rust module map |
| [daemon-api-v1.md](spec/daemon-api-v1.md) | `multi64d` HTTP + WebSocket |
| [test-l3-application-v0.md](spec/test-l3-application-v0.md) | M64T / `multi64_test.z64` |

**Index:** [spec/README.md](spec/README.md)

### Reading order (implementors)

1. [l3-bridge-protocol-v1.md](spec/l3-bridge-protocol-v1.md)  
2. [l3-over-sc64.md](spec/l3-over-sc64.md)  
3. [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md) (optional)  
4. [daemon-api-v1.md](spec/daemon-api-v1.md)  
5. [test-l3-application-v0.md](spec/test-l3-application-v0.md)  
6. [sc64-sd-usb-host.md](spec/sc64-sd-usb-host.md), [ed64-sd-usb-host.md](spec/ed64-sd-usb-host.md), [xfer64-cart-serial.md](spec/xfer64-cart-serial.md) — SD / Xfer64 internals

---

## Connectors (`connectors/`)

Host programs that use **`multi64d`**’s **WebSocket** for **raw L3**. The cart link is whatever **`multi64d`** is built with (**SC64 L2** today; **EverDrive** still [draft](spec/l3-over-everdrive-x7.md)).

| Document | Programs |
|----------|----------|
| [test-rom.md](connectors/test-rom.md) | **`multi64-test-connector`** (+ optional GUI) ↔ **`multi64_test.z64`** ([`n64/test-rom`](../n64/README.md)) |

WebSocket contract: [daemon-api-v1.md](spec/daemon-api-v1.md). Start **`multi64d`** before running a connector.

---

## Operations

| Document | Topic |
|----------|-------|
| [run-as-service.md](run-as-service.md) | `multi64d` as a service |

---

## Contributing

[CONTRIBUTING.md](../CONTRIBUTING.md) — build, layout, style.
