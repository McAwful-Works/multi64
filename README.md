<img src="branding/multi64.svg" alt="" width="72" align="left" />

# Multi64

**N64 flash cart bridge** · PC ↔ Nintendo 64 over USB

<br clear="left" />

Talk to a Nintendo 64 from your PC over the USB cable a flash cart is already using. Three
Windows apps sit on top of it, each installed on its own:

| App | What it does |
|-----|--------------|
| [**Multi64**](crates/multi64/README.md) | Runs the bridge the others talk through. Pick a serial port, start it, leave it in the tray. |
| [**Xfer64**](crates/xfer64/README.md) | Browse and copy the files on the cart's SD card from Windows, with no drive letter. |
| [**AP64**](crates/ap64/README.md) | Play Archipelago seeds on a real console: adds the cart agent to a patched seed, then connects Archipelago's client to the game. |

AP64 needs Multi64's bridge running. Xfer64 talks to the cart itself: when the bridge is up,
Xfer64 has it let go of the cart for each operation and hand it back afterward.

## Which carts work

**SummerCart64** is the one proven on hardware, and what everything here is tested against.

**EverDrive-64 X7** and **EverDrive-64 PRO** can be selected, and the code for both is written,
but neither has been validated against a cart. The X7 mapping has run on exactly one, where the
cart could not send while the host was sending; the PRO's has never run on one at all, and
unlike the X7 it was designed here rather than transcribed from a working reference. Treat both
as experiments. What would have to be answered is written down:
[X7](docs/spec/l3-over-everdrive-x7.md) §4.5, [PRO](docs/spec/l3-over-everdrive-pro.md).

## For developers

The bridge is three layers: cart-agnostic **L3** framing, a per-cart **L2** adapter over
USB serial, and `multi64d`, a daemon exposing raw L3 octets over HTTP and a WebSocket.

```sh
cargo run -p multi64d --release -- --serial COM3
```

It listens on `http://127.0.0.1:38765/`, with L3 bytes as WebSocket frames at
`ws://127.0.0.1:38765/ws`. To keep one running, see [running it as a service](docs/run-as-service.md).

| Where | What is in it |
|-------|---------------|
| [`docs/README.md`](docs/README.md) | The map, and the order to read it in |
| [`docs/spec/`](docs/spec/) | Normative protocols: [L3](docs/spec/l3-bridge-protocol-v1.md), [the L2 contract](docs/spec/l2-link-adapter.md), [the daemon API](docs/spec/daemon-api-v1.md) |
| [`docs/integration/`](docs/integration/) | Putting the M64P agent into a game ROM |
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Building, the crate inventory, and the conventions |
| [`n64/`](n64/README.md) | The test ROM and the game-resident agent |

The test ROM is how the stack is exercised without a game. Build
[`n64/test-rom/`](n64/test-rom/) into `multi64_test.z64`, boot it, and run the whole suite at
once:

```sh
cargo run -p multi64-test-connector --release -- suite
```

**Multi64 Test** ([`crates/multi64-test-app`](crates/multi64-test-app/README.md)) runs the same
checks in a window, as one portable exe for handing to a tester. Neither needs the controller.

Documentation has a [house style](docs/documentation-style.md), and the marks the apps use are
in [`branding/`](branding/README.md).

## License

`MIT OR Apache-2.0` — take either, the same pattern as the Rust compiler and standard library.
See [LICENSE](LICENSE), [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE) and the
Apache [NOTICE](NOTICE). For sensitive issues, [SECURITY.md](SECURITY.md).
