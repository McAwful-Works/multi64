//! Parse L3 frames and streaming decoder.

use crate::types::{Channel, Frame, FrameFlags, FrameType};
use crate::{HEADER_LEN, MAGIC};

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
                self.buf.clear();
                return;
            };
            if magic_pos > 0 {
                self.buf.drain(..magic_pos);
            }
            if self.buf.len() < HEADER_LEN {
                return;
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
}
