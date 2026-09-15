//! Full-duplex wire parsing: `CMP`/`ERR`/`PKT` interleaved on one serial stream.

use crate::{header_data_len, CmpResponse};

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
/// Returns `None` if more bytes are needed, or if the length exceeds
/// [`MAX_PKT_DATA_LEN`](crate::MAX_PKT_DATA_LEN) (not a real packet).
pub fn try_parse_pkt(buf: &[u8]) -> Option<(usize, PktPacket)> {
    if buf.len() < 8 {
        return None;
    }
    if &buf[0..3] != b"PKT" {
        return None;
    }
    let id = buf[3];
    let len = header_data_len(buf)?;
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
    ///
    /// A tag whose length exceeds [`MAX_CMP_DATA_LEN`](crate::MAX_CMP_DATA_LEN) /
    /// [`MAX_PKT_DATA_LEN`](crate::MAX_PKT_DATA_LEN) was found inside other data (for example an L3
    /// payload after opening mid-stream), so it is skipped instead of waited on.
    pub fn next_event(&mut self) -> Option<WireEvent> {
        loop {
            if self.buf.len() < 8 {
                return None;
            }
            let Some(len) = header_data_len(&self.buf) else {
                self.buf.drain(..1);
                continue;
            };
            let total = 8 + len;
            if self.buf.len() < total {
                return None;
            }
            let is_pkt = &self.buf[0..3] == b"PKT";
            let ok = &self.buf[0..3] == b"CMP";
            let id = self.buf[3];
            let data = self.buf[8..total].to_vec();
            self.buf.drain(..total);
            return Some(if is_pkt {
                WireEvent::Pkt(PktPacket { id, data })
            } else {
                WireEvent::Cmp(CmpResponse {
                    ok,
                    cmd_id: id,
                    data,
                })
            });
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

    fn header(tag: &[u8; 3], id: u8, len: u32) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.push(id);
        v.extend_from_slice(&len.to_be_bytes());
        v
    }

    /// #135: a `PKT` tag found inside other data, with a length no SC64 packet can have, must be
    /// skipped rather than waited on.
    #[test]
    fn oversized_pkt_length_resyncs() {
        let mut blob = header(b"PKT", b'U', 0x2000_0000);
        blob.extend_from_slice(&header(b"PKT", b'U', 2));
        blob.extend_from_slice(&[7, 8]);
        let mut b = WireBuffer::default();
        b.push_bytes(&blob);
        match b.next_event() {
            Some(WireEvent::Pkt(p)) => assert_eq!(p.data, vec![7, 8]),
            other => panic!("expected the valid PKT, got {other:?}"),
        }
    }

    /// A length at the cap is a real (if large) packet and is waited on; one byte over is noise.
    #[test]
    fn pkt_length_cap_boundary() {
        let cap = crate::MAX_PKT_DATA_LEN as u32;
        let mut b = WireBuffer::default();
        b.push_bytes(&header(b"PKT", b'U', cap));
        assert!(b.next_event().is_none());
        assert_eq!(
            b.buf.len(),
            8,
            "a header at the cap is kept while its data arrives"
        );

        let mut b = WireBuffer::default();
        b.push_bytes(&header(b"PKT", b'U', cap + 1));
        assert!(b.next_event().is_none());
        assert!(b.buf.len() < 8, "a header over the cap is discarded");
    }

    /// #135: the same for `CMP` / `ERR`.
    #[test]
    fn oversized_cmp_length_resyncs() {
        for tag in [b"CMP", b"ERR"] {
            let mut blob = header(tag, b'v', 0x9000_0000);
            blob.extend_from_slice(&header(b"CMP", b'v', 4));
            blob.extend_from_slice(b"SCv2");
            let mut b = WireBuffer::default();
            b.push_bytes(&blob);
            match b.next_event() {
                Some(WireEvent::Cmp(c)) => assert_eq!(c.data, b"SCv2"),
                other => panic!("expected the valid CMP, got {other:?}"),
            }
        }
    }
}
