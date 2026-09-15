//! Parse L3 frames and streaming decoder.

use crate::types::{Channel, Frame, FrameFlags, FrameType};
use crate::{HEADER_LEN, MAGIC, MAX_PAYLOAD_CEILING};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    BadMagic,
    UnknownFrameType(u8),
    InvalidChannel(u8),
    /// `PAYLOAD_LEN` exceeds [`MAX_PAYLOAD_CEILING`], so no handshake can have allowed it.
    PayloadTooLarge(u32),
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
            DecodeError::PayloadTooLarge(len) => write!(
                f,
                "payload length {len} exceeds the protocol ceiling {MAX_PAYLOAD_CEILING}"
            ),
            DecodeError::PayloadLengthMismatch => write!(f, "payload length mismatch"),
            DecodeError::InvalidPayloadLength => {
                write!(f, "invalid payload length for message type")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A 16-byte header whose fields all passed validation.
struct Header {
    ty: FrameType,
    channel: Channel,
    flags: FrameFlags,
    request_id: u32,
    payload_len: usize,
}

impl Header {
    fn into_frame(self, payload: Vec<u8>) -> Frame {
        Frame {
            ty: self.ty,
            channel: self.channel,
            flags: self.flags,
            request_id: self.request_id,
            payload,
        }
    }
}

/// Validate the header at the start of `buf` without needing its payload: `TYPE` and `CHANNEL` must
/// decode, and `PAYLOAD_LEN` must not exceed [`MAX_PAYLOAD_CEILING`], the largest `MAX_PAYLOAD` a
/// handshake can negotiate (spec §6). Neither decoder tracks the handshake, so neither can use the
/// negotiated value.
fn parse_header(buf: &[u8]) -> Result<Header, DecodeError> {
    if buf.len() < HEADER_LEN {
        return Err(DecodeError::TooShort);
    }
    if buf[0..4] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let ty = FrameType::from_u8(buf[4]).ok_or(DecodeError::UnknownFrameType(buf[4]))?;
    let channel = Channel::from_u8(buf[5]).ok_or(DecodeError::InvalidChannel(buf[5]))?;
    let payload_len = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
    if payload_len > MAX_PAYLOAD_CEILING {
        return Err(DecodeError::PayloadTooLarge(payload_len));
    }
    Ok(Header {
        ty,
        channel,
        flags: FrameFlags(u16::from_be_bytes([buf[6], buf[7]])),
        request_id: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        payload_len: payload_len as usize,
    })
}

/// Parse one complete frame from the start of `buf`, returning it and the bytes it used.
///
/// [`DecodeError::TooShort`] means more bytes could complete the frame. A header that no amount of
/// further input can make valid is reported as such, even before its payload has arrived.
pub fn decode_frame(buf: &[u8]) -> Result<(Frame, usize), DecodeError> {
    let header = parse_header(buf)?;
    let total = HEADER_LEN + header.payload_len;
    if buf.len() < total {
        return Err(DecodeError::TooShort);
    }
    Ok((header.into_frame(buf[HEADER_LEN..total].to_vec()), total))
}

/// Incremental decoder: buffers bytes, emits frames, resynchronizes on `M64B`.
///
/// Chunk boundaries are arbitrary: a trailing partial magic is kept for the next push. A header that
/// [`decode_frame`] would reject (bad `TYPE`, `CHANNEL` or `PAYLOAD_LEN`) is rejected as soon as its
/// 16 bytes are buffered, one byte is dropped, and the search for `M64B` resumes.
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
            let Ok(header) = parse_header(&self.buf) else {
                // Noise that happens to contain `M64B`: never wait on the payload it claims. Drop
                // one byte and search again.
                self.buf.drain(..1);
                continue;
            };
            let total = HEADER_LEN + header.payload_len;
            if self.buf.len() < total {
                return;
            }
            let frame = header.into_frame(self.buf[HEADER_LEN..total].to_vec());
            self.buf.drain(..total);
            emit(frame);
        }
    }
}

fn find_magic(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == MAGIC)
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

    /// `decode_frame` enforces the same ceiling as `StreamDecoder`: a header over it is an error from
    /// the header alone, not `TooShort` (which tells a caller to wait for bytes that must not come).
    #[test]
    fn decode_frame_rejects_payload_over_ceiling() {
        let header = corrupt_header(FrameType::Data.to_u8(), 0x00, MAX_PAYLOAD_CEILING + 1);
        let too_large = DecodeError::PayloadTooLarge(MAX_PAYLOAD_CEILING + 1);
        assert_eq!(decode_frame(&header).unwrap_err(), too_large);

        let mut whole = header;
        whole.resize(HEADER_LEN + MAX_PAYLOAD_CEILING as usize + 1, 0x5A);
        assert_eq!(decode_frame(&whole).unwrap_err(), too_large);
        assert_eq!(Frame::decode(&whole).unwrap_err(), too_large);
    }

    fn frame_on_channel(channel: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = corrupt_header(FrameType::Data.to_u8(), channel, payload.len() as u32);
        bytes[7] = FrameFlags::FINAL.bits() as u8;
        bytes.extend_from_slice(payload);
        bytes
    }

    /// Spec §4: `0x80`–`0xFF` are experimental channels, so a frame on one is a valid frame.
    #[test]
    fn experimental_channels_are_accepted() {
        for ch in [0x80u8, 0xC3, 0xFF] {
            let mut bytes = frame_on_channel(ch, b"xp");
            let (frame, n) = decode_frame(&bytes).unwrap();
            assert_eq!(n, bytes.len());
            assert_eq!(frame.channel.to_u8(), ch);
            assert_eq!(
                frame.encode().unwrap(),
                bytes,
                "channel 0x{ch:02x} round-trips"
            );

            bytes.extend_from_slice(&valid_frames(1));
            let mut dec = StreamDecoder::new();
            let mut got = Vec::new();
            dec.push_bytes(&bytes, |fr| got.push(fr));
            assert_eq!(got.len(), 2, "channel 0x{ch:02x}");
            assert_eq!(got[0].channel.to_u8(), ch);
            assert_eq!(got[0].payload, b"xp");
        }
    }

    /// Spec §4: `0x03`–`0x7F` are reserved for future standard channels, which a v1 peer does not
    /// send, so they still mark a header as noise.
    #[test]
    fn reserved_channels_are_still_rejected() {
        for ch in [0x03u8, 0x7F] {
            assert_eq!(
                decode_frame(&frame_on_channel(ch, b"xp")).unwrap_err(),
                DecodeError::InvalidChannel(ch)
            );
            let mut bytes = corrupt_header(FrameType::Data.to_u8(), ch, 1000);
            bytes.extend_from_slice(&valid_frames(2));
            let mut dec = StreamDecoder::new();
            let mut got = Vec::new();
            dec.push_bytes(&bytes, |fr| got.push(fr));
            assert_eq!(got.len(), 2, "channel 0x{ch:02x}");
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
