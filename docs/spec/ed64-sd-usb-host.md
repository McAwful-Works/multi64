# EverDrive 64 X-series: host USB serial and the `RomRead` SD experiment

**Spec-Revision:** 1  

This note describes how **Xfer64** talks to an **EverDrive 64 X-series** cart over **USB serial**, and records an experimental attempt at read-only SD access that mirrors the SummerCart64 **host FAT/exFAT** path. It is **not** Windows mass-storage.

> **The SD experiment rests on a premise vendor sources contradict.** `RomRead` reads cart ROM memory, not the SD card, and this path has never read an SD card on hardware in this repository. See [Why `RomRead` is not SD access](#why-romread-is-not-sd-access).

## Authoritative packet layout (Krikzz / UNFLoader)

The on-wire format matches Krikzz’s reference tooling:

- Repository: [krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub) — see `usb64/usb64/CommandProcessor.cs` (`CommandPacketTransmit`, opcodes).
- Same packing as [N64-UNFLoader](https://github.com/buu342/N64-UNFLoader) (`device_sendcmd_everdrive`).

Outbound: 16-byte **`cmd`** packet: ASCII `cmd`, 1-byte opcode, then three **big-endian `u32`**: address, length as **byte count ÷ 512** (sectors), argument. For **RomRead** (`R`) and **RamRead** (`r`), the cart then streams **`length` bytes** of raw payload on the serial port (no extra 16-byte CMP wrapper before the data in the vendor path we follow).

Implementation: Rust crate [`multi64-ed64-link`](../../crates/multi64-ed64-link/src/lib.rs) (`Ed64Link::command_packet`, `rom_read`, `ram_read`, `test_connection`).

## N64-side context

Krikzz's sample ROM [`ED64-XIO`](https://github.com/krikzz/ed64-x-pub/tree/master/ED64-XIO) is the reference for the cart side. It reaches the SD card **only from the N64**, through cart registers: the `REG_SDIO` block at `0x1F808020` (`REG_BASE` `0x1F800000` + `0x8020`), driven by `bi_sd_cmd_rd` / `bi_sd_cmd_wr` / `bi_sd_dat_rd` / `bi_sd_dat_wr` in `inc/bios.h`. [N64brew — EverDrive-64 X7](https://n64brew.dev/wiki/EverDrive-64_X7) documents the cart's registers from the console side. Neither defines a USB command a PC could use to read SD sectors.

## Experimental: linear SD sectors via `RomRead`

Vendor **`usb64`** has no SD command. Its command set is `c` (fill ROM), `R` (`RomRead`), `W` (`RomWrite`), `s` (start ROM), `t` (test connection), `r` (`RamRead`) and `f` (FPGA). The experiment nevertheless tries to read SD sectors with `RomRead`:

- User supplies **`ed64RomLinearBase`** (32-bit address in cart memory space).
- Host reads logical block **`LBA`** by issuing **RomRead** at  
  **`address = ed64RomLinearBase + LBA × 512`**, with length a multiple of 512 bytes.

Writes are **not** supported on this path.

### Why `RomRead` is not SD access

- **`RomRead` reads cart ROM space.** `usb64` defines it against `ROM_BASE_ADDRESS` = `0x10000000`, the SDRAM that holds the loaded ROM image.
- **The SD card does not appear there on its own.** In the vendor sample, the only route from the card into ROM space is `bi_sd_to_rom`, which *console-side code* runs to copy sectors into ROM memory. A host `RomRead` sees SD data only after something on the N64 has done that.
- **So a "linear base" is not a property of the cart.** No firmware setting maps the card into ROM space for a host to discover. What the scan below finds is ROM memory whose first 512 bytes pass `looks_like_disk_sector0`, which is a weak test: bytes `0x55 0xAA` at offset 510 are enough on their own.
- **It has never worked here.** No commit or test in this repository records this path reading a real SD card; every hardware-verified SD change is SummerCart64.

Treat any EverDrive listing produced this way as untrustworthy. Real host SD access to an X-series cart would need a ROM running on the console that reads sectors through `REG_SDIO` and returns them over USB — which also rules out SD access while a game is running. The **EverDrive-64 PRO** instead offers official file-level SD access over USB through edlink; see [`l3-over-everdrive-x7.md` §1.1](./l3-over-everdrive-x7.md#11-which-everdrives-this-can-apply-to).

### Hints and auto-discovery (Xfer64)

Xfer64 ships a **curated list** of candidate bases ([`ED64_LINEAR_BASE_HINTS`](../../crates/multi64-ed64-link/src/linear_probe.rs)) plus a **coarse grid** over typical N64 cart ROM space. For each address, the host issues **`RomRead` for 512 bytes at `base + 0`** and accepts the base if the buffer looks like **disk sector 0** (protective MBR / MBR boot signature `0x55AA`, plausible FAT BPB, or exFAT boot). Given the premise above, a match means only that those bytes resemble a boot sector: it can be a **false positive**, and there may be **several**.

The UI exposes **Scan for SD base** (Settings → EverDrive advanced), which runs [`probe_ed64_sd_linear_bases`](../../crates/multi64-ed64-link/src/linear_probe.rs) on the resolved COM port. The saved **`ed64RomLinearBase`** (if any) is **tried first** during a scan, then the generic hint/grid list.

On each SD session open, [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) **re-reads sector 0** at the configured base and checks [`looks_like_disk_sector0`](../../crates/multi64-ed64-link/src/linear_probe.rs) so a replug, firmware change, or bad manual value fails fast with a clear error.

Implementation: [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) implements [`SdCardTransport`](../../crates/multi64-sc64-sd/src/link.rs) for [`SectorPartitionDisk`](../../crates/multi64-sc64-sd/src/partition.rs).

## Xfer64 session behavior

When **EverDrive** is selected and **`ed64RomLinearBase`** is set, Xfer64 opens a session analogous to SC64: serial open → work → flush/close. See [`xfer64-cart-serial.md`](./xfer64-cart-serial.md) for sessions, `multi64d` coordination, and the Rust module map.
