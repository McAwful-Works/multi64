//! Parse L3 frames and streaming decoder.

use crate::types::{Channel, Frame, FrameFlags, FrameType};
use crate::{HEADER_LEN, MAGIC, MAX_PAYLOAD_CEILING};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    BadMagic,
    UnknownFrameType(u8),
    InvalidChannel(u8),
    PayloadLengthMismatch,
    InvalidPayloadLength,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "buffer too short for frame"),
            DecodeError::BadMagic => write!(f, "bad magic"),
            DecodeError::UnknownFrameType(b) => write!(f, "unknown frame type 0x{b:02x}"),
            DecodeError::InvalidChannel(b) => write!(f, "invalid channel 0x{b:02x}"),
            DecodeError::PayloadLengthMismatch => write!(f, "payload length mismatch"),
            DecodeError::InvalidPayloadLength => {
                write!(f, "invalid payload length for message type")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

pub fn decode_frame(buf: &[u8]) -> Result<(Frame, usize), DecodeError> {
    if buf.len() < HEADER_LEN {
        return Err(DecodeError::TooShort);
    }
    if buf[0..4] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let ty = buf[4];
    let ty_e = FrameType::from_u8(ty).ok_or(DecodeError::UnknownFrameType(ty))?;
    let ch = buf[5];
    let channel = Channel::from_u8(ch).ok_or(DecodeError::InvalidChannel(ch))?;
    let flags = FrameFlags(u16::from_be_bytes([buf[6], buf[7]]));
    let request_id = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let payload_len = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
    let total = HEADER_LEN + payload_len;
    if buf.len() < total {
        return Err(DecodeError::TooShort);
    }
    let payload = buf[HEADER_LEN..total].to_vec();
    Ok((
        Frame {
            ty: ty_e,
            channel,
            flags,
            request_id,
            payload,
        },
        total,
    ))
}

/// Incremental decoder: buffers bytes, emits frames, resynchronizes on `M64B`.
///
/// Chunk boundaries are arbitrary: a trailing partial magic is kept for the next push. A header whose
/// `TYPE`, `CHANNEL` or `PAYLOAD_LEN` is invalid is rejected as soon as its 16 bytes are buffered, one
/// byte is dropped, and the search for `M64B` resumes.
pub struct StreamDecoder {
    buf: Vec<u8>,
}

impl Default for StreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append bytes and invoke `emit` for each complete frame.
    pub fn push_bytes(&mut self, chunk: &[u8], mut emit: impl FnMut(Frame)) {
        self.buf.extend_from_slice(chunk);
        self.drain_frames(&mut emit);
    }

    fn drain_frames(&mut self, emit: &mut impl FnMut(Frame)) {
        loop {
            let Some(magic_pos) = find_magic(&self.buf) else {
                // Keep a trailing partial magic (`M`, `M6`, `M64`) that the next push may complete.
                let keep = (MAGIC.len() - 1).min(self.buf.len());
                let discard = self.buf.len() - keep;
                self.buf.drain(..discard);
                return;
            };
            if magic_pos > 0 {
                self.buf.drain(..magic_pos);
            }
            if self.buf.len() < HEADER_LEN {
                return;
            }
            if !header_is_plausible(&self.buf[..HEADER_LEN]) {
                // Noise that happens to contain `M64B`: never wait on the payload it claims.
                self.buf.drain(..1);
                continue;
            }
            let payload_len =
                u32::from_be_bytes([self.buf[12], self.buf[13], self.buf[14], self.buf[15]])
                    as usize;
            let total = HEADER_LEN + payload_len;
            if self.buf.len() < total {
                return;
            }
            match decode_frame(&self.buf[..total]) {
                Ok((frame, n)) => {
                    debug_assert_eq!(n, total);
                    self.buf.drain(..total);
                    emit(frame);
                }
                Err(_) => {
                    // Lose sync: drop one byte and search again for M64B.
                    self.buf.drain(..1);
                }
            }
        }
    }
}

fn find_magic(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == MAGIC)
}

/// Validate a 16-byte header before waiting for its payload: `TYPE` and `CHANNEL` must decode and
/// `PAYLOAD_LEN` must not exceed [`MAX_PAYLOAD_CEILING`], the largest `MAX_PAYLOAD` a handshake can
/// negotiate (spec §6). The decoder does not track the handshake, so it cannot use the negotiated value.
fn header_is_plausible(header: &[u8]) -> bool {
    FrameType::from_u8(header[4]).is_some()
        && Channel::from_u8(header[5]).is_some()
        && u32::from_be_bytes([header[12], header[13], header[14], header[15]])
            <= MAX_PAYLOAD_CEILING
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::{handshake_client_frame, PROTO_MAJOR, PROTO_MINOR};

    #[test]
    fn decoder_two_frames() {
        let a = handshake_client_frame(100, 0).unwrap().encode().unwrap();
        let b = handshake_client_frame(200, 0).unwrap().encode().unwrap();
        let mut both = a.clone();
        both.extend_from_slice(&b);
        let mut dec = StreamDecoder::new();
        let mut got = Vec::new();
        dec.push_bytes(&both, |f| got.push(f));
        assert_eq!(got.len(), 2);
        let (maj, min, max, _) = crate::parse_handshake_client(&got[0].payload).unwrap();
        assert_eq!(maj, PROTO_MAJOR);
        assert_eq!(min, PROTO_MINOR);
        assert_eq!(max, 100);
        let (_, _, max2, _) = crate::parse_handshake_client(&got[1].payload).unwrap();
        assert_eq!(max2, 200);
    }

    /// #132: a push that ends inside `M64B` must keep the partial magic for the next push.
    #[test]
    fn decoder_magic_split_across_pushes() {
        let f = handshake_client_frame(100, 0).unwrap().encode().unwrap();
        for split in 1..MAGIC.len() {
            let mut dec = StreamDecoder::new();
            let mut got = Vec::new();
            dec.push_bytes(&f[..split], |fr| got.push(fr));
            dec.push_bytes(&f[split..], |fr| got.push(fr));
            assert_eq!(got.len(), 1, "magic split after {split} byte(s)");
            assert_eq!(got[0].ty, FrameType::Handshake);
        }
    }

    /// #132: the split still resynchronises when noise precedes the partial magic.
    #[test]
    fn decoder_magic_split_after_noise() {
        let f = handshake_client_frame(100, 0).unwrap().encode().unwrap();
        let mut first = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        first.extend_from_slice(&f[..3]);
        let mut dec = StreamDecoder::new();
        let mut got = Vec::new();
        dec.push_bytes(&first, |fr| got.push(fr));
        dec.push_bytes(&f[3..], |fr| got.push(fr));
        assert_eq!(got.len(), 1);
    }

    fn corrupt_header(ty: u8, channel: u8, len: u32) -> Vec<u8> {
        let mut h = MAGIC.to_vec();
        h.push(ty);
        h.push(channel);
        h.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        h.extend_from_slice(&len.to_be_bytes());
        h
    }

    fn valid_frames(n: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            let f = Frame {
                ty: FrameType::Data,
                channel: Channel::Application,
                flags: FrameFlags::FINAL,
                request_id: i,
                payload: i.to_be_bytes().to_vec(),
            };
            out.extend_from_slice(&f.encode().unwrap());
        }
        out
    }

    /// #133: a header claiming `0xFFFFFFFF` payload bytes must not swallow the frames after it.
    #[test]
    fn decoder_huge_length_header_does_not_stall() {
        let mut bytes = corrupt_header(FrameType::Data.to_u8(), 0x00, 0xFFFF_FFFF);
        bytes.extend_from_slice(&valid_frames(101));
        let mut dec = StreamDecoder::new();
        let mut got = Vec::new();
        dec.push_bytes(&bytes, |fr| got.push(fr));
        assert_eq!(got.len(), 101);
        for (i, fr) in got.iter().enumerate() {
            assert_eq!(fr.request_id, i as u32);
        }
    }

    /// #133: the same holds when the stream arrives in small pieces.
    #[test]
    fn decoder_huge_length_header_does_not_stall_chunked() {
        let mut bytes = corrupt_header(FrameType::Data.to_u8(), 0x00, 0xFFFF_FFFF);
        bytes.extend_from_slice(&valid_frames(101));
        let mut dec = StreamDecoder::new();
        let mut got = Vec::new();
        for chunk in bytes.chunks(7) {
            dec.push_bytes(chunk, |fr| got.push(fr));
        }
        assert_eq!(got.len(), 101);
    }

    /// #133: a bad TYPE or CHANNEL is rejected from the header alone, without waiting for the
    /// payload its length claims.
    #[test]
    fn decoder_bad_type_or_channel_resyncs_before_payload() {
        for header in [
            corrupt_header(0x7E, 0x00, 1000),
            corrupt_header(FrameType::Data.to_u8(), 0x55, 1000),
        ] {
            let mut bytes = header;
            bytes.extend_from_slice(&valid_frames(3));
            let mut dec = StreamDecoder::new();
            let mut got = Vec::new();
            dec.push_bytes(&bytes, |fr| got.push(fr));
            assert_eq!(got.len(), 3);
        }
    }

    /// A payload at exactly the protocol ceiling is still a valid frame.
    #[test]
    fn decoder_accepts_payload_at_ceiling() {
        let f = Frame {
            ty: FrameType::Data,
            channel: Channel::Application,
            flags: FrameFlags::FINAL,
            request_id: 7,
            payload: vec![0x5A; crate::MAX_PAYLOAD_CEILING as usize],
        };
        let mut dec = StreamDecoder::new();
        let mut got = Vec::new();
        dec.push_bytes(&f.encode().unwrap(), |fr| got.push(fr));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload.len(), crate::MAX_PAYLOAD_CEILING as usize);
    }
}
