# Reference daemon API (v1)

**Spec-Revision:** 1  

This document describes **`multi64d`**, the reference PC daemon: HTTP metadata + health + **WebSocket** bridge to the **L3 octet stream** between host and N64.

The daemon is **L3-facing**: clients send and receive **raw L3 bytes** on the WebSocket binary channel. How those bytes move over USB/serial is **L2** and depends on the cart. The **reference build** uses the **SummerCart64** mapping ([`l3-over-sc64.md`](./l3-over-sc64.md)). Other carts (e.g. **EverDrive** — [`l3-over-everdrive-x7.md`](./l3-over-everdrive-x7.md)) should present the **same L3 stream** at this boundary once implemented.

---

## 1. Transport

- **HTTP** `GET /` — JSON metadata (service name, version, WebSocket path, configured serial, `serialActive`).
- **HTTP** `GET /health` — minimal JSON `{"status":"ok"}` for load balancers and process supervisors.
- **HTTP** `POST /v1/serial/release` — drop the COM port so another process (e.g. Xfer64) can open it; `serialActive` in `GET /` becomes `false` until resume.
- **HTTP** `POST /v1/serial/resume` — reopen the configured serial device and restore normal operation.
- **WebSocket** `GET /ws` — bidirectional messages after upgrade.

Default listen address: **`127.0.0.1:38765`** (configurable via `--listen` / `MULTI64D_LISTEN` / config file).

### 1.1 `GET /` (JSON)

| Field | Type | Meaning |
|-------|------|--------|
| `service` | string | Always `"multi64d"`. |
| `version` | string | Daemon binary version. |
| `websocket_path` | string | Path for the WebSocket upgrade (e.g. `"/ws"`). |
| `serial` | string | Configured serial device path (e.g. `COM3`); empty in the metadata-only test router. |
| `serialActive` | boolean | `true` when the daemon holds an open serial link; `false` after **`POST /v1/serial/release`** until **`POST /v1/serial/resume`**. |

### 1.2 Serial yield (Xfer64)

Only one process can open the cart’s COM port at a time. Tools such as **Xfer64** may **`GET /`** (compare `serial` to the port they need and check `serialActive`), then **`POST /v1/serial/release`** before opening the port locally, and **`POST /v1/serial/resume`** when finished.

| Endpoint | Success body |
|----------|----------------|
| `POST /v1/serial/release` | `{"released":true}` |
| `POST /v1/serial/resume` | `{"resumed":true}` |

`POST /v1/serial/resume` is **idempotent**: if the link is already active (e.g. nested release/resume from Xfer64), the handler succeeds without opening a second serial handle.

While released, WebSocket binary writes to the cart are ignored; clients should tolerate brief disconnect-like behavior until resume.

---

## 2. WebSocket: binary vs text

| Direction | Format | Meaning |
|-----------|--------|--------|
| Server → Client | **Binary** | Raw **L3 stream octets** from the N64 (order-preserving). **SC64 realization:** concatenation of `PKT` `U` / `MULTI64_L3` payloads per [`l3-over-sc64.md`](./l3-over-sc64.md). Clients run [`multi64-l3`](../../crates/l3) framing / `StreamDecoder` on this byte stream. |
| Client → Server | **Binary** | Raw **L3 stream octets** to the N64. **SC64 realization:** host sends `USB_WRITE` chunks per [`l3-over-sc64.md`](./l3-over-sc64.md). |
| Either | **Text (JSON)** | Optional control messages (see §3). |

There is **no** base64 wrapper in v1; use binary frames for throughput.

---

## 3. JSON control messages (text frames)

### 3.1 Server hello

Immediately after the WebSocket handshake, the server sends one **text** frame:

```json
{"type":"hello","service":"multi64d","version":"0.1.0","docs":"docs/spec/daemon-api-v1.md"}
```

(`version` matches the built `multi64d` binary.)

### 3.2 Ping / pong

Client may send:

```json
{"type":"ping"}
```

Server responds with:

```json
{"type":"pong"}
```

---

## 4. Semantics

- **Ordering:** Binary chunks from the cart are sent to the client **in order**. Multiple WebSocket clients receive a **broadcast** of the same outbound binary chunks (fan-out).
- **Inbound:** Any client may send binary data; writes are **serialized** against the single serial link (mutex + blocking I/O).
- **Backpressure:** Slow WebSocket clients may **lag**; the server uses a bounded broadcast buffer and may **drop** old outbound chunks (`Lagged`); clients should keep up or reconnect.

---

## 5. CLI and configuration (reference)

### 5.1 Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--config <path>` | (none) | TOML config file; env: `MULTI64D_CONFIG` |
| `--serial` | (required†) | Serial device (e.g. `COM3`, `/dev/ttyACM0`); env: `MULTI64D_SERIAL` |
| `--baud` | `115200` | Baud (often ignored on USB-CDC cart adapters); env: `MULTI64D_BAUD` |
| `--listen` | `127.0.0.1:38765` | TCP bind address; env: `MULTI64D_LISTEN` |
| `--clear-serial` | off | Clear host serial buffers after open; env: `MULTI64D_CLEAR_SERIAL` (`true` / `false`) |
| `--no-print-ports` | off | If set, do not log available serial ports at startup (default is to log them at info) |
| `--list-ports` | off | Print serial port names to stdout and exit (for scripts) |

† Serial may come from **`--serial`**, **`MULTI64D_SERIAL`**, or **`serial = "..."`** in a config file (see §5.2). CLI and environment override file values.

Example config: [`crates/multi64d/multi64d.toml.example`](../../crates/multi64d/multi64d.toml.example).

### 5.2 Config file discovery

If `--config` / `MULTI64D_CONFIG` is not set, the daemon loads the first file that exists:

1. `./multi64d.toml` (current working directory)
2. OS config directory: `multi64d/config.toml` — e.g. Linux `~/.config/multi64d/config.toml`, Windows `%APPDATA%\multi64d\config.toml`, macOS `~/Library/Application Support/multi64d/config.toml`

### 5.3 Logging

Environment: **`RUST_LOG`** (e.g. `info`, `multi64d=debug`) for tracing.

---

## 6. Revision

**Spec-Revision** counts edits to **this** document only. It is **independent** of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | Daemon API (`multi64d`): HTTP, WebSocket, JSON control; pre-release tree. |

**Spec-Revision policy:** All normative docs use **Spec-Revision** **1** for now; edits **accumulate** under **1** until maintainers announce a bump. This is independent of L3 **Protocol-Major** / **Protocol-Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).
