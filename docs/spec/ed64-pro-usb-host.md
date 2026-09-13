# EverDrive-64 PRO: host USB link (edlink Gen3)

**Spec-Revision:** 1  
**Status:** **Draft.** This is transcribed from Krikzz's sources and has **never been run against an EverDrive-64 PRO**. Nothing here is normative until someone answers §11 on hardware. **`multi64-ed64pro-link`** implements it and is equally unverified.

This document describes how a PC talks to an **EverDrive-64 PRO** over USB: the handshake, command framing, status and errors, bulk transfers, the SD file system, cart memory, and the channel to a running ROM. It does **not** define L3 over the PRO; that would be a separate mapping on top of §9.

The PRO is **not** an X-series cart. It does not speak `usb64` or the `DMA@` framing in [`l3-over-everdrive-x7.md`](./l3-over-everdrive-x7.md) (see §1.1 there). Unlike the X-series, its microcontroller serves **file-system commands to the PC directly**, so real SD access over USB is possible without any code running on the N64.

---

## 1. Sources

Every byte below comes from these revisions:

| Source | Revision | Role |
|--------|----------|------|
| [krikzz/edlink](https://github.com/krikzz/edlink) | `eb2f51142fca98d2408442660f76a11e729c006d` | The PC utility: `edlink/Device/Link.cs`, `Device/DeviceIO.cs`, `Device/DeviceIO_V2.cs`, `DEV_ED64/DeviceIO.cs`, `Device/DeviceCmd.cs` |
| [krikzz/ed64-pro-pub](https://github.com/krikzz/ed64-pro-pub) | `5d7e96905331a841f97f8c51e0b0cba878e72fe3` | Console-side library `edio/everdrive.c` / `.h` and sample `edio/appmain.c` |

Both are MIT-licensed. edlink's README says console applications "use the same command interface as USB communication, but through a FIFO port mapped into the CPU address space". That is the basis for using the console library as a reference for commands the PC utility does not send.

Where the sources disagree, §10 lists the difference and which one this document follows.

---

## 2. Transport

- The cart appears as a **serial port** (edlink uses .NET `SerialPort`, i.e. the OS VCP driver).
- Open at **921600 baud**.
- **Probing:** 200 ms read and write timeouts.
- **After identification:** 2000 ms timeouts.

edlink scans every serial port and keeps the first that passes §4. A host SHOULD accept an explicit port as well.

---

## 3. Framing

### 3.1 Commands

A command is 4 bytes, optionally followed by a subcommand byte:

| Offset | Value |
|--------|-------|
| 0 | `0x2B` (`'+'`) |
| 1 | `0xD4` (`'+' ^ 0xFF`) |
| 2 | command |
| 3 | command `^ 0xFF` |
| 4 | subcommand (only for commands that take one) |

Arguments follow the frame directly.

### 3.2 Integers

16- and 32-bit values are **big-endian** both ways. (edlink sets `SwapEndians = true` for the PRO, which writes the most significant byte first; the N64 side writes them natively, and the N64 is big-endian.)

### 3.3 Strings

A **`u16` byte length**, then the bytes. There is no terminator.

### 3.4 Writes

edlink writes in blocks of at most 4096 bytes and **never makes a single 512-byte write**: a 512-byte block is sent as two 256-byte writes, commented "512 does not work well by some reasons". The stream carries the same bytes either way. A host SHOULD reproduce the split anyway, in case the cause is USB packet boundaries (§11).

---

## 4. Connection handshake

1. Open the port (§2) and send **66 zero bytes**.
2. Discard any input.
3. Send `CMD_STATUS2` (`0x40`) then `CMD_STATUS` (`0x10`).
4. Read 2 bytes:
   - `0xA5` in either byte: legacy Mega / N8 firmware. **Reject** — another console.
   - First byte is not `0x5A`: **reject**.
   - Otherwise read 2 more. If the protocol ID (byte 1) is `0x05` (Mega) or `0x06` (N8), it is a Gen2 cart for another console, which also answers `CMD_STATUS2`; read its remaining 2 bytes and **reject**.
   - Otherwise it is Gen3. A Gen3 cart does not answer `CMD_STATUS2`, so the 4 bytes are the reply to `CMD_STATUS`.
5. Send `CMD_STATUS` and read the 4-byte status reply (§5). Require protocol **`0x07`** and device **`0x27`**.

---

## 5. Status and errors

`CMD_STATUS` (`0x10`) returns 4 bytes:

| Byte | Meaning |
|------|---------|
| 0 | status key `0x5A` |
| 1 | protocol ID, `0x07` |
| 2 | device ID, `0x27` for the EverDrive-64 PRO |
| 3 | result of the previous command; `0` on success |

Commands that end in a status check send `CMD_STATUS` after their payload. If byte 3 is non-zero, send **`CMD_NRESP`** (`0x13`) followed by that byte. The cart returns one byte of detail. edlink reports the pair as `operation error SS.DD`.

---

## 6. Bulk transfers (`EPO`)

`CMD_EPO` (`0x81`) with subcommand `0x10` moves data between two endpoints. It is followed by 17 bytes:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | source address |
| 4 | 4 | destination address |
| 8 | 4 | length in bytes |
| 12 | 1 | source endpoint |
| 13 | 1 | destination endpoint |
| 14 | 2 | reserved, `0` |
| 16 | 1 | `0` — starts the transfer |

This matches `EpoXfer` in `everdrive.c` plus its `ed_run_xfer` byte.

**Endpoints used by the host:**

| Value | Endpoint |
|-------|----------|
| `0x10` | **LINK**: the USB stream, unacknowledged |
| `0x11` | **LINK_ACK**: the USB stream in acknowledged 1024-byte blocks |
| `0x12` | **FS**: the currently open file |
| `0x13` | **FCI**: cart memory |

**Directions:**

- **Reading into the host** (source FS or FCI → LINK): read exactly `length` raw bytes, then check status (§5).
- **Writing a file** (LINK_ACK → FS): before each block of up to **1024** bytes the cart sends one byte, which MUST be `0`; then send the block. After the last block, check status.
- **Writing cart memory** (LINK → FCI): send the bytes unacknowledged. **edlink reads no status afterwards**; the call is commented out.

---

## 7. File system (`CMD_FS`, `0x80`)

**Paths** are relative to the SD root with no leading slash, e.g. `ed64/sysdata/config.ini`. `""` is the root. edlink strips its own `sd:` prefix before sending.

| Subcommand | Name | Request after frame | Reply |
|------------|------|---------------------|-------|
| `0x10` | INIT | — | status |
| `0x13` | DIR_LD | option byte, path | status |
| `0x14` | DIR_SIZE | — | `u16` entry count |
| `0x16` | DIR_GET | `u16` start, `u16` count, `u16` max name length | per entry: result byte, then (if `0`) a file record |
| `0x17` | FOPN | mode byte, path | status |
| `0x18` | FCLOSE | — | status |
| `0x19` | FPTR | `u32` position | status |
| `0x1A` | FINFO | path | result byte, then (if `0`) a file record |
| `0x1C` | DIR_MK | path | status |
| `0x1D` | DEL | path | status |
| `0x1F` | AVB | — | `u32` low, then `u32` high: bytes left in the open file |
| `0x22` | DTEST | path | status: `0` if the directory exists |
| `0x23` | FTEST | path | status: `0` if the file exists |

**A file record** is `u32` size, `u16` MS-DOS date, `u16` MS-DOS time, a FAT attribute byte (`0x10` = directory), then the name as a string (§3.3).

**Reading and writing the open file** uses §6 with FS as the source or destination. **Open modes** are the FatFs `FA_*` flags (`READ 0x01`, `WRITE 0x02`, `CREATE_NEW 0x04`, `CREATE_ALWAYS 0x08`, `OPEN_ALWAYS 0x10`, `OPEN_APPEND 0x30`), plus `0x80` to create missing parent directories. **Directory options:** `SORTED 0x01`, `HIDE_SYS 0x02`, among others.

**Listing:** send DIR_LD, then DIR_SIZE, then request entries. The console sample requests **one entry per DIR_GET**, because a bulk request must not leave more than **2048** unread bytes in the cart's FIFO. A host SHOULD do the same until the USB-side limit is known (§11).

---

## 8. Cart memory and the FIFO

- **Memory:** read and write FCI addresses with §6.
- **FIFO:** the host feeds a running ROM by writing to FCI **`0x10010000`** (`ADDR_FCI_SYS + 0x10000`). The ROM reads it through its `FIFODATA` register.

---

## 9. Data from a running ROM

A ROM sends to the host with `ed_usb_wr`, an `EPO` from LINK to the **USB** endpoint (§10). The host reads it **raw, with no framing**: edlink's `usbrd` reads whatever bytes are waiting. That data shares the serial stream with command replies, so a host MUST NOT issue commands while it expects ROM output.

Any L3 mapping over the PRO would have to supply its own framing on top of this.

---

## 10. Where the sources disagree

| Topic | edlink (PC) | ed64-pro-pub (console) | This document |
|-------|-------------|------------------------|---------------|
| Endpoint numbers above `0x13` | `FLA 0x14`, `EFU 0x15`, `USB 0x16` | `FLA 0x14`, `RAM 0x15`, `NOP 0x16`, `DBG 0x17`, `USB 0x18` | Uses only `0x10`–`0x13`, where they agree |
| Reading a file | FS → **LINK**, unacknowledged | FS → **LINK_ACK**, console acknowledges each block | Follows **edlink**, the PC-side reference |
| Directory, info, mkdir, delete, exists, seek | Not implemented for Gen3 (legacy V1 only) | Implemented | Follows **ed64-pro-pub**: the only Gen3 source |
| String length | `str.Length` with ASCII encoding (non-ASCII becomes `?`) | Byte length of a NUL-terminated string | **UTF-8 byte length**; identical for ASCII |

---

## 11. Open questions — resolve on hardware before dropping Draft

1. **The handshake itself.** Does a PRO answer §4 as described: silent on `CMD_STATUS2`, 4 bytes on `CMD_STATUS`?
2. **VCP behaviour at 921600 baud** under sustained transfers, on Windows and Linux. What USB descriptors does the PRO present? These are needed for descriptor-based auto-detect.
3. **The 512-byte write split** (§3.4): is it still needed, and does it matter for anything but MCU app loads?
4. **Directory paging:** is the 2048-byte FIFO limit relevant on the USB side, or can DIR_GET return many entries per request?
5. **Existence checks:** does a non-zero DTEST/FTEST status only mean "absent", or can it also be an error that needs `CMD_NRESP`?
6. **Non-ASCII paths:** how does the firmware encode names in file records, and what does it accept in requests?
7. **SD access while a game runs:** do file-system commands work, and is it safe, while the console is running a ROM that may itself use the card?

---

## 12. Implementation in this repository

| Component | Role |
|-----------|------|
| [`crates/ed64pro-link`](../../crates/ed64pro-link/README.md) | **`multi64-ed64pro-link`**: `Ed64Pro` implements §2–§9. Tests use a scripted transport that checks exact request bytes; never run against a cart. |

---

## 13. Revision

**Spec-Revision** counts edits to this document only. It is independent of L3 **Protocol-Major/Minor** ([`l3-bridge-protocol-v1.md`](./l3-bridge-protocol-v1.md) §12).

| Value | Meaning |
|-------|---------|
| **1** | Initial draft, transcribed from the pinned sources in §1. |
