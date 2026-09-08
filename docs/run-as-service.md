# Running `multi64d` as a background service

> **See also:** [Daemon API](spec/daemon-api-v1.md) · [Documentation map](README.md) · [Flash carts (L2)](README.md#flash-carts-l2-backends)

The reference daemon binds to **`127.0.0.1:38765`** by default (see [`spec/daemon-api-v1.md`](spec/daemon-api-v1.md)). Use a config file or `MULTI64D_LISTEN` if you need a different address or port.

## Configuration

- **Working-directory config:** `multi64d.toml` next to where you start the process.
- **Per-user config:** `multi64d/config.toml` under the OS config directory (Linux `~/.config`, Windows `%APPDATA%`, macOS `~/Library/Application Support`).
- **Explicit path:** `--config` or `MULTI64D_CONFIG`.

Copy [`crates/multi64d/multi64d.toml.example`](../crates/multi64d/multi64d.toml.example) and set `serial` to your cart’s COM or `/dev/tty*` device.

## A missing cart is not a startup failure

This matters most when supervising the process. If the configured serial device cannot be opened at startup — the usual case at boot, when the cart is unplugged or powered off — the daemon **logs a warning and starts anyway** rather than exiting. HTTP and the WebSocket come up immediately, `GET /` reports `serialActive: false`, and the daemon reopens the port on its own within about a second of the cart appearing.

Two consequences for a service unit:

- **`Restart=on-failure` will not fire for a missing cart**, and does not need to. The process stays up and waits; restarting it would accomplish nothing that the built-in retry does not.
- **"Running" is not "has a cart".** Health checks against `/health` answer `{"status":"ok"}` whenever the process is alive, including while the link is down. Anything that needs to know the cart is present must read **`serialActive`** from `GET /`.

See [`daemon-api-v1.md` §1.3](spec/daemon-api-v1.md) for the faulted-link contract and §1.3.1 for the startup case.

## Linux / Steam Deck

1. Install the binary (e.g. `cargo install --path crates/multi64d` or copy from `target/release/`).
2. Ensure your user can open the serial device (dialout group, udev rules for your USB flash cart if needed).
3. Run under **systemd** (user or system unit). Example **user** service `~/.config/systemd/user/multi64d.service`:

```ini
[Unit]
Description=Multi64 WebSocket bridge (L3 / flash cart)
After=network.target

[Service]
Type=simple
ExecStart=%h/.cargo/bin/multi64d --config %h/.config/multi64d/config.toml
Restart=on-failure
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
```

Then: `systemctl --user daemon-reload`, `systemctl --user enable --now multi64d.service`.

Health check: `curl -s http://127.0.0.1:38765/health`.

## Windows

1. Build or copy `multi64d.exe`.
2. Place `%APPDATA%\multi64d\config.toml` (or use `--config`) with `serial = "COMx"`.
3. Run via **Task Scheduler** (trigger: at log on), or **NSSM** / **WinSW** to wrap the executable as a Windows service. Use `Restart` on failure and the same `ExecStart` arguments you would use in a shell.

For development, a terminal window is enough; for a stable machine-daemon, prefer Task Scheduler or a service wrapper so the process restarts after crashes.

## macOS

Same idea as Linux: `launchd` `LaunchAgent` in `~/Library/LaunchAgents/` with `ProgramArguments` pointing at `multi64d` and optional `--config`. Use `curl` on `/health` for monitoring.
