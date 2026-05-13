//! L3 frame types and enums.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub ty: FrameType,
    pub channel: Channel,
    pub flags: FrameFlags,
    pub request_id: u32,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Handshake = 0x01,
    HandshakeOk = 0x02,
    HandshakeReject = 0x03,
    Data = 0x10,
    Ack = 0x11,
    Error = 0x12,
    Heartbeat = 0x20,
    HeartbeatAck = 0x21,
    SessionOpen = 0x30,
    SessionOpenOk = 0x31,
    SessionClose = 0x32,
    SessionCloseAck = 0x33,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => Self::Handshake,
            0x02 => Self::HandshakeOk,
            0x03 => Self::HandshakeReject,
            0x10 => Self::Data,
            0x11 => Self::Ack,
            0x12 => Self::Error,
            0x20 => Self::Heartbeat,
            0x21 => Self::HeartbeatAck,
            0x30 => Self::SessionOpen,
            0x31 => Self::SessionOpenOk,
            0x32 => Self::SessionClose,
            0x33 => Self::SessionCloseAck,
            _ => return None,
        })
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Channel {
    Application = 0x00,
    Log = 0x01,
    Control = 0x02,
}

impl Channel {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x00 => Self::Application,
            0x01 => Self::Log,
            0x02 => Self::Control,
            _ => return None,
        })
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameFlags(pub u16);

impl FrameFlags {
    pub const FINAL: FrameFlags = FrameFlags(0x0001);

    pub const fn empty() -> Self {
        FrameFlags(0)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub fn contains(self, other: FrameFlags) -> bool {
        (self.0 & other.0) == other.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    Unspecified = 0x0000,
    VersionUnsupported = 0x0001,
    FrameTooLarge = 0x0002,
    MalformedFrame = 0x0003,
    UnknownFrameType = 0x0004,
    BufferFull = 0x0005,
    TransportTimeout = 0x0006,
    SessionReset = 0x0007,
    FeatureUnsupported = 0x0008,
    SessionLayerRequired = 0x0009,
    SessionNotOpen = 0x000A,
}

impl ErrorCode {
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0x0000 => Self::Unspecified,
            0x0001 => Self::VersionUnsupported,
            0x0002 => Self::FrameTooLarge,
            0x0003 => Self::MalformedFrame,
            0x0004 => Self::UnknownFrameType,
            0x0005 => Self::BufferFull,
            0x0006 => Self::TransportTimeout,
            0x0007 => Self::SessionReset,
            0x0008 => Self::FeatureUnsupported,
            0x0009 => Self::SessionLayerRequired,
            0x000A => Self::SessionNotOpen,
            _ => return None,
        })
    }

    pub fn to_u16(self) -> u16 {
        self as u16
    }
}
