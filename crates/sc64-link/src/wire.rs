//! Full-duplex wire parsing: `CMP`/`ERR`/`PKT` interleaved on one serial stream.

use crate::CmpResponse;

/// Async packet from SC64 (`PKT` + id + length + data).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PktPacket {
    pub id: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireEvent {
    Cmp(CmpResponse),
    Pkt(PktPacket),
}

/// Try to parse one `PKT` at the start of `buf`.
pub fn try_parse_pkt(buf: &[u8]) -> Option<(usize, PktPacket)> {
    if buf.len() < 8 {
        return None;
    }
    if &buf[0..3] != b"PKT" {
        return None;
    }
    let id = buf[3];
    let len = u32::from_be_bytes(buf[4..8].try_into().unwrap()) as usize;
    let total = 8 + len;
    if buf.len() < total {
        return None;
    }
    Some((
        total,
        PktPacket {
            id,
            data: buf[8..total].to_vec(),
        },
    ))
}

/// Buffer that yields `CMP`/`ERR`/`PKT` in order from a raw serial byte stream.
#[derive(Default)]
pub struct WireBuffer {
    buf: Vec<u8>,
}

impl WireBuffer {
    pub fn push_bytes(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Pop the next complete vendor packet, or `None` if more bytes are needed.
    pub fn next_event(&mut self) -> Option<WireEvent> {
        loop {
            if self.buf.len() < 8 {
                return None;
            }
            let tag = &self.buf[0..3];
            if tag == b"CMP" || tag == b"ERR" {
                let len = u32::from_be_bytes(self.buf[4..8].try_into().unwrap()) as usize;
                let total = 8 + len;
                if self.buf.len() < total {
                    return None;
                }
                let ok = tag == b"CMP";
                let cmd_id = self.buf[3];
                let data = self.buf[8..total].to_vec();
                self.buf.drain(..total);
                return Some(WireEvent::Cmp(CmpResponse { ok, cmd_id, data }));
            }
            if tag == b"PKT" {
                let len = u32::from_be_bytes(self.buf[4..8].try_into().unwrap()) as usize;
                let total = 8 + len;
                if self.buf.len() < total {
                    return None;
                }
                let id = self.buf[3];
                let pkt_data = self.buf[8..total].to_vec();
                self.buf.drain(..total);
                return Some(WireEvent::Pkt(PktPacket { id, data: pkt_data }));
            }
            self.buf.drain(..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pkt_round_trip() {
        let mut v = Vec::new();
        v.extend_from_slice(b"PKT");
        v.push(b'U');
        v.extend_from_slice(&(4u32).to_be_bytes());
        v.extend_from_slice(&[1, 2, 3, 4]);
        let (n, p) = try_parse_pkt(&v).unwrap();
        assert_eq!(n, v.len());
        assert_eq!(p.id, b'U');
        assert_eq!(p.data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn wire_buffer_order() {
        let mut b = WireBuffer::default();
        let mut blob = Vec::new();
        blob.extend_from_slice(b"CMP");
        blob.push(b'v');
        blob.extend_from_slice(&(2u32).to_be_bytes());
        blob.extend_from_slice(&[9, 9]);
        blob.extend_from_slice(b"PKT");
        blob.push(b'U');
        blob.extend_from_slice(&(1u32).to_be_bytes());
        blob.push(0);
        b.push_bytes(&blob);
        match b.next_event().unwrap() {
            WireEvent::Cmp(c) => {
                assert!(c.ok);
                assert_eq!(c.data, vec![9, 9]);
            }
            _ => panic!("expected CMP"),
        }
        match b.next_event().unwrap() {
            WireEvent::Pkt(p) => {
                assert_eq!(p.id, b'U');
                assert_eq!(p.data, vec![0]);
            }
            _ => panic!("expected PKT"),
        }
    }
}
