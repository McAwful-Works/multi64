# Documentation

Normative wire rules live in **`spec/`**. Each spec file carries its own **Spec-Revision** until maintainers bump it.

---

## Start here

| If you want… | Read |
|--------------|------|
| Repo overview | [Root README](../README.md) |
| **L3** | [l3-bridge-protocol-v1.md](spec/l3-bridge-protocol-v1.md) |
| **RDRAM peek/poke** | [memory-l3-application-v0.md](spec/memory-l3-application-v0.md) |
| **Put the M64P agent in a game ROM** | [integration/README.md](integration/README.md), [n64/agent](../n64/agent/README.md) |
| **L2** (any cart) | [l2-link-adapter.md](spec/l2-link-adapter.md) |
| **SC64** L3 over USB | [l3-over-sc64.md](spec/l3-over-sc64.md) |
| **SC64** SD / FAT host | [sc64-sd-usb-host.md](spec/sc64-sd-usb-host.md) |
| **EverDrive X7** L3 (draft) | [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md), [ed64-l2 README](../crates/ed64-l2/README.md) |
| **EverDrive-64 PRO** L3 (draft) | [l3-over-everdrive-pro.md](spec/l3-over-everdrive-pro.md), [ed64pro-l2 README](../crates/ed64pro-l2/README.md) |
| **EverDrive** SD notes | [ed64-sd-usb-host.md](spec/ed64-sd-usb-host.md) |
| Cart comparison | [Flash carts (L2)](#flash-carts-l2-backends) below |
| **Xfer64** (SD / COM, maintainers) | [xfer64-cart-serial.md](spec/xfer64-cart-serial.md) |
| **`multi64d`** | [daemon-api-v1.md](spec/daemon-api-v1.md), [run-as-service.md](run-as-service.md) |
| Test ROM + connector | [connectors/test-rom.md](connectors/test-rom.md) |
| Frontend themes / tokens (Multi64, Xfer64) | [frontend-appearance.md](frontend-appearance.md) |
| Build **`multi64_test.z64`** | [n64/README](../n64/README.md) |

---

## Flash carts (L2 backends)

L3 is cart-agnostic; **L2** is per device. **SC64** is the reference implementation here; **EverDrive X7** and **EverDrive-64 PRO** L2 are implemented but have never been run against a cart.

Krikzz's N64 carts fall into two USB families: the **X-series** (X7, 3.0; X5 has no USB), which speaks `usb64` plus `DMA@` framing, and the **EverDrive-64 PRO** (August 2026), which speaks [edlink](https://github.com/krikzz/edlink) Gen3 — see [ed64-pro-usb-host.md](spec/ed64-pro-usb-host.md). Read [l3-over-everdrive-x7.md §1.1](spec/l3-over-everdrive-x7.md#11-which-everdrives-this-can-apply-to) before starting any EverDrive work.

| | **SummerCart64** | **EverDrive-64 X7** |
|---|------------------|---------------------|
| **L2 spec** | [l3-over-sc64.md](spec/l3-over-sc64.md) | [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md) |
| **Rust L2** | `multi64-sc64-l2` | `multi64-ed64-l2` (implemented, **unvalidated on hardware**) |
| **USB smoke** (not L3) | `sc64-smoke` | `ed64-smoke` ([§8](spec/l3-over-everdrive-x7.md)) |
| **Serial e2e** | `sc64-echo-test`, `sc64-l3-framing-e2e` | `ed64-echo-test`, `ed64-l3-framing-e2e` (run, but **unvalidated on hardware**) |
| **`multi64d`** | SC64 L2 (default) | `--cart ed64` (experimental, never run against a cart) |

The **EverDrive-64 PRO** has its own L2, [l3-over-everdrive-pro.md](spec/l3-over-everdrive-pro.md), implemented by `multi64-ed64pro-l2` (`multi64d --cart ed64pro`) and the test ROM's `ed64pro.c`, **unvalidated on hardware**. Its tools are `ed64pro-echo-test` and `ed64pro-l3-framing-e2e` (§9); opening its link runs the edlink handshake, so it needs no separate smoke tool. Neither has run on a cart.

The test ROM's hardware runs are recorded in [`n64/README.md`](../n64/README.md#hardware-record).

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
| [l3-over-everdrive-pro.md](spec/l3-over-everdrive-pro.md) | EverDrive-64 PRO L3 (draft) |
| [xfer64-cart-serial.md](spec/xfer64-cart-serial.md) | Xfer64: SD sessions, `multi64d` COM yield, Rust module map |
| [daemon-api-v1.md](spec/daemon-api-v1.md) | `multi64d` HTTP + WebSocket |
| [test-l3-application-v0.md](spec/test-l3-application-v0.md) | M64T / `multi64_test.z64` |
| [memory-l3-application-v0.md](spec/memory-l3-application-v0.md) | M64P: RDRAM peek/poke over L3 |

**Index:** [spec/README.md](spec/README.md)

### Reading order (implementors)

1. [l3-bridge-protocol-v1.md](spec/l3-bridge-protocol-v1.md)  
2. [l3-over-sc64.md](spec/l3-over-sc64.md)  
3. [l3-over-everdrive-x7.md](spec/l3-over-everdrive-x7.md), [l3-over-everdrive-pro.md](spec/l3-over-everdrive-pro.md) (optional)  
4. [daemon-api-v1.md](spec/daemon-api-v1.md)  
5. [test-l3-application-v0.md](spec/test-l3-application-v0.md)  
6. [memory-l3-application-v0.md](spec/memory-l3-application-v0.md) (optional — RDRAM peek/poke)  
7. [sc64-sd-usb-host.md](spec/sc64-sd-usb-host.md), [ed64-sd-usb-host.md](spec/ed64-sd-usb-host.md), [xfer64-cart-serial.md](spec/xfer64-cart-serial.md) — SD / Xfer64 internals

---

## ROM integration (`integration/`)

Guides, not specifications: how to make a game on real hardware readable and writable through
[M64P](spec/memory-l3-application-v0.md), using the agent in [`n64/agent`](../n64/agent/README.md).

| Document | Purpose |
|----------|---------|
| [integration/README.md](integration/README.md) | Overview, the three questions to answer first, checklist |
| [integration/cart-agent.md](integration/cart-agent.md) | The agent's contract, what it does to the machine, building it |
| [integration/placing-the-agent.md](integration/placing-the-agent.md) | RAM, per-frame hook, ROM location, loader, CRC — with and without a decomp |
| [integration/host-connector.md](integration/host-connector.md) | Writing an emulator-API stand-in on the PC |
| [integration/testing.md](integration/testing.md) | Verification order, and the traps met |

---

## Connectors (`connectors/`)

Host programs that use **`multi64d`**’s **WebSocket** for **raw L3**. The cart link is whatever **`multi64d`** is built with (**SC64 L2** today; the EverDrive [X7](spec/l3-over-everdrive-x7.md) and [PRO](spec/l3-over-everdrive-pro.md) mappings are still draft).

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
