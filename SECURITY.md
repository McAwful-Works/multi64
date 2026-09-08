# Security

Multi64 is open source and maintained on a best-effort basis. For development workflow and where to change code, see [**`CONTRIBUTING.md`**](CONTRIBUTING.md).

If you discover a security issue that should not be discussed in public (for example, a serious flaw in the WebSocket bridge or protocol handling), please use **GitHub’s private vulnerability reporting** for this repository if it is enabled, or open a **draft security advisory** with maintainers. For general bugs and discussion, use public **Issues**.

The reference daemon (`multi64d`) is intended for **local development** (default bind `127.0.0.1`). Do not expose it to untrusted networks without additional controls.

## Browser origins

Binding to loopback does not put the daemon out of reach: it writes directly to flash-cart hardware, and any page in a browser on the same machine can reach `127.0.0.1`. CORS does not prevent that on its own — a WebSocket upgrade is not subject to the CORS response gate, and `POST /v1/serial/release` is a CORS *simple* request that needs no preflight, so its side effect lands even though the page cannot read the reply.

Every route is therefore gated on the `Origin` header:

- **No `Origin`** — allowed. Native clients (Xfer64, Multi64, `multi64-test-connector`) send none.
- **An `Origin`** — allowed only if that exact origin is configured via `--allow-origin` / `MULTI64D_ALLOW_ORIGIN` / `allow_origin`. Otherwise **`403 Forbidden`** before routing, `/ws` included.
- The allow-list is **empty by default**.

**This is not authentication.** Any native process on the host can still reach the daemon, exactly as it could open the COM port directly. The guard addresses drive-by access from a browser, nothing more. Full detail: [`daemon-api-v1.md` §1.4](docs/spec/daemon-api-v1.md).
