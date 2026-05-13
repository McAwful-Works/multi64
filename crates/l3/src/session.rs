//! Optional L3 session layer — see `docs/spec/l3-bridge-protocol-v1.md` §11.

use crate::decode::DecodeError;
use crate::encode::EncodeError;
use crate::types::{Channel, Frame, FrameFlags, FrameType};

/// `FEATURES` bit 0 in `HANDSHAKE`: request session open/close semantics before `APPLICATION` `DATA`.
pub const FEATURE_SESSION_LAYER: u32 = 1;

/// Tracks whether application `DATA` on [`Channel::Application`] is allowed (after handshake and optional session open).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    session_layer: bool,
    handshake_complete: bool,
    session_id: Option<u32>,
}

impl SessionState {
    pub fn new(session_layer_negotiated: bool) -> Self {
        Self {
            session_layer: session_layer_negotiated,
            handshake_complete: false,
            session_id: None,
        }
    }

    /// Call when `HANDSHAKE_OK` has been processed.
    pub fn on_handshake_ok(&mut self) {
        self.handshake_complete = true;
    }

    /// Call when `SESSION_OPEN_OK` has been processed (`id` is the effective session id).
    pub fn on_session_open_ok(&mut self, id: u32) {
        self.session_id = Some(id);
    }

    /// Clear session id after `SESSION_CLOSE` / `SESSION_CLOSE_ACK` if you intend to block `APPLICATION` until a new open.
    pub fn on_session_closed(&mut self) {
        if self.session_layer {
            self.session_id = None;
        }
    }

    /// `true` when the host or device may send `DATA` on `APPLICATION` per §11.
    pub fn application_data_allowed(&self) -> bool {
        if !self.handshake_complete {
            return false;
        }
        if !self.session_layer {
            return true;
        }
        self.session_id.is_some()
    }

    pub fn session_layer(&self) -> bool {
        self.session_layer
    }

    pub fn session_id(&self) -> Option<u32> {
        self.session_id
    }
}

/// `SESSION_OPEN` — host → device, `CHANNEL` = control.
pub fn session_open_frame(session_id: u32) -> Result<Frame, EncodeError> {
    let mut p = [0u8; 6];
    p[0..4].copy_from_slice(&session_id.to_be_bytes());
    Ok(Frame {
        ty: FrameType::SessionOpen,
        channel: Channel::Control,
        flags: FrameFlags::FINAL,
        request_id: 0,
        payload: p.to_vec(),
    })
}

pub fn parse_session_open(payload: &[u8]) -> Result<u32, DecodeError> {
    if payload.len() != 6 {
        return Err(DecodeError::InvalidPayloadLength);
    }
    if payload[4] != 0 || payload[5] != 0 {
        return Err(DecodeError::InvalidPayloadLength);
    }
    Ok(u32::from_be_bytes(payload[0..4].try_into().unwrap()))
}

/// `SESSION_OPEN_OK` — device → host.
pub fn session_open_ok_frame(session_id: u32) -> Result<Frame, EncodeError> {
    Ok(Frame {
        ty: FrameType::SessionOpenOk,
        channel: Channel::Control,
        flags: FrameFlags::FINAL,
        request_id: 0,
        payload: session_id.to_be_bytes().to_vec(),
    })
}

pub fn parse_session_open_ok(payload: &[u8]) -> Result<u32, DecodeError> {
    if payload.len() != 4 {
        return Err(DecodeError::InvalidPayloadLength);
    }
    Ok(u32::from_be_bytes(payload.try_into().unwrap()))
}

/// `SESSION_CLOSE` — either direction.
pub fn session_close_frame(reason: u16, message: Option<&str>) -> Result<Frame, EncodeError> {
    let msg = message.unwrap_or("");
    let msg_bytes = msg.as_bytes();
    if msg_bytes.len() > u16::MAX as usize {
        return Err(EncodeError::PayloadTooLarge {
            len: 4 + msg_bytes.len(),
            max: u16::MAX as u32,
        });
    }
    let ml = msg_bytes.len() as u16;
    let mut p = Vec::with_capacity(4 + msg_bytes.len());
    p.extend_from_slice(&reason.to_be_bytes());
    p.extend_from_slice(&ml.to_be_bytes());
    p.extend_from_slice(msg_bytes);
    Ok(Frame {
        ty: FrameType::SessionClose,
        channel: Channel::Control,
        flags: FrameFlags::FINAL,
        request_id: 0,
        payload: p,
    })
}

pub fn parse_session_close(payload: &[u8]) -> Result<(u16, String), DecodeError> {
    if payload.len() < 4 {
        return Err(DecodeError::InvalidPayloadLength);
    }
    let reason = u16::from_be_bytes([payload[0], payload[1]]);
    let msg_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
    if payload.len() != 4 + msg_len {
        return Err(DecodeError::InvalidPayloadLength);
    }
    let text = std::str::from_utf8(&payload[4..]).map_err(|_| DecodeError::InvalidPayloadLength)?;
    Ok((reason, text.to_string()))
}

/// `SESSION_CLOSE_ACK` — response to [`FrameType::SessionClose`].
pub fn session_close_ack_frame(request_id: u32) -> Result<Frame, EncodeError> {
    Ok(Frame {
        ty: FrameType::SessionCloseAck,
        channel: Channel::Control,
        flags: FrameFlags::FINAL,
        request_id,
        payload: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn session_open_round_trip() {
        let f = session_open_frame(0xdeadbeef).unwrap().encode().unwrap();
        let (g, _) = Frame::decode(&f).unwrap();
        assert_eq!(g.ty, FrameType::SessionOpen);
        assert_eq!(parse_session_open(&g.payload).unwrap(), 0xdeadbeef);
    }

    #[test]
    fn session_state_without_layer() {
        let mut s = SessionState::new(false);
        assert!(!s.application_data_allowed());
        s.on_handshake_ok();
        assert!(s.application_data_allowed());
    }

    #[test]
    fn session_state_with_layer() {
        let mut s = SessionState::new(true);
        s.on_handshake_ok();
        assert!(!s.application_data_allowed());
        s.on_session_open_ok(42);
        assert!(s.application_data_allowed());
        s.on_session_closed();
        assert!(!s.application_data_allowed());
    }
}
