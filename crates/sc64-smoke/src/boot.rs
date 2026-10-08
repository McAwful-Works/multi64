//! Booting a ROM with the cart's own bootloader instead of its menu: what `--boot-rom` sends.
//!
//! The ROM goes into SDRAM at internal address 0, which the console sees as cart ROM at PI
//! `0x1000_0000` (vendor `docs/01_memory_map.md`). Config `BOOT_MODE` = [`BOOT_MODE_ROM`] then makes
//! the bootloader start that ROM on the next reset instead of loading the menu from the SD card;
//! with `CIC_SEED` left at [`CIC_SEED_AUTO`] it picks the CIC from the ROM's IPL3. Config ids and
//! values are vendor `docs/04_config_options.md`. No other config value is touched.

use multi64_sc64_link::{cmd, cmd_packet};

/// Config ids this tool reads, in vendor order (`docs/04_config_options.md`). `--config` prints
/// each one; the index is the id.
pub const CONFIG_NAMES: [&str; 15] = [
    "BOOTLOADER_SWITCH",
    "ROM_WRITE_ENABLE",
    "ROM_SHADOW_ENABLE",
    "DD_MODE",
    "ISV_ADDRESS",
    "BOOT_MODE",
    "SAVE_TYPE",
    "CIC_SEED",
    "TV_TYPE",
    "DD_SD_ENABLE",
    "DD_DRIVE_TYPE",
    "DD_DISK_STATE",
    "BUTTON_STATE",
    "BUTTON_MODE",
    "ROM_EXTENDED_ENABLE",
];

pub const CONFIG_BOOT_MODE: u32 = 5;
pub const CONFIG_CIC_SEED: u32 = 7;

/// `BOOT_MODE` 0: the bootloader loads the menu from the SD card. The cart's default.
pub const BOOT_MODE_MENU: u32 = 0;
/// `BOOT_MODE` 1: the bootloader boots whatever is in SDRAM.
pub const BOOT_MODE_ROM: u32 = 1;

/// `CIC_SEED` 0xFFFF: the bootloader works the CIC out from the ROM's IPL3.
pub const CIC_SEED_AUTO: u32 = 0xFFFF;

/// The words a `.z64` (big-endian, as the console reads it) ROM starts with.
const Z64_MAGIC: [u8; 4] = [0x80, 0x37, 0x12, 0x40];
/// The same header byte-swapped in 16-bit units (`.v64`) and in 32-bit units (`.n64`).
const V64_MAGIC: [u8; 4] = [0x37, 0x80, 0x40, 0x12];
const N64_MAGIC: [u8; 4] = [0x40, 0x12, 0x37, 0x80];

/// Header and IPL3 together: anything shorter cannot boot.
const MIN_ROM_LEN: usize = 0x1000;

/// Largest ROM written: 64 MiB of SDRAM less the last 128 KiB, which holds SRAM and FlashRAM saves
/// or, with `ROM_SHADOW_ENABLE`, is mapped to flash (vendor `docs/04_config_options.md`). Larger
/// ROMs need settings this tool does not make.
pub const MAX_ROM_LEN: usize = 0x03FE_0000;

/// Bytes per `MEMORY_WRITE` / `MEMORY_READ`. Each is one command and one response, so a chunk is
/// also what one timeout covers.
pub const CHUNK_LEN: usize = 1024 * 1024;

pub fn config_get_packet(id: u32) -> Vec<u8> {
    cmd_packet(cmd::CONFIG_GET, id, 0, &[])
}

pub fn config_set_packet(id: u32, value: u32) -> Vec<u8> {
    cmd_packet(cmd::CONFIG_SET, id, value, &[])
}

/// One `MEMORY_WRITE` per [`CHUNK_LEN`] of `rom`, each at its own SDRAM address, starting at 0.
pub fn memory_write_packets(rom: &[u8]) -> impl Iterator<Item = (u32, Vec<u8>)> + '_ {
    rom.chunks(CHUNK_LEN).enumerate().map(|(i, chunk)| {
        let addr = (i * CHUNK_LEN) as u32;
        (
            addr,
            cmd_packet(cmd::MEMORY_WRITE, addr, chunk.len() as u32, chunk),
        )
    })
}

/// The `MEMORY_READ`s that read `len` bytes of SDRAM back from address 0, with the address and
/// length each covers.
pub fn memory_read_packets(len: usize) -> impl Iterator<Item = (u32, usize, Vec<u8>)> {
    (0..len).step_by(CHUNK_LEN).map(move |off| {
        let n = CHUNK_LEN.min(len - off);
        (
            off as u32,
            n,
            cmd_packet(cmd::MEMORY_READ, off as u32, n as u32, &[]),
        )
    })
}

/// Why `rom` cannot be booted as it stands, or `None` if it can.
pub fn check_rom(rom: &[u8]) -> Option<String> {
    if rom.len() < MIN_ROM_LEN {
        return Some(format!(
            "{} bytes is too short for a ROM: the header and IPL3 alone are {MIN_ROM_LEN:#x}",
            rom.len()
        ));
    }
    if rom.len() > MAX_ROM_LEN {
        return Some(format!(
            "{} bytes is larger than this tool writes ({MAX_ROM_LEN:#x}); use sc64deployer upload",
            rom.len()
        ));
    }
    match rom[0..4].try_into().unwrap() {
        Z64_MAGIC => None,
        V64_MAGIC => Some("byte-swapped (.v64) ROM: convert it to .z64 first".into()),
        N64_MAGIC => Some("little-endian (.n64) ROM: convert it to .z64 first".into()),
        other => Some(format!(
            "does not start like an N64 ROM: {:02X}{:02X}{:02X}{:02X}, not 80371240",
            other[0], other[1], other[2], other[3]
        )),
    }
}

/// The first offset where `got` differs from `want`, and how many bytes differ in all.
pub fn first_difference(want: &[u8], got: &[u8]) -> Option<(usize, usize)> {
    let mut first = None;
    let mut count = want.len().abs_diff(got.len());
    for (i, (a, b)) in want.iter().zip(got).enumerate() {
        if a != b {
            first.get_or_insert(i);
            count += 1;
        }
    }
    if first.is_none() && count > 0 {
        first = Some(want.len().min(got.len()));
    }
    first.map(|f| (f, count))
}

/// A config value as the cart returns it: 4 bytes, big-endian.
pub fn config_value(data: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(data.try_into().ok()?))
}

pub fn boot_mode_name(value: u32) -> &'static str {
    match value {
        0 => "menu",
        1 => "ROM",
        2 => "64DD IPL",
        3 => "direct ROM",
        4 => "direct 64DD IPL",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(p: &[u8]) -> (u8, u32, u32) {
        assert_eq!(&p[0..3], b"CMD");
        (
            p[3],
            u32::from_be_bytes(p[4..8].try_into().unwrap()),
            u32::from_be_bytes(p[8..12].try_into().unwrap()),
        )
    }

    #[test]
    fn config_packets() {
        let get = config_get_packet(CONFIG_BOOT_MODE);
        assert_eq!(get.len(), 12);
        assert_eq!(args(&get), (b'c', 5, 0));

        let set = config_set_packet(CONFIG_BOOT_MODE, BOOT_MODE_ROM);
        assert_eq!(set.len(), 12);
        assert_eq!(args(&set), (b'C', 5, 1));
    }

    #[test]
    fn config_names_match_vendor_ids() {
        assert_eq!(CONFIG_NAMES[CONFIG_BOOT_MODE as usize], "BOOT_MODE");
        assert_eq!(CONFIG_NAMES[CONFIG_CIC_SEED as usize], "CIC_SEED");
    }

    #[test]
    fn writes_cover_the_rom_in_order() {
        let rom: Vec<u8> = (0..CHUNK_LEN * 2 + 100).map(|i| i as u8).collect();
        let packets: Vec<_> = memory_write_packets(&rom).collect();
        assert_eq!(packets.len(), 3);

        let mut rebuilt = Vec::new();
        for (i, (addr, p)) in packets.iter().enumerate() {
            let (id, a0, a1) = args(p);
            assert_eq!(id, b'M');
            assert_eq!(a0, *addr);
            assert_eq!(a0 as usize, i * CHUNK_LEN);
            assert_eq!(a1 as usize, p.len() - 12, "arg1 is the data length");
            rebuilt.extend_from_slice(&p[12..]);
        }
        assert_eq!(args(&packets[2].1).2, 100);
        assert_eq!(rebuilt, rom);
    }

    #[test]
    fn reads_cover_the_rom_and_carry_no_data() {
        let len = CHUNK_LEN + 4;
        let packets: Vec<_> = memory_read_packets(len).collect();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].0, 0);
        assert_eq!(packets[0].1, CHUNK_LEN);
        assert_eq!(args(&packets[0].2), (b'm', 0, CHUNK_LEN as u32));
        assert_eq!(packets[1].0 as usize, CHUNK_LEN);
        assert_eq!(packets[1].1, 4);
        assert_eq!(args(&packets[1].2), (b'm', CHUNK_LEN as u32, 4));
        assert!(packets.iter().all(|(_, _, p)| p.len() == 12));
    }

    fn rom_with(magic: [u8; 4], len: usize) -> Vec<u8> {
        let mut rom = vec![0; len];
        rom[0..4].copy_from_slice(&magic);
        rom
    }

    #[test]
    fn rom_checks() {
        assert_eq!(check_rom(&rom_with(Z64_MAGIC, MIN_ROM_LEN)), None);
        assert_eq!(check_rom(&rom_with(Z64_MAGIC, MAX_ROM_LEN)), None);
        assert!(check_rom(&rom_with(Z64_MAGIC, MIN_ROM_LEN - 1))
            .unwrap()
            .contains("too short"));
        assert!(check_rom(&rom_with(Z64_MAGIC, MAX_ROM_LEN + 1))
            .unwrap()
            .contains("larger"));
        assert!(check_rom(&rom_with(V64_MAGIC, MIN_ROM_LEN))
            .unwrap()
            .contains(".v64"));
        assert!(check_rom(&rom_with(N64_MAGIC, MIN_ROM_LEN))
            .unwrap()
            .contains(".n64"));
        assert!(check_rom(&rom_with([1, 2, 3, 4], MIN_ROM_LEN))
            .unwrap()
            .contains("01020304"));
    }

    #[test]
    fn differences() {
        assert_eq!(first_difference(b"abcd", b"abcd"), None);
        assert_eq!(first_difference(b"abcd", b"abXY"), Some((2, 2)));
        assert_eq!(first_difference(b"abcd", b"ab"), Some((2, 2)));
        assert_eq!(first_difference(b"Xbcd", b"abc"), Some((0, 2)));
    }

    #[test]
    fn config_values() {
        assert_eq!(config_value(&[0, 0, 0xFF, 0xFF]), Some(CIC_SEED_AUTO));
        assert_eq!(config_value(&[0, 0, 1]), None);
        assert_eq!(boot_mode_name(BOOT_MODE_MENU), "menu");
        assert_eq!(boot_mode_name(BOOT_MODE_ROM), "ROM");
    }
}
