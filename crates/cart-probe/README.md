# multi64-cart-probe

Tells a **SummerCart64**, **EverDrive-64 PRO** and **EverDrive-64 X7** apart on a serial port. Multi64's and Xfer64's **Auto** cart settings both use it.

It works in two tiers, in this order:

1. **USB descriptors** (`usb_is_sc64`). Writes nothing, so it is safe on any port and works while `multi64d` holds the cart's port. Only a SummerCart64 can be recognised this way: FTDI `0403:6014` with an `SC64…` serial number (Windows) or `SC64` product string.
2. **Identity probes** (`probe_port`). Sends SC64 `IDENTIFIER_GET`, then the PRO's edlink handshake at 921600 baud, then the X-series `usb64` test. The device on the other end receives those bytes. A port held by another process cannot be opened and reports nothing. A port with no cart costs about three seconds.

The EverDrive probes follow [`ed64-pro-usb-host.md`](../../docs/spec/ed64-pro-usb-host.md) §4 and [`l3-over-everdrive-x7.md`](../../docs/spec/l3-over-everdrive-x7.md) §8. **They have never been run against a cart.** The SC64 probe and descriptor match have.

Host-only unit tests cover the descriptor match and reply parsing. Nothing here is exercised against hardware in CI.
