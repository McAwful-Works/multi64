//! L3 application protocol: framed messages with magic **`M64B`**, version negotiation, and optional session layer.
//!
//! This crate implements encode/decode, a [`StreamDecoder`] for incremental bytes off the wire, and session helpers.
//! The normative specification is **`docs/spec/l3-bridge-protocol-v1.md`** in the multi64 repository.
//!
//! # Unsafe code
//!
//! This crate contains **no** `unsafe` (`#![forbid(unsafe_code)]`).

#![forbid(unsafe_code)]

mod decode;
mod encode;
mod session;
mod types;

pub use decode::{DecodeError, StreamDecoder};
pub use encode::EncodeError;
pub use session::{
    parse_session_close, parse_session_open, parse_session_open_ok, session_close_ack_frame,
    session_close_frame, session_open_frame, session_open_ok_frame, SessionState,
    FEATURE_SESSION_LAYER,
};
pub use types::{Channel, ErrorCode, Frame, FrameFlags, FrameType};

/// Magic bytes at the start of every L3 frame (`M64B`).
pub const MAGIC: [u8; 4] = [0x4D, 0x36, 0x34, 0x42];

pub const HEADER_LEN: usize = 16;

/// L3 version 1 (`PROTO_MAJOR` / `PROTO_MINOR` in `HANDSHAKE`).
pub const PROTO_MAJOR: u8 = 1;
pub const PROTO_MINOR: u8 = 0;

/// Default maximum payload size for new sessions (before handshake negotiation).
pub const DEFAULT_MAX_PAYLOAD: u32 = 8192;

impl Frame {
    /// Total serialized size in bytes.
    pub fn wire_len(&self) -> usize {
        HEADER_LEN + self.payload.len()
    }

    /// Serialize this frame to bytes.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        encode::encode_frame(self)
    }

    /// Parse one complete frame from the start of `buf`.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), DecodeError> {
        decode::decode_frame(buf)
    }
}

/// Build a `HANDSHAKE` frame (host → device) with a 12-byte binary payload per spec.
pub fn handshake_client_frame(max_payload: u32, features: u32) -> Result<Frame, EncodeError> {
    let mut p = [0u8; 12];
    p[0] = PROTO_MAJOR;
    p[1] = PROTO_MINOR;
    // p[2..4] reserved
    p[4..8].copy_from_slice(&max_payload.to_be_bytes());
    p[8..12].copy_from_slice(&features.to_be_bytes());
    Ok(Frame {
        ty: FrameType::Handshake,
        channel: Channel::Control,
        flags: FrameFlags::empty(),
        request_id: 0,
        payload: p.to_vec(),
    })
}

/// Parse `HANDSHAKE` client payload (12 bytes).
pub fn parse_handshake_client(payload: &[u8]) -> Result<(u8, u8, u32, u32), DecodeError> {
    if payload.len() != 12 {
        return Err(DecodeError::InvalidPayloadLength);
    }
    let major = payload[0];
    let minor = payload[1];
    let max_payload = u32::from_be_bytes(payload[4..8].try_into().unwrap());
    let features = u32::from_be_bytes(payload[8..12].try_into().unwrap());
    Ok((major, minor, max_payload, features))
}

/// Build `HANDSHAKE_OK` (device → host) with 8-byte payload.
pub fn handshake_ok_frame(
    proto_major: u8,
    proto_minor: u8,
    effective_max_payload: u32,
) -> Result<Frame, EncodeError> {
    let mut p = [0u8; 8];
    p[0] = proto_major;
    p[1] = proto_minor;
    p[4..8].copy_from_slice(&effective_max_payload.to_be_bytes());
    Ok(Frame {
        ty: FrameType::HandshakeOk,
        channel: Channel::Control,
        flags: FrameFlags::empty(),
        request_id: 0,
        payload: p.to_vec(),
    })
}

pub fn parse_handshake_ok(payload: &[u8]) -> Result<(u8, u8, u32), DecodeError> {
    if payload.len() != 8 {
        return Err(DecodeError::InvalidPayloadLength);
    }
    let major = payload[0];
    let minor = payload[1];
    let max_payload = u32::from_be_bytes(payload[4..8].try_into().unwrap());
    Ok((major, minor, max_payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_data() {
        let f = Frame {
            ty: FrameType::Data,
            channel: Channel::Application,
            flags: FrameFlags::FINAL,
            request_id: 0x11223344,
            payload: vec![1, 2, 3, 4, 5],
        };
        let b = f.encode().unwrap();
        let (g, n) = Frame::decode(&b).unwrap();
        assert_eq!(n, b.len());
        assert_eq!(g.ty, f.ty);
        assert_eq!(g.channel, f.channel);
        assert_eq!(g.flags, f.flags);
        assert_eq!(g.request_id, f.request_id);
        assert_eq!(g.payload, f.payload);
    }

    #[test]
    fn handshake_round_trip() {
        let f = handshake_client_frame(4096, 0).unwrap();
        let b = f.encode().unwrap();
        let (g, _) = Frame::decode(&b).unwrap();
        assert_eq!(g.ty, FrameType::Handshake);
        let (maj, min, max, feat) = parse_handshake_client(&g.payload).unwrap();
        assert_eq!(maj, PROTO_MAJOR);
        assert_eq!(min, PROTO_MINOR);
        assert_eq!(max, 4096);
        assert_eq!(feat, 0);
    }

    #[test]
    fn stream_decoder_resync() {
        let f = handshake_client_frame(100, 0).unwrap().encode().unwrap();
        let mut junk = vec![0u8, 1, 2, 3];
        junk.extend_from_slice(&f);
        let mut dec = StreamDecoder::new();
        let mut out = Vec::new();
        dec.push_bytes(&junk, |frame| out.push(frame));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ty, FrameType::Handshake);
    }
}
