//! Serialize L3 frames.

use crate::types::{Channel, Frame, FrameType};
use crate::{HEADER_LEN, MAGIC};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodeError {
    PayloadTooLarge { len: usize, max: u32 },
    InvalidChannel(u8),
    InvalidFrameType(u8),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::PayloadTooLarge { len, max } => {
                write!(f, "payload length {len} exceeds negotiated max {max}")
            }
            EncodeError::InvalidChannel(b) => write!(f, "invalid channel 0x{b:02x}"),
            EncodeError::InvalidFrameType(b) => write!(f, "invalid frame type 0x{b:02x}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encode with optional cap (use `None` to skip the check during tests).
pub fn encode_frame_with_cap(
    frame: &Frame,
    max_payload: Option<u32>,
) -> Result<Vec<u8>, EncodeError> {
    let plen = frame.payload.len();
    if let Some(max) = max_payload {
        if plen as u64 > u64::from(max) {
            return Err(EncodeError::PayloadTooLarge { len: plen, max });
        }
    }

    let ty = frame.ty.to_u8();
    if FrameType::from_u8(ty).is_none() {
        return Err(EncodeError::InvalidFrameType(ty));
    }

    let ch = frame.channel.to_u8();
    if Channel::from_u8(ch).is_none() {
        return Err(EncodeError::InvalidChannel(ch));
    }

    let mut out = Vec::with_capacity(HEADER_LEN + plen);
    out.extend_from_slice(&MAGIC);
    out.push(ty);
    out.push(ch);
    out.extend_from_slice(&frame.flags.bits().to_be_bytes());
    out.extend_from_slice(&frame.request_id.to_be_bytes());
    out.extend_from_slice(&(plen as u32).to_be_bytes());
    out.extend_from_slice(&frame.payload);
    Ok(out)
}

pub(crate) fn encode_frame(frame: &Frame) -> Result<Vec<u8>, EncodeError> {
    encode_frame_with_cap(frame, None)
}
