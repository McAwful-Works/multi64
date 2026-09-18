//! The header checksum IPL3 verifies at boot, over ROM 0x1000..0x101000.
//!
//! Each boot chip (CIC) pairs with its own IPL3, which uses its own seed and, for some, its
//! own mixing. The CIC is identified by hashing IPL3 against the retail ones: an IPL3 this
//! table does not know is refused rather than guessed, because a wrong checksum is a black
//! screen, and a patched IPL3 may not check the sum at all.

use std::fmt;

use sha1::{Digest, Sha1};

pub const CRC_START: usize = 0x1000;
pub const CRC_END: usize = 0x101000;
/// Where the two checksum words live in the header.
pub const CRC_OFFSET: usize = 0x10;
pub const IPL3: std::ops::Range<usize> = 0x40..0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cic {
    Cic6102,
    Cic6103,
    Cic6105,
}

impl fmt::Display for Cic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Cic::Cic6102 => "CIC-6102",
            Cic::Cic6103 => "CIC-6103",
            Cic::Cic6105 => "CIC-6105",
        })
    }
}

impl std::str::FromStr for Cic {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "6102" => Ok(Cic::Cic6102),
            "6103" => Ok(Cic::Cic6103),
            "6105" => Ok(Cic::Cic6105),
            _ => Err(format!("unsupported CIC {s:?} (known: 6102, 6103, 6105)")),
        }
    }
}

/// SHA-1 of the retail IPL3 (ROM 0x40..0x1000) that goes with each CIC.
const KNOWN_IPL3: [(&str, Cic); 3] = [
    ("b2afae246e1dab746bfb28cb346e2911965eefa1", Cic::Cic6102),
    ("3f7347aa0426ee97d96721b09b918d4c9cddb69b", Cic::Cic6103),
    ("4159269055e8a5be2e5c8e3e0f5e0d552f1e85ad", Cic::Cic6105),
];

/// Which CIC this ROM's IPL3 belongs to, or `None` for an IPL3 that is not a known retail one.
pub fn identify(rom: &[u8]) -> Option<Cic> {
    let ipl3 = rom.get(IPL3)?;
    let digest = hex(&Sha1::digest(ipl3));
    #[cfg(test)]
    if digest == test_ipl3::SHA1 {
        return Some(Cic::Cic6102);
    }
    KNOWN_IPL3
        .iter()
        .find(|(hash, _)| *hash == digest)
        .map(|&(_, cic)| cic)
}

/// The two checksum words for `rom`, which must be at least `CRC_END` bytes long.
pub fn compute(rom: &[u8], cic: Cic) -> (u32, u32) {
    let seed: u32 = match cic {
        Cic::Cic6102 => 0xF8CA_4DDC,
        Cic::Cic6103 => 0xA388_6759,
        Cic::Cic6105 => 0xDF26_F436,
    };
    let word = |at: usize| u32::from_be_bytes([rom[at], rom[at + 1], rom[at + 2], rom[at + 3]]);
    let (mut t1, mut t2, mut t3, mut t4, mut t5, mut t6) = (seed, seed, seed, seed, seed, seed);
    for i in (CRC_START..CRC_END).step_by(4) {
        let d = word(i);
        let sum = t6.wrapping_add(d);
        if sum < t6 {
            t4 = t4.wrapping_add(1);
        }
        t6 = sum;
        t3 ^= d;
        let rot = d.rotate_left(d & 0x1F);
        t5 = t5.wrapping_add(rot);
        t2 ^= if t2 > d { rot } else { t6 ^ d };
        t1 = t1.wrapping_add(match cic {
            // 6105's IPL3 mixes in a word of itself, chosen by the low byte of the offset.
            Cic::Cic6105 => word(0x40 + 0x0710 + (i & 0xFF)) ^ d,
            _ => t5 ^ d,
        });
    }
    match cic {
        Cic::Cic6103 => ((t6 ^ t4).wrapping_add(t3), (t5 ^ t2).wrapping_add(t1)),
        _ => (t6 ^ t4 ^ t3, t5 ^ t2 ^ t1),
    }
}

/// The checksum words as stored in the header.
pub fn stored(rom: &[u8]) -> Option<(u32, u32)> {
    Some((
        crate::rom::read_u32(rom, CRC_OFFSET)?,
        crate::rom::read_u32(rom, CRC_OFFSET + 4)?,
    ))
}

/// Recompute and store the checksum.
pub fn fix(rom: &mut [u8], cic: Cic) -> (u32, u32) {
    let (a, b) = compute(rom, cic);
    crate::rom::write_u32(rom, CRC_OFFSET, a);
    crate::rom::write_u32(rom, CRC_OFFSET + 4, b);
    (a, b)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A stand-in boot code for unit tests, which cannot carry a retail one: IPL3 filled with
/// 0xA5, treated as 6102.
#[cfg(test)]
pub(crate) mod test_ipl3 {
    pub const FILL: u8 = 0xA5;
    pub const SHA1: &str = "49be16b4d8512c9fc2b7d0894757d87001090ccd";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Word i = i * 0x9E3779B9 + 0x12345. The expected sums come from a Python reference
    /// that reproduces the stored header CRC of retail 6102 and 6103 ROMs and of two
    /// Archipelago 6105 seeds.
    fn synthetic() -> Vec<u8> {
        (0..CRC_END as u32 / 4)
            .flat_map(|w| {
                w.wrapping_mul(0x9E37_79B9)
                    .wrapping_add(0x12345)
                    .to_be_bytes()
            })
            .collect()
    }

    #[test]
    fn known_answers() {
        let rom = synthetic();
        assert_eq!(compute(&rom, Cic::Cic6102), (0xDF8E_4DDE, 0x88E9_B9BE));
        assert_eq!(compute(&rom, Cic::Cic6103), (0xCD0C_675C, 0xDAFE_B563));
        assert_eq!(compute(&rom, Cic::Cic6105), (0xF522_F438, 0x6BBF_A42F));
    }

    #[test]
    fn fix_writes_what_compute_returns() {
        let mut rom = synthetic();
        let sums = fix(&mut rom, Cic::Cic6102);
        assert_eq!(stored(&rom), Some(sums));
        // The header is outside the summed range, so fixing is idempotent.
        assert_eq!(compute(&rom, Cic::Cic6102), sums);
    }

    #[test]
    fn an_unknown_ipl3_is_not_identified() {
        assert_eq!(identify(&synthetic()), None);
        assert_eq!(identify(&[0; 0x100]), None);
    }
}
