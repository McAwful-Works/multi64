//! Whole-ROM transforms applied before a profile's checks and writes.
//!
//! `yaz0_dmadata`: games built on Nintendo's DMA manager (Ocarina of Time and its kin) keep a
//! file table ("dmadata") of `vrom_start, vrom_end, rom_start, rom_end` entries, and store
//! most files Yaz0-compressed. A seed's code can only be patched where it is plain bytes, so
//! the whole image is decompressed: every file is placed at its vrom and its entry rewritten
//! as uncompressed (`rom_start = vrom_start`, `rom_end = 0`). The game reads either form.

use crate::rom::read_u32;

fn yaz0(src: &[u8], at: usize) -> Result<Vec<u8>, String> {
    let err = || format!("Yaz0 data at 0x{at:X} is truncated or corrupt");
    if src.get(at..at + 4) != Some(b"Yaz0".as_slice()) {
        return Err(format!("no Yaz0 header at 0x{at:X}"));
    }
    let size = read_u32(src, at + 4).ok_or_else(err)? as usize;
    let mut out = Vec::with_capacity(size);
    let mut s = at + 16;
    let byte = |i: usize| src.get(i).copied().ok_or_else(err);
    while out.len() < size {
        let code = byte(s)?;
        s += 1;
        for bit in 0..8 {
            if out.len() >= size {
                break;
            }
            if code & (0x80 >> bit) != 0 {
                out.push(byte(s)?);
                s += 1;
            } else {
                let (b1, b2) = (byte(s)? as usize, byte(s + 1)? as usize);
                s += 2;
                let dist = ((b1 & 0xF) << 8 | b2) + 1;
                let n = match b1 >> 4 {
                    0 => {
                        s += 1;
                        byte(s - 1)? as usize + 0x12
                    }
                    n => n + 2,
                };
                if dist > out.len() {
                    return Err(err());
                }
                for _ in 0..n {
                    let b = out[out.len() - dist];
                    out.push(b);
                }
            }
        }
    }
    Ok(out)
}

/// An entry of the file table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaEntry {
    /// Where the entry is, in the ROM.
    pub at: usize,
    pub vrom_start: u32,
    pub vrom_end: u32,
    pub rom_start: u32,
    pub rom_end: u32,
}

/// The file table at `table`, up to its terminating all-zero entry.
pub fn dma_entries(rom: &[u8], table: usize) -> Result<Vec<DmaEntry>, String> {
    let mut out = Vec::new();
    let mut at = table;
    loop {
        let w = |o| read_u32(rom, at + o).ok_or("the file table runs past the end");
        let e = DmaEntry {
            at,
            vrom_start: w(0)?,
            vrom_end: w(4)?,
            rom_start: w(8)?,
            rom_end: w(12)?,
        };
        if e.vrom_start == 0 && e.vrom_end == 0 && at > table {
            return Ok(out);
        }
        out.push(e);
        at += 16;
        if out.len() > 0x2000 {
            return Err("the file table has no end".into());
        }
    }
}

/// Decompress every file in the table at `table`. See the module doc.
pub fn yaz0_dmadata(rom: &[u8], table: u32) -> Result<Vec<u8>, String> {
    let entries = dma_entries(rom, table as usize)?;
    // The table describes itself; an entry at `table` proves this is a file table at all.
    if !entries
        .iter()
        .any(|e| e.vrom_start == table && e.rom_start == table)
    {
        return Err(format!("no file table at 0x{table:X}"));
    }
    let used = |e: &&DmaEntry| e.rom_start != 0xFFFF_FFFF;
    let end = entries
        .iter()
        .filter(used)
        .map(|e| e.vrom_end as usize)
        .max()
        .unwrap_or(0);
    if end > 0x0400_0000 {
        return Err(format!(
            "the decompressed image would be 0x{end:X} bytes, over 64 MiB"
        ));
    }
    let mut out = vec![0u8; end.max(0x0010_1000)];
    out[..0x1000].copy_from_slice(&rom[..0x1000]);
    for e in entries.iter().filter(used) {
        let len = e
            .vrom_end
            .checked_sub(e.vrom_start)
            .ok_or("a file ends before it starts")? as usize;
        let data = if e.rom_end == 0 {
            rom.get(e.rom_start as usize..e.rom_start as usize + len)
                .ok_or_else(|| format!("file at vrom 0x{:X} is past the end", e.vrom_start))?
                .to_vec()
        } else {
            // A randomizer can leave a stub in place of a file the game never loads: fewer
            // bytes than the entry spans. The rest stays zero. More would overwrite the next
            // file, and is refused.
            let d = yaz0(rom, e.rom_start as usize)?;
            if d.len() > len {
                return Err(format!(
                    "file at vrom 0x{:X} decompresses to {} bytes, more than its {len}",
                    e.vrom_start,
                    d.len()
                ));
            }
            d
        };
        out[e.vrom_start as usize..e.vrom_start as usize + data.len()].copy_from_slice(&data);
    }
    for e in entries.iter().filter(used) {
        let words = [e.vrom_start, e.vrom_end, e.vrom_start, 0];
        for (i, w) in words.iter().enumerate() {
            crate::rom::write_u32(&mut out, e.at + 4 * i, *w);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "abcabcabc!" as literals then one back-reference, and a long-run reference.
    fn sample() -> (Vec<u8>, Vec<u8>) {
        let plain = b"abcabcabcabcabcabcabcabcabc!".to_vec();
        let mut y = b"Yaz0".to_vec();
        y.extend_from_slice(&(plain.len() as u32).to_be_bytes());
        y.extend_from_slice(&[0; 8]);
        // 3 literals, one 3-byte-distance copy of 24 bytes (long form: n = 24 - 0x12), 1 literal
        y.push(0b1110_1000);
        y.extend_from_slice(b"abc");
        y.extend_from_slice(&[0x00, 0x02, 24 - 0x12]);
        y.push(b'!');
        (y, plain)
    }

    #[test]
    fn yaz0_decodes_literals_and_long_copies() {
        let (y, plain) = sample();
        assert_eq!(yaz0(&y, 0).unwrap(), plain);
    }

    #[test]
    fn yaz0_refuses_a_bad_back_reference() {
        let mut y = b"Yaz0\0\0\0\x04".to_vec();
        y.extend_from_slice(&[0; 8]);
        y.extend_from_slice(&[0x00, 0x10, 0x05]);
        assert!(yaz0(&y, 0).is_err());
    }

    #[test]
    fn a_table_is_decompressed_and_rewritten() {
        let (y, plain) = sample();
        let table = 0x1000usize;
        let mut rom = vec![0u8; 0x0020_0000];
        let entry = |rom: &mut Vec<u8>, i: usize, w: [u32; 4]| {
            for (k, v) in w.iter().enumerate() {
                crate::rom::write_u32(rom, table + i * 16 + 4 * k, *v);
            }
        };
        entry(&mut rom, 0, [0x1000, 0x1040, 0x1000, 0]);
        entry(
            &mut rom,
            1,
            [
                0x0010_2000,
                0x0010_2000 + plain.len() as u32,
                0x0010_8000,
                0x0010_8000 + y.len() as u32,
            ],
        );
        entry(&mut rom, 2, [0x0010_3000, 0x0010_3004, 0x0010_9000, 0]);
        rom[0x0010_8000..0x0010_8000 + y.len()].copy_from_slice(&y);
        rom[0x0010_9000..0x0010_9004].copy_from_slice(b"RAW!");
        let out = yaz0_dmadata(&rom, table as u32).unwrap();
        assert_eq!(
            &out[0x0010_2000..0x0010_2000 + plain.len()],
            plain.as_slice()
        );
        assert_eq!(&out[0x0010_3000..0x0010_3004], b"RAW!");
        let e = dma_entries(&out, table).unwrap();
        assert_eq!((e[1].rom_start, e[1].rom_end), (0x0010_2000, 0));
        assert!(yaz0_dmadata(&rom, 0x2000).is_err(), "no table there");
    }

    /// A randomizer's stub for a file the game never loads: a tiny Yaz0 blob under an
    /// entry spanning far more. It is kept, and the rest of the span stays zero.
    #[test]
    fn a_short_placeholder_file_is_zero_filled() {
        let (y, plain) = sample();
        let table = 0x1000usize;
        let mut rom = vec![0u8; 0x0020_0000];
        for (k, v) in [0x1000u32, 0x1030, 0x1000, 0].iter().enumerate() {
            crate::rom::write_u32(&mut rom, table + 4 * k, *v);
        }
        let entry = [
            0x0010_2000u32,
            0x0010_2400,
            0x0010_8000,
            0x0010_8000 + y.len() as u32,
        ];
        for (k, v) in entry.iter().enumerate() {
            crate::rom::write_u32(&mut rom, table + 16 + 4 * k, *v);
        }
        rom[0x0010_8000..0x0010_8000 + y.len()].copy_from_slice(&y);
        let out = yaz0_dmadata(&rom, table as u32).unwrap();
        assert_eq!(
            &out[0x0010_2000..0x0010_2000 + plain.len()],
            plain.as_slice()
        );
        assert!(out[0x0010_2000 + plain.len()..0x0010_2400]
            .iter()
            .all(|&b| b == 0));
    }
}
