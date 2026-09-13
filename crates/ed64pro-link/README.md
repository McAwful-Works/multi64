# `multi64-ed64pro-link` (EverDrive-64 PRO)

**Experimental. Never run against a cart.** Host side of Krikzz's edlink **Gen3** USB protocol, which the **EverDrive-64 PRO** (released August 2026) speaks. It is not the X7's `usb64` / `DMA@` protocol; see [`l3-over-everdrive-x7.md` §1.1](../../docs/spec/l3-over-everdrive-x7.md#11-which-everdrives-this-can-apply-to).

Wire contract: [`docs/spec/ed64-pro-usb-host.md`](../../docs/spec/ed64-pro-usb-host.md) (Draft).

## What it does

`Ed64Pro` covers:

- the connection **handshake**, which checks the status key, protocol ID `0x07` and device ID `0x27`;
- **file system** commands: open, close, read, write, seek, file info, directory listing, make directory, delete and exists checks;
- **cart memory** read and write;
- the **FIFO** a running ROM reads, and the raw **USB** data a running ROM sends back.

## How it is tested

Host-only. A scripted transport records every byte written and supplies the cart's replies. Each test checks the exact request bytes against Krikzz's sources. That proves the port matches those sources, **not** that the sources match the hardware.

```sh
cargo test -p multi64-ed64pro-link
```

## Attribution

This crate reimplements the protocol from two MIT-licensed Krikzz repositories, pinned in the spec:

- [krikzz/edlink](https://github.com/krikzz/edlink) — Copyright (c) 2026 krikzz
- [krikzz/ed64-pro-pub](https://github.com/krikzz/ed64-pro-pub) — Copyright (c) 2026 krikzz

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
