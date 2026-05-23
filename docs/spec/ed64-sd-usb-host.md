# EverDrive 64 X-series: host USB serial and experimental SD-over-`RomRead`

**Spec-Revision:** 1  

This note describes how **Xfer64** talks to an **EverDrive 64 X-series** cart over **USB serial** for read-only access that mirrors the SummerCart64 **host FAT/exFAT** path. It is **not** Windows mass-storage.

## Authoritative packet layout (Krikzz / UNFLoader)

The on-wire format matches Krikzz’s reference tooling:

- Repository: [krikzz/ed64-x-pub](https://github.com/krikzz/ed64-x-pub) — see `usb64/usb64/CommandProcessor.cs` (`CommandPacketTransmit`, opcodes).
- Same packing as [N64-UNFLoader](https://github.com/buu342/N64-UNFLoader) (`device_sendcmd_everdrive`).

Outbound: 16-byte **`cmd`** packet: ASCII `cmd`, 1-byte opcode, then three **big-endian `u32`**: address, length as **byte count ÷ 512** (sectors), argument. For **RomRead** (`R`) and **RamRead** (`r`), the cart then streams **`length` bytes** of raw payload on the serial port (no extra 16-byte CMP wrapper before the data in the vendor path we follow).

Implementation: Rust crate [`multi64-ed64-link`](../../crates/multi64-ed64-link/src/lib.rs) (`Ed64Link::command_packet`, `rom_read`, `ram_read`, `test_connection`).

## N64-side context (community)

[N64brew — EverDrive-64 X7](https://n64brew.dev/wiki/EverDrive-64_X7) documents cart registers and behavior on the console side. It does **not** define a public “send LBA over USB” command for PC tools.

## Experimental: linear SD sectors via `RomRead`

Vendor **`usb64`** does not document raw SD LBA access over USB. For **read-only** host access aligned with our **512-byte sector** + partition table stack, we use an **experimental** mapping:

- User supplies **`ed64RomLinearBase`** (32-bit address in cart memory space).
- Host reads logical block **`LBA`** by issuing **RomRead** at  
  **`address = ed64RomLinearBase + LBA × 512`**, with length a multiple of 512 bytes.

Whether this window actually reflects the SD card’s linear media depends on **firmware/OS** and how the cart exposes storage; the value is **per-setup** and may need experimentation or external notes. Writes are **not** supported on this path in the current build.

### Hints and auto-discovery (Xfer64)

Xfer64 ships a **curated list** of candidate bases ([`ED64_LINEAR_BASE_HINTS`](../../crates/multi64-ed64-link/src/linear_probe.rs)) plus a **coarse grid** over typical N64 cart ROM space. For each address, the host issues **`RomRead` for 512 bytes at `base + 0`** and accepts the base if the buffer looks like **disk sector 0** (protective MBR / MBR boot signature `0x55AA`, plausible FAT BPB, or exFAT boot). This can produce **false positives** or **multiple** candidates; the user picks the one that lists the SD correctly.

The UI exposes **Scan for SD base** (Settings → EverDrive advanced), which runs [`probe_ed64_sd_linear_bases`](../../crates/multi64-ed64-link/src/linear_probe.rs) on the resolved COM port. The saved **`ed64RomLinearBase`** (if any) is **tried first** during a scan, then the generic hint/grid list.

On each SD session open, [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) **re-reads sector 0** at the configured base and checks [`looks_like_disk_sector0`](../../crates/multi64-ed64-link/src/linear_probe.rs) so a replug, firmware change, or bad manual value fails fast with a clear error.

Implementation: [`Ed64RomLinear`](../../crates/multi64-sc64-sd/src/ed64_linear.rs) implements [`SdCardTransport`](../../crates/multi64-sc64-sd/src/link.rs) for [`SectorPartitionDisk`](../../crates/multi64-sc64-sd/src/partition.rs).

## Xfer64 session behavior

When **EverDrive** is selected and **`ed64RomLinearBase`** is set, Xfer64 opens a session analogous to SC64: serial open → work → flush/close. See [`xfer64-cart-serial.md`](./xfer64-cart-serial.md) for sessions, `multi64d` coordination, and the Rust module map.
