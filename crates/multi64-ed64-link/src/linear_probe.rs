//! Heuristic **linear base** discovery: first 512-byte block at each candidate address should look like **disk sector 0**
//! (MBR / protective MBR, FAT/exFAT boot sector). Uses **edlink EPO / FCI** when the cart answers as ED64, else **`usb64` `RomRead`**.

use crate::{Ed64Link, EdlinkLink, PROTOCOL_ID_ED64, SECTOR_BYTES};
use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

/// Curated **first** addresses to try (vendor ROM window, common cart space). Not exhaustive.
pub const ED64_LINEAR_BASE_HINTS: &[u32] = &[
    0x1000_0000, // Krikzz `usb64` `ROM_BASE_ADDRESS`
    0x1008_0000,
    0x1010_0000,
    0x1020_0000,
    0x1040_0000,
    0x1080_0000,
    0x1100_0000,
    0x0F00_0000,
    0x0F80_0000,
    0x0FC0_0000,
    0x1800_0000,
    0x1C00_0000,
];

/// Extra addresses from a coarse **grid** (N64 cart ROM mapping window; step keeps serial probe bounded).
fn scan_grid_addresses() -> Vec<u32> {
    let mut v = Vec::new();
    // Around typical ROM base: 0x10000000 .. 0x12000000 step 1 MiB
    let mut a = 0x1000_0000u32;
    while a <= 0x1200_0000 {
        v.push(a);
        a = a.saturating_add(0x10_0000);
    }
    // Broader cart range: 0x0F000000 .. 0x12000000 step 2 MiB
    let mut b = 0x0F00_0000u32;
    while b <= 0x1200_0000 {
        v.push(b);
        b = b.saturating_add(0x20_0000);
    }
    v
}

/// Build deduplicated, sorted list of bases to probe (hints first, then grid).
pub fn ed64_linear_base_probe_list() -> Vec<u32> {
    let mut set = BTreeSet::new();
    for &h in ED64_LINEAR_BASE_HINTS {
        set.insert(h);
    }
    for a in scan_grid_addresses() {
        set.insert(a);
    }
    set.into_iter().collect()
}

/// Same as [`ed64_linear_base_probe_list`], but **`preferred`** is tried first (if set) so a
/// persisted address is re-validated before the generic hint/grid scan.
pub fn ed64_linear_base_probe_list_with_preferred(preferred: Option<u32>) -> Vec<u32> {
    let mut out = Vec::new();
    if let Some(p) = preferred {
        out.push(p);
    }
    for b in ed64_linear_base_probe_list() {
        if !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

/// True if `buf` looks like **sector 0** of a partitioned disk or super-floppy (best-effort).
pub fn looks_like_disk_sector0(buf: &[u8; SECTOR_BYTES]) -> bool {
    if buf.iter().all(|&b| b == 0) {
        return false;
    }
    if is_exfat_boot_sector(buf) {
        return true;
    }
    if mbr_boot_signature(buf) {
        return true;
    }
    if is_fat_bpb_plausible(buf) {
        return true;
    }
    false
}

fn mbr_boot_signature(buf: &[u8; SECTOR_BYTES]) -> bool {
    buf[510] == 0x55 && buf[511] == 0xAA
}

fn is_exfat_boot_sector(buf: &[u8; SECTOR_BYTES]) -> bool {
    // Jump + OEM "EXFAT" at 3..8
    buf[3..8].eq_ignore_ascii_case(b"EXFAT")
}

/// FAT12/16/32 BPB: jump, then bytes_per_sector often 512 at 0x0B, and "FAT" strings in BPB.
fn is_fat_bpb_plausible(buf: &[u8; SECTOR_BYTES]) -> bool {
    let jmp = buf[0];
    if jmp != 0xEB && jmp != 0xE9 && !(jmp == 0xE8 && buf[2] == 0x90) {
        return false;
    }
    let bps = u16::from_le_bytes([buf[11], buf[12]]);
    if bps != 512 && bps != 1024 && bps != 2048 && bps != 4096 {
        return false;
    }
    // "FAT12", "FAT16", or "FAT32" near OEM name / filesystem label region
    let s = &buf[0x52..0x5A.min(buf.len())];
    s.windows(3).any(|w| w == b"FAT")
}

fn finalize_linear_probe_results(mut found: Vec<u32>, preferred_first: Option<u32>) -> Vec<u32> {
    found.sort_unstable();
    found.dedup();
    if let Some(p) = preferred_first {
        if let Some(pos) = found.iter().position(|&x| x == p) {
            found.remove(pos);
            found.insert(0, p);
        }
    }
    found
}

/// Open serial, then probe each candidate base: **edlink `fci_read`** when Gen3 ED64 is detected, else **`usb64` `RomRead`**.
/// Returns **all** bases whose first sector passes [`looks_like_disk_sector0`]. May be empty; may have false positives.
///
/// `preferred_first`: saved address from settings — checked **first** so rediscovery prefers the
/// last known-good base when the cart is plugged in again.
pub fn probe_ed64_sd_linear_bases(
    port: &str,
    baud: u32,
    preferred_first: Option<u32>,
) -> io::Result<(Vec<u32>, usize)> {
    probe_ed64_sd_linear_bases_with_cancel(port, baud, preferred_first, || true)
}

/// Same as [`probe_ed64_sd_linear_bases`], but stops between addresses when `should_continue` returns
/// `false`. Returns [`io::ErrorKind::Interrupted`] with message `"Cancelled"`.
pub fn probe_ed64_sd_linear_bases_with_cancel(
    port: &str,
    baud: u32,
    preferred_first: Option<u32>,
    mut should_continue: impl FnMut() -> bool,
) -> io::Result<(Vec<u32>, usize)> {
    let list = ed64_linear_base_probe_list_with_preferred(preferred_first);
    let checked = list.len();
    let mut found = Vec::new();

    if let Ok(mut el) = EdlinkLink::try_open(port) {
        if el.protocol_id() == PROTOCOL_ID_ED64 {
            el.set_timeout(Duration::from_millis(750))?;
            for &base in &list {
                if !should_continue() {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
                }
                let _ = el.clear_buffers();
                let mut sector = [0u8; SECTOR_BYTES];
                if el.fci_read(base, &mut sector).is_ok() && looks_like_disk_sector0(&sector) {
                    found.push(base);
                }
            }
            return Ok((finalize_linear_probe_results(found, preferred_first), checked));
        }
    }

    let mut link = Ed64Link::open(port, baud)?;
    link.set_timeout(Duration::from_millis(750))?;
    let _ = link.clear_buffers();
    link.test_connection()?;

    for base in list {
        if !should_continue() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
        }
        let _ = link.clear_buffers();
        match link.rom_read(base, SECTOR_BYTES) {
            Ok(data) => {
                if data.len() != SECTOR_BYTES {
                    continue;
                }
                let mut sector = [0u8; SECTOR_BYTES];
                sector.copy_from_slice(&data);
                if looks_like_disk_sector0(&sector) {
                    found.push(base);
                }
            }
            Err(_) => continue,
        }
    }

    Ok((finalize_linear_probe_results(found, preferred_first), checked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbr_signature_detected() {
        let mut b = [0u8; 512];
        b[510] = 0x55;
        b[511] = 0xAA;
        assert!(looks_like_disk_sector0(&b));
    }

    #[test]
    fn exfat_signature_detected() {
        let mut b = [0u8; 512];
        b[3..8].copy_from_slice(b"EXFAT");
        assert!(looks_like_disk_sector0(&b));
    }

    #[test]
    fn all_zero_rejected() {
        let b = [0u8; 512];
        assert!(!looks_like_disk_sector0(&b));
    }

    #[test]
    fn preferred_is_first_in_probe_list() {
        let v = ed64_linear_base_probe_list_with_preferred(Some(0xdead_beef));
        assert_eq!(v.first().copied(), Some(0xdead_beef));
        assert!(!v.iter().skip(1).any(|&x| x == 0xdead_beef));
    }
}
