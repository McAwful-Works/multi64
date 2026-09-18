//! ROM files as they arrive: three byte orders, one header.

use std::fmt;

/// How the file's bytes are ordered on disk. Everything past [`to_big_endian`] works on `Z64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    /// Big-endian, the console's own order (`.z64`).
    Z64,
    /// 16-bit byte-swapped (`.v64`).
    V64,
    /// 32-bit little-endian (`.n64`).
    N64,
}

impl fmt::Display for ByteOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ByteOrder::Z64 => "z64 (big-endian)",
            ByteOrder::V64 => "v64 (byte-swapped)",
            ByteOrder::N64 => "n64 (little-endian)",
        })
    }
}

/// Recognise the byte order from the first word, which is `80 37 12 40` on every retail ROM.
pub fn byte_order(data: &[u8]) -> Option<ByteOrder> {
    match data.get(..4)? {
        [0x80, 0x37, 0x12, 0x40] => Some(ByteOrder::Z64),
        [0x37, 0x80, 0x40, 0x12] => Some(ByteOrder::V64),
        [0x40, 0x12, 0x37, 0x80] => Some(ByteOrder::N64),
        _ => None,
    }
}

/// Convert in place to big-endian. A trailing partial word is left as it is.
pub fn to_big_endian(data: &mut [u8], order: ByteOrder) {
    match order {
        ByteOrder::Z64 => {}
        ByteOrder::V64 => data.chunks_exact_mut(2).for_each(|c| c.swap(0, 1)),
        ByteOrder::N64 => data.chunks_exact_mut(4).for_each(|c| c.reverse()),
    }
}

/// The header fields used to tell games apart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Header {
    /// Internal name, 0x20..0x34, trimmed.
    pub name: String,
    /// Four-character game code, 0x3B..0x3F (e.g. `N` + two letters + region).
    pub game_code: String,
    /// Mask ROM version, 0x3F.
    pub version: u8,
}

pub fn header(rom: &[u8]) -> Option<Header> {
    if rom.len() < 0x1000 {
        return None;
    }
    let text = |r: &[u8]| {
        r.iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect::<String>()
    };
    Some(Header {
        name: text(&rom[0x20..0x34]).trim_end().to_string(),
        game_code: text(&rom[0x3B..0x3F]),
        version: rom[0x3F],
    })
}

pub fn read_u32(rom: &[u8], at: usize) -> Option<u32> {
    rom.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn write_u32(rom: &mut [u8], at: usize, value: u32) {
    rom[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_orders_normalise_to_the_same_bytes() {
        let z64 = [0x80, 0x37, 0x12, 0x40, 1, 2, 3, 4];
        let v64 = [0x37, 0x80, 0x40, 0x12, 2, 1, 4, 3];
        let n64 = [0x40, 0x12, 0x37, 0x80, 4, 3, 2, 1];
        for input in [z64, v64, n64] {
            let mut data = input;
            let order = byte_order(&data).unwrap();
            to_big_endian(&mut data, order);
            assert_eq!(data, z64, "{order}");
        }
    }

    #[test]
    fn a_non_rom_has_no_byte_order() {
        assert_eq!(byte_order(b"PK\x03\x04"), None);
        assert_eq!(byte_order(&[0x80, 0x37]), None);
    }
}
