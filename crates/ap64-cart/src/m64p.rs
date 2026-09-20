//! M64P codec — RDRAM peek/poke and cartridge ROM reads, carried in L3 APPLICATION payloads.
//!
//! Normative spec: `docs/spec/memory-l3-application-v0.md` in the multi64 repo.
//! This module is the host counterpart of multi64's `n64/test-rom/mem_proto.c`
//! and knows nothing about transports; it turns requests into bytes and bytes
//! back into responses.

use std::fmt;

pub const MAGIC: [u8; 4] = *b"M64P";

pub const MSG_HELLO: u8 = 0x01;
pub const MSG_PEEKV: u8 = 0x02;
pub const MSG_POKEV: u8 = 0x03;
pub const MSG_PEEKROM: u8 = 0x04;
pub const MSG_WATCH: u8 = 0x05;

pub const MSG_HELLO_ACK: u8 = 0x81;
pub const MSG_PEEKV_RESP: u8 = 0x82;
pub const MSG_POKE_ACK: u8 = 0x83;
pub const MSG_PEEKROM_RESP: u8 = 0x84;
pub const MSG_WATCH_ACK: u8 = 0x85;
pub const MSG_ERR: u8 = 0xE0;

/// Spec §4. Enforced here as well as on the cart, so an over-large request fails
/// locally with a useful message instead of costing a round trip to be told `ERR`.
pub const MAX_REGIONS: usize = 32;
pub const MAX_REGION_BYTES: usize = 4096;
pub const MAX_TOTAL_BYTES: usize = 7936;

/// `HELLO_ACK` flags bit 0: the agent accepts `POKEV`.
pub const FLAG_WRITABLE: u8 = 0x01;
/// `HELLO_ACK` flags bit 1: the agent answers `PEEKROM`, and `rom_bytes` follows the flags.
pub const FLAG_CART_ROM: u8 = 0x02;
/// `HELLO_ACK` flags bit 2: the agent watches slots, and `watch_slots` follows `rom_bytes`.
pub const FLAG_WATCH: u8 = 0x04;

/// `ERR` code for a `PEEKROM` that could not get the PI bus: nothing was read, retry.
pub const E_BUSY: u8 = 0x07;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub addr: u32,
    pub len: u16,
}

/// One slot to watch: where it is, and which changes are worth queueing (spec 4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub addr: u32,
    pub len: u8,
    /// The byte in the slot the filter tests; ignored when `values` is empty.
    pub at: u8,
    /// Values that byte may take, or empty to keep every change.
    pub values: Vec<u8>,
}

/// A value a watched slot held, as the agent saw it at a frame boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Index into the slot list the host sent.
    pub slot: u8,
    pub bytes: Vec<u8>,
}

/// Spec 4.3 limits, which an agent reports through `watch_slots`.
pub const WATCH_MAX_LEN: usize = 8;
pub const WATCH_MAX_VALUES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    HelloAck {
        proto: u8,
        agent_ver: u16,
        rdram_bytes: u32,
        flags: u8,
        /// The cart ROM window `PEEKROM` may address, when `flags` has `FLAG_CART_ROM`.
        rom_bytes: Option<u32>,
        /// Slots the agent can watch at once, when `flags` has `FLAG_WATCH`.
        watch_slots: Option<u8>,
    },
    PeekV {
        rid: u16,
        regions: Vec<Vec<u8>>,
        /// What the agent's watched slots held between this host's reads (spec 4.3).
        /// Always empty unless this host set a watch.
        events: Vec<Event>,
    },
    PeekRom {
        rid: u16,
        regions: Vec<Vec<u8>>,
    },
    PokeAck {
        rid: u16,
        applied: u8,
    },
    WatchAck {
        rid: u16,
        watching: u8,
    },
    Err {
        rid: u16,
        code: u8,
    },
}

impl Response {
    /// The `rid` this response answers, if it carries one. `HELLO_ACK` does not.
    pub fn rid(&self) -> Option<u16> {
        match self {
            Response::HelloAck { .. } => None,
            Response::PeekV { rid, .. }
            | Response::PeekRom { rid, .. }
            | Response::PokeAck { rid, .. }
            | Response::WatchAck { rid, .. }
            | Response::Err { rid, .. } => Some(*rid),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Payload was not an M64P message at all — expected on a shared channel.
    NotM64p,
    Truncated,
    UnknownMsg(u8),
    /// A *request* type came back from the cart -- our own bytes, still sitting
    /// in the staging buffer because the agent never wrote a reply over them.
    ///
    /// Not corruption, and not a protocol violation: it is what a silent agent
    /// looks like. Seen on every console reset, and during scene loads where the
    /// frame hook stops running with a request outstanding.
    EchoedRequest(u8),
    TooManyRegions(usize),
    RegionTooLarge(usize),
    TotalTooLarge(usize),
    /// The cart answered `ERR`; carries the spec §5 code.
    Refused {
        rid: u16,
        code: u8,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotM64p => write!(f, "payload is not M64P"),
            Error::Truncated => write!(f, "M64P payload truncated"),
            Error::UnknownMsg(m) => write!(f, "unknown M64P msg 0x{m:02X}"),
            Error::EchoedRequest(m) => {
                write!(f, "echoed request 0x{m:02X}: agent has not replied")
            }
            Error::TooManyRegions(n) => write!(f, "{n} regions exceeds limit of {MAX_REGIONS}"),
            Error::RegionTooLarge(n) => {
                write!(f, "region of {n} bytes exceeds limit of {MAX_REGION_BYTES}")
            }
            Error::TotalTooLarge(n) => {
                write!(f, "{n} bytes total exceeds limit of {MAX_TOTAL_BYTES}")
            }
            Error::Refused { rid, code } => {
                write!(f, "cart refused request {rid}: {}", err_name(*code))
            }
        }
    }
}

impl std::error::Error for Error {}

pub fn err_name(code: u8) -> &'static str {
    match code {
        0x01 => "E_MALFORMED",
        0x02 => "E_TOO_MANY",
        0x03 => "E_TOO_LARGE",
        0x04 => "E_RANGE",
        0x05 => "E_READONLY",
        0x06 => "E_UNSUPPORTED",
        0x07 => "E_BUSY",
        _ => "unknown",
    }
}

fn header(msg: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(16);
    v.extend_from_slice(&MAGIC);
    v.push(msg);
    v
}

fn check_limits(lens: impl Iterator<Item = usize> + Clone) -> Result<(), Error> {
    let n = lens.clone().count();
    if n > MAX_REGIONS {
        return Err(Error::TooManyRegions(n));
    }
    let mut total = 0usize;
    for len in lens {
        if len > MAX_REGION_BYTES {
            return Err(Error::RegionTooLarge(len));
        }
        total += len;
    }
    if total > MAX_TOTAL_BYTES {
        return Err(Error::TotalTooLarge(total));
    }
    Ok(())
}

pub fn encode_hello() -> Vec<u8> {
    header(MSG_HELLO)
}

pub fn encode_peekv(rid: u16, regions: &[Region]) -> Result<Vec<u8>, Error> {
    encode_peek(MSG_PEEKV, rid, regions)
}

/// `PEEKROM`: `PEEKV`'s body, over cartridge ROM offsets (spec 4.2).
pub fn encode_peekrom(rid: u16, regions: &[Region]) -> Result<Vec<u8>, Error> {
    encode_peek(MSG_PEEKROM, rid, regions)
}

fn encode_peek(msg: u8, rid: u16, regions: &[Region]) -> Result<Vec<u8>, Error> {
    check_limits(regions.iter().map(|r| r.len as usize))?;
    let mut v = header(msg);
    v.extend_from_slice(&rid.to_be_bytes());
    v.push(regions.len() as u8);
    for r in regions {
        v.extend_from_slice(&r.addr.to_be_bytes());
        v.extend_from_slice(&r.len.to_be_bytes());
    }
    Ok(v)
}

pub fn encode_pokev(rid: u16, writes: &[(u32, &[u8])]) -> Result<Vec<u8>, Error> {
    check_limits(writes.iter().map(|(_, d)| d.len()))?;
    let mut v = header(MSG_POKEV);
    v.extend_from_slice(&rid.to_be_bytes());
    v.push(writes.len() as u8);
    for (addr, data) in writes {
        v.extend_from_slice(&addr.to_be_bytes());
        v.extend_from_slice(&(data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
    }
    Ok(v)
}

/// `WATCH`: the slots to follow, replacing whatever was being watched. Empty stops.
pub fn encode_watch(rid: u16, slots: &[Slot]) -> Result<Vec<u8>, Error> {
    if slots.len() > u8::MAX as usize {
        return Err(Error::TooManyRegions(slots.len()));
    }
    for s in slots {
        if s.len as usize > WATCH_MAX_LEN || s.len == 0 {
            return Err(Error::RegionTooLarge(s.len as usize));
        }
        if s.values.len() > WATCH_MAX_VALUES {
            return Err(Error::RegionTooLarge(s.values.len()));
        }
    }
    let mut v = Vec::with_capacity(8 + slots.len() * 12);
    v.extend_from_slice(&MAGIC);
    v.push(MSG_WATCH);
    v.extend_from_slice(&rid.to_be_bytes());
    v.push(slots.len() as u8);
    for s in slots {
        v.extend_from_slice(&s.addr.to_be_bytes());
        v.push(s.len);
        v.push(s.at);
        v.push(s.values.len() as u8);
        v.extend_from_slice(&s.values);
    }
    Ok(v)
}

/// Parse one APPLICATION payload. Returns `NotM64p` for anything else on the
/// channel (M64T shares it), which callers should treat as "not mine", not an error.
pub fn parse(app: &[u8]) -> Result<Response, Error> {
    if app.len() < 5 || app[0..4] != MAGIC {
        return Err(Error::NotM64p);
    }
    let msg = app[4];
    let b = &app[5..];

    let be16 = |o: usize| -> u16 { u16::from_be_bytes([b[o], b[o + 1]]) };

    match msg {
        MSG_HELLO_ACK => {
            if b.len() < 8 {
                return Err(Error::Truncated);
            }
            let flags = b[7];
            let rom_bytes = if flags & FLAG_CART_ROM != 0 {
                if b.len() < 12 {
                    return Err(Error::Truncated);
                }
                Some(u32::from_be_bytes([b[8], b[9], b[10], b[11]]))
            } else {
                None
            };
            // Appended fields come in bit order, each present only if its bit is set, so
            // watch_slots sits after rom_bytes when there is one and after flags when not.
            let mut off = if rom_bytes.is_some() { 12 } else { 8 };
            let watch_slots = if flags & FLAG_WATCH != 0 {
                if b.len() < off + 1 {
                    return Err(Error::Truncated);
                }
                off += 1;
                Some(b[off - 1])
            } else {
                None
            };
            let _ = off;
            Ok(Response::HelloAck {
                proto: b[0],
                agent_ver: be16(1),
                rdram_bytes: u32::from_be_bytes([b[3], b[4], b[5], b[6]]),
                flags,
                rom_bytes,
                watch_slots,
            })
        }
        MSG_PEEKV_RESP | MSG_PEEKROM_RESP => {
            if b.len() < 3 {
                return Err(Error::Truncated);
            }
            let rid = be16(0);
            let n = b[2] as usize;
            let mut regions = Vec::with_capacity(n);
            let mut off = 3usize;
            for _ in 0..n {
                if off + 2 > b.len() {
                    return Err(Error::Truncated);
                }
                let len = u16::from_be_bytes([b[off], b[off + 1]]) as usize;
                off += 2;
                if off + len > b.len() {
                    return Err(Error::Truncated);
                }
                regions.push(b[off..off + len].to_vec());
                off += len;
            }
            if msg == MSG_PEEKROM_RESP {
                return Ok(Response::PeekRom { rid, regions });
            }
            // The trailer is there only for a host that set a watch, so its absence is
            // the normal case and never an error.
            let mut events = Vec::new();
            if off + 3 <= b.len() {
                let n = b[off] as usize;
                off += 3; // n, then dropped:u16, which the agent also counts for itself
                for _ in 0..n {
                    if off + 2 > b.len() {
                        return Err(Error::Truncated);
                    }
                    let (slot, len) = (b[off], b[off + 1] as usize);
                    off += 2;
                    if off + len > b.len() {
                        return Err(Error::Truncated);
                    }
                    events.push(Event {
                        slot,
                        bytes: b[off..off + len].to_vec(),
                    });
                    off += len;
                }
            }
            Ok(Response::PeekV {
                rid,
                regions,
                events,
            })
        }
        MSG_POKE_ACK => {
            if b.len() < 3 {
                return Err(Error::Truncated);
            }
            Ok(Response::PokeAck {
                rid: be16(0),
                applied: b[2],
            })
        }
        MSG_WATCH_ACK => {
            if b.len() < 3 {
                return Err(Error::Truncated);
            }
            Ok(Response::WatchAck {
                rid: be16(0),
                watching: b[2],
            })
        }
        MSG_ERR => {
            if b.len() < 3 {
                return Err(Error::Truncated);
            }
            Ok(Response::Err {
                rid: be16(0),
                code: b[2],
            })
        }
        // Request ids are host-to-cart only, so receiving one means we read back
        // our own bytes rather than a reply.
        MSG_HELLO | MSG_PEEKV | MSG_POKEV | MSG_PEEKROM | MSG_WATCH => {
            Err(Error::EchoedRequest(msg))
        }
        other => Err(Error::UnknownMsg(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent that watches appends watch_slots after rom_bytes, in flag-bit order.
    #[test]
    fn hello_ack_with_a_watching_agent() {
        let mut app = b"M64P\x81".to_vec();
        app.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x07]);
        app.extend_from_slice(&[0x04, 0x00, 0x00, 0x00]);
        app.push(4);
        assert_eq!(
            parse(&app).unwrap(),
            Response::HelloAck {
                proto: 0,
                agent_ver: 0x0100,
                rdram_bytes: 0x800000,
                flags: 7,
                rom_bytes: Some(0x0400_0000),
                watch_slots: Some(4),
            }
        );
    }

    /// Without PEEKROM there is no rom_bytes to skip, so watch_slots follows the flags.
    #[test]
    fn hello_ack_that_watches_but_does_not_read_the_rom() {
        let mut app = b"M64P\x81".to_vec();
        app.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x05]);
        app.push(2);
        match parse(&app).unwrap() {
            Response::HelloAck {
                rom_bytes,
                watch_slots,
                ..
            } => assert_eq!((rom_bytes, watch_slots), (None, Some(2))),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn a_watch_names_each_slot_and_its_filter() {
        let slots = [Slot {
            addr: 0x0040_002C,
            len: 4,
            at: 1,
            values: vec![0x01, 0x02],
        }];
        assert_eq!(
            encode_watch(0x0102, &slots).unwrap(),
            b"M64P\x05\x01\x02\x01\x00\x40\x00\x2c\x04\x01\x02\x01\x02"
        );
        // No slots is how a host stops watching, and is not an error.
        assert_eq!(encode_watch(1, &[]).unwrap(), b"M64P\x05\x00\x01\x00");
    }

    #[test]
    fn a_watch_the_wire_cannot_carry_is_refused_before_it_is_sent() {
        let too_long = Slot {
            addr: 0,
            len: (WATCH_MAX_LEN + 1) as u8,
            at: 0,
            values: vec![],
        };
        assert!(encode_watch(1, &[too_long]).is_err());
        let too_many_values = Slot {
            addr: 0,
            len: 4,
            at: 0,
            values: vec![0; WATCH_MAX_VALUES + 1],
        };
        assert!(encode_watch(1, &[too_many_values]).is_err());
    }

    #[test]
    fn watch_ack_parses() {
        let mut app = b"M64P\x85".to_vec();
        app.extend_from_slice(&[0x00, 0x09, 1]);
        assert_eq!(
            parse(&app).unwrap(),
            Response::WatchAck {
                rid: 9,
                watching: 1
            }
        );
    }

    /// What a watched slot held rides the response the host was already waiting for.
    #[test]
    fn a_peekv_response_carries_what_the_slot_held() {
        let mut app = b"M64P\x82".to_vec();
        app.extend_from_slice(&[0x00, 0x05, 1]);
        app.extend_from_slice(&[0x00, 0x02, 0xAA, 0xBB]);
        app.extend_from_slice(&[2, 0x00, 0x03]); // two events, three dropped
        app.extend_from_slice(&[0, 4, 0x60, 0x01, 0x00, 0x11]);
        app.extend_from_slice(&[1, 2, 0x77, 0x88]);
        match parse(&app).unwrap() {
            Response::PeekV {
                rid,
                regions,
                events,
            } => {
                assert_eq!(rid, 5);
                assert_eq!(regions, vec![vec![0xAA, 0xBB]]);
                assert_eq!(
                    events,
                    vec![
                        Event {
                            slot: 0,
                            bytes: vec![0x60, 0x01, 0x00, 0x11]
                        },
                        Event {
                            slot: 1,
                            bytes: vec![0x77, 0x88]
                        },
                    ]
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// A trailer cut off mid-event is a broken response, not an empty one.
    #[test]
    fn a_truncated_trailer_is_an_error() {
        let mut app = b"M64P\x82".to_vec();
        app.extend_from_slice(&[0x00, 0x05, 0]);
        app.extend_from_slice(&[1, 0x00, 0x00]);
        app.extend_from_slice(&[0, 4, 0x60]); // says four bytes, sends one
        assert_eq!(parse(&app), Err(Error::Truncated));
    }

    #[test]
    fn hello_is_just_magic_and_msg() {
        assert_eq!(encode_hello(), b"M64P\x01");
    }

    #[test]
    fn peekv_layout_matches_spec() {
        let regions = [
            Region {
                addr: 0x11A5D0,
                len: 16,
            },
            Region {
                addr: 0x40002C,
                len: 4,
            },
        ];
        let w = encode_peekv(0xBEEF, &regions).unwrap();
        assert_eq!(&w[0..5], b"M64P\x02");
        assert_eq!(&w[5..7], &[0xBE, 0xEF]); // rid
        assert_eq!(w[7], 2); // n
        assert_eq!(&w[8..12], &[0x00, 0x11, 0xA5, 0xD0]);
        assert_eq!(&w[12..14], &[0x00, 0x10]);
        assert_eq!(&w[14..18], &[0x00, 0x40, 0x00, 0x2C]);
        assert_eq!(&w[18..20], &[0x00, 0x04]);
        assert_eq!(w.len(), 20);
    }

    #[test]
    fn pokev_carries_data_inline() {
        let w = encode_pokev(1, &[(0x200000, &[0xDE, 0xAD][..])]).unwrap();
        assert_eq!(&w[0..5], b"M64P\x03");
        assert_eq!(w[7], 1);
        assert_eq!(&w[8..12], &[0x00, 0x20, 0x00, 0x00]);
        assert_eq!(&w[12..14], &[0x00, 0x02]);
        assert_eq!(&w[14..16], &[0xDE, 0xAD]);
    }

    #[test]
    fn peekv_response_round_trips() {
        // Shaped exactly as the cart builds it.
        let mut app = b"M64P\x82".to_vec();
        app.extend_from_slice(&[0xBE, 0xEF, 2]);
        app.extend_from_slice(&[0x00, 0x03, 1, 2, 3]);
        app.extend_from_slice(&[0x00, 0x02, 9, 8]);
        match parse(&app).unwrap() {
            Response::PeekV {
                rid,
                regions,
                events,
            } => {
                assert_eq!(rid, 0xBEEF);
                assert_eq!(regions, vec![vec![1, 2, 3], vec![9, 8]]);
                assert!(events.is_empty(), "no watch was set, so no trailer");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn hello_ack_parses() {
        let mut app = b"M64P\x81".to_vec();
        app.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x01]);
        assert_eq!(
            parse(&app).unwrap(),
            Response::HelloAck {
                proto: 0,
                agent_ver: 0x0100,
                rdram_bytes: 0x800000,
                flags: 1,
                rom_bytes: None,
                watch_slots: None,
            }
        );
    }

    /// An agent with PEEKROM appends rom_bytes after the flags; every earlier field keeps
    /// its offset.
    #[test]
    fn hello_ack_with_the_cart_rom_window() {
        let mut app = b"M64P\x81".to_vec();
        app.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x03]);
        app.extend_from_slice(&[0x04, 0x00, 0x00, 0x00]);
        assert_eq!(
            parse(&app).unwrap(),
            Response::HelloAck {
                proto: 0,
                agent_ver: 0x0100,
                rdram_bytes: 0x800000,
                flags: 3,
                rom_bytes: Some(0x0400_0000),
                watch_slots: None,
            }
        );
        // The flag without the field is a truncated message, not a zero-sized window.
        assert_eq!(parse(&app[..13]), Err(Error::Truncated));
    }

    #[test]
    fn peekrom_shares_peekv_layout() {
        let r = [Region {
            addr: 0x20,
            len: 20,
        }];
        let rom = encode_peekrom(5, &r).unwrap();
        let ram = encode_peekv(5, &r).unwrap();
        assert_eq!(rom[4], MSG_PEEKROM);
        assert_eq!(rom[5..], ram[5..]);
        let mut resp = b"M64P\x84\x00\x05\x01\x00\x02".to_vec();
        resp.extend_from_slice(b"AB");
        assert_eq!(
            parse(&resp).unwrap(),
            Response::PeekRom {
                rid: 5,
                regions: vec![b"AB".to_vec()]
            }
        );
        assert_eq!(parse(&rom), Err(Error::EchoedRequest(MSG_PEEKROM)));
    }

    #[test]
    fn err_and_ack_parse() {
        assert_eq!(
            parse(b"M64P\xE0\x00\x07\x04").unwrap(),
            Response::Err { rid: 7, code: 4 }
        );
        assert_eq!(
            parse(b"M64P\x83\x00\x09\x02").unwrap(),
            Response::PokeAck { rid: 9, applied: 2 }
        );
    }

    #[test]
    fn m64t_payloads_are_not_ours() {
        assert_eq!(parse(b"M64T\x01").unwrap_err(), Error::NotM64p);
        assert_eq!(parse(b"").unwrap_err(), Error::NotM64p);
    }

    /// Reading back a request means the agent never wrote a reply over it -- a
    /// silent agent, not corruption. Telling the two apart is what keeps a
    /// console reset from costing a transport rebuild, and a rebuild redoes
    /// HELLO and drops the OoT client with it.
    #[test]
    fn a_request_read_back_is_an_echo_not_an_unknown_message() {
        for msg in [MSG_HELLO, MSG_PEEKV, MSG_POKEV] {
            let app = [b'M', b'6', b'4', b'P', msg];
            assert_eq!(
                parse(&app).unwrap_err(),
                Error::EchoedRequest(msg),
                "0x{msg:02X} is host-to-cart, so receiving it is our own bytes"
            );
        }
    }

    /// A reply id we do not know is still a real fault, and must not be quietly
    /// swallowed by the echo path.
    #[test]
    fn an_unrecognised_reply_is_still_an_error() {
        let app = [b'M', b'6', b'4', b'P', 0x8F];
        assert_eq!(
            parse(&app).unwrap_err(),
            Error::UnknownMsg(0x8F),
            "0x8F is in the reply range and means something is wrong"
        );
    }

    #[test]
    fn truncated_region_list_is_caught() {
        // Declares two regions, supplies one.
        let mut app = b"M64P\x82".to_vec();
        app.extend_from_slice(&[0, 1, 2]);
        app.extend_from_slice(&[0x00, 0x01, 0xFF]);
        assert_eq!(parse(&app).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn limits_are_enforced_before_sending() {
        let many: Vec<Region> = (0..33).map(|i| Region { addr: i, len: 1 }).collect();
        assert_eq!(
            encode_peekv(0, &many).unwrap_err(),
            Error::TooManyRegions(33)
        );

        let big = [Region { addr: 0, len: 4097 }];
        assert_eq!(
            encode_peekv(0, &big).unwrap_err(),
            Error::RegionTooLarge(4097)
        );

        let total: Vec<Region> = (0..2).map(|_| Region { addr: 0, len: 4096 }).collect();
        assert_eq!(
            encode_peekv(0, &total).unwrap_err(),
            Error::TotalTooLarge(8192)
        );
    }

    #[test]
    fn zero_regions_is_legal() {
        let w = encode_peekv(5, &[]).unwrap();
        assert_eq!(w[7], 0);
        assert_eq!(w.len(), 8);
    }
}
