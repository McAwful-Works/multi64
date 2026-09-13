# `multi64-ed64pro-l2` (EverDrive-64 PRO)

**Experimental — never run against a cart.** Carries the L3 octet stream over an EverDrive-64 PRO, as specified in [`l3-over-everdrive-pro.md`](../../docs/spec/l3-over-everdrive-pro.md). Context: [`docs/README.md` — Flash carts](../../docs/README.md#flash-carts-l2-backends).

Unlike the X7 pipe (`multi64-ed64-l2`), there is no reference implementation to follow: neither libdragon nor UNFLoader supports the PRO. The mapping is this repository's own design on top of Krikzz's MIT-licensed [edlink](https://github.com/krikzz/edlink) and [ed64-pro-pub](https://github.com/krikzz/ed64-pro-pub) sources.

## Build

```sh
cargo check -p multi64-ed64pro-l2
cargo test -p multi64-ed64pro-l2
```

## How it works

| Direction | Mechanism |
|-----------|-----------|
| Host → ROM | L3 octets written to the cart FIFO with [`multi64-ed64pro-link`](../ed64pro-link/README.md)'s `fifo_write`, in 1024-byte chunks spaced 34 ms apart |
| ROM → host | Raw bytes the ROM sent with its USB transfer command, read with `usb_read` |

No framing is added: L3 carries its own. `open` runs the edlink handshake, so an open pipe has at least identified an EverDrive-64 PRO — unlike the X7 pipe, where `open` only means the port opened.

The chunk spacing is a guess. The ROM's FIFO holds 2048 bytes and nothing reports how much it has drained; spec §5 lists what to measure.

## Use

`multi64d --cart ed64pro` selects it ([daemon API §5.1](../../docs/spec/daemon-api-v1.md)). The PRO runs at a fixed 921600 baud, so `--baud` does not apply. The ROM side is the test ROM's PRO driver, [`n64/test-rom/ed64pro.c`](../../n64/test-rom/ed64pro.c).

Tests run against `multi64-ed64pro-link`'s in-memory `FakeEd64Pro`, which agrees with the spec by construction and so cannot catch errors in it.
