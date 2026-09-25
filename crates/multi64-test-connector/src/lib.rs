//! Library for **`multi64_test.z64`** over **`multi64d`** WebSocket (L3 APPLICATION / **M64T**).
//! See [`docs/connectors/test-rom.md`](../../docs/connectors/test-rom.md).

pub mod suite;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use multi64_l3::{Channel, Frame, FrameFlags, FrameType, StreamDecoder};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

macro_rules! log_line {
    ($log:expr, $($arg:tt)*) => {
        $log(format!($($arg)*))
    };
}

type WsRead = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;
type WsWrite = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

/// ASCII `M64T` — see `docs/spec/test-l3-application-v0.md`.
const M64T_MAGIC: [u8; 4] = [0x4D, 0x36, 0x34, 0x54];
/// `M64P` — the RDRAM peek/poke profile (`docs/spec/memory-l3-application-v0.md`). Shares the
/// APPLICATION channel with M64T; the ROM dispatches on this magic, not on its mode.
const M64P_MAGIC: [u8; 4] = [0x4D, 0x36, 0x34, 0x50];

const M64P_MSG_HELLO: u8 = 0x01;
const M64P_MSG_PEEKV: u8 = 0x02;
const M64P_MSG_POKEV: u8 = 0x03;
const M64P_MSG_HELLO_ACK: u8 = 0x81;
const M64P_MSG_PEEKV_RESP: u8 = 0x82;
const M64P_MSG_POKE_ACK: u8 = 0x83;
const M64P_MSG_PEEKROM: u8 = 0x04;
const M64P_MSG_PEEKROM_RESP: u8 = 0x84;
const M64P_MSG_ERR: u8 = 0xE0;

/// Name for an `ERR` code, so a failure says what was wrong rather than printing a number.
fn m64p_err_name(code: u8) -> &'static str {
    match code {
        0x01 => "malformed",
        0x02 => "too many regions",
        0x03 => "region or total too large",
        0x04 => "address outside RDRAM",
        0x05 => "agent does not accept writes",
        0x06 => "agent cannot read the cart ROM",
        0x07 => "PI stayed busy; try again",
        _ => "unknown",
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
#[allow(dead_code)]
enum M64tMsg {
    Ping = 0x01,
    Echo = 0x02,
    ReqVersion = 0x03,
    ReqController = 0x04,
    SessionOpen = 0x05,
    SessionClose = 0x06,
    ReqEepromInfo = 0x07,
    ReqEepromRead = 0x08,
    ReqEepromWrite = 0x09,
    ReqSramInfo = 0x0A,
    ReqSramRead = 0x0B,
    ReqSramWrite = 0x0C,
    ReqRumble = 0x0D,
    ReqDisplayText = 0x0E,
    ReqSetMode = 0x0F,
    ReqDiag = 0x10,
    Pong = 0x81,
    EchoReply = 0x82,
    Version = 0x83,
    Controller = 0x84,
    SessionAck = 0x85,
    SessionEnd = 0x86,
    EepromInfo = 0x87,
    EepromData = 0x88,
    EepromStatus = 0x89,
    SramInfo = 0x8A,
    SramData = 0x8B,
    SramStatus = 0x8C,
    RumbleAck = 0x8D,
    DisplayTextAck = 0x8E,
    SetModeAck = 0x8F,
    Diag = 0x90,
    StressLarge = 0xE1,
    BenchTick = 0xF0,
    ControllerPollExit = 0xF1,
}

/// One connector action (same surface as the CLI subcommands). Used by the GUI and tests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ConnectorCommand {
    Ping,
    Echo {
        hex: Option<String>,
        text: Option<String>,
    },
    Version,
    ReqController,
    SessionOpen {
        #[serde(default = "default_hex_challenge")]
        hex_challenge: String,
    },
    SessionClose,
    EepromInfo,
    EepromRead {
        offset: u16,
        len: u16,
    },
    EepromWrite {
        offset: u16,
        hex: String,
    },
    SramInfo,
    SramRead {
        offset: u32,
        len: u16,
    },
    SramWrite {
        offset: u32,
        hex: String,
    },
    Rumble {
        #[serde(default)]
        port: u8,
        #[serde(default = "default_rumble_frames")]
        frames: u8,
    },
    DisplayText {
        #[serde(default)]
        text: String,
    },
    /// Put the ROM into a mode (`0` RAW_ECHO … `4` MEM_AGENT). Honored in every mode, RAW_ECHO
    /// included, which is what makes an unattended run possible at all.
    SetMode {
        mode: u8,
    },
    /// Read the ROM's counter snapshot. `expect_clean` fails the command when the stream-health
    /// counters are non-zero, which is how a run asserts the link stayed in step.
    Diag {
        #[serde(default)]
        expect_clean: bool,
    },
    /// M64P `HELLO` — protocol version, RDRAM size, and whether the agent accepts writes.
    MemHello,
    MemPeek {
        addr: u32,
        len: u16,
    },
    MemPoke {
        addr: u32,
        hex: String,
    },
    /// M64P `PEEKROM` — read the cartridge ROM (`addr` is a ROM offset). With `expect_hex`, the
    /// command fails unless those are the bytes read.
    MemRomPeek {
        addr: u32,
        len: u16,
        #[serde(default)]
        expect_hex: Option<String>,
    },
    /// Write a pattern into the ROM's scratch region, read it back, and restore what was there.
    ///
    /// The address comes from `DIAG`, never from the caller: every other address in RDRAM belongs
    /// to the ROM or libdragon, and this is the only M64P check that proves a write actually
    /// landed rather than merely that a reply came back.
    MemRoundTrip {
        #[serde(default = "default_round_trip_len")]
        len: u16,
    },
}

fn default_round_trip_len() -> u16 {
    64
}

fn default_hex_challenge() -> String {
    "0000000000000000".into()
}

fn default_rumble_frames() -> u8 {
    60
}

fn build_m64t_payload(msg: u8, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(5 + body.len());
    p.extend_from_slice(&M64T_MAGIC);
    p.push(msg);
    p.extend_from_slice(body);
    p
}

fn build_m64p_payload(msg: u8, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(5 + body.len());
    p.extend_from_slice(&M64P_MAGIC);
    p.push(msg);
    p.extend_from_slice(body);
    p
}

/// `PEEKV` body: `rid`, region count, then each `addr`/`len` (spec §2).
fn m64p_peek_body(rid: u16, regions: &[(u32, u16)]) -> Vec<u8> {
    let mut b = Vec::with_capacity(3 + regions.len() * 6);
    b.extend_from_slice(&rid.to_be_bytes());
    b.push(regions.len() as u8);
    for (addr, len) in regions {
        b.extend_from_slice(&addr.to_be_bytes());
        b.extend_from_slice(&len.to_be_bytes());
    }
    b
}

/// `POKEV` body: `rid`, region count, then each `addr`/`len` followed by that region's bytes.
fn m64p_poke_body(rid: u16, regions: &[(u32, &[u8])]) -> Vec<u8> {
    let mut b = Vec::with_capacity(3 + regions.iter().map(|(_, d)| 6 + d.len()).sum::<usize>());
    b.extend_from_slice(&rid.to_be_bytes());
    b.push(regions.len() as u8);
    for (addr, data) in regions {
        b.extend_from_slice(&addr.to_be_bytes());
        b.extend_from_slice(&(data.len() as u16).to_be_bytes());
        b.extend_from_slice(data);
    }
    b
}

/// First region's bytes out of a `PEEKV_RESP` body (`rid`, count, then `len`+bytes per region).
fn m64p_first_region(body: &[u8]) -> Result<Vec<u8>> {
    if body.len() < 3 {
        anyhow::bail!("PEEKV_RESP body too short ({} bytes)", body.len());
    }
    if body[2] == 0 {
        anyhow::bail!("PEEKV_RESP carried no regions");
    }
    if body.len() < 5 {
        anyhow::bail!("PEEKV_RESP region header truncated");
    }
    let len = u16::from_be_bytes([body[3], body[4]]) as usize;
    let start = 5;
    if body.len() < start + len {
        anyhow::bail!(
            "PEEKV_RESP declared {len} bytes but carried {}",
            body.len() - start
        );
    }
    Ok(body[start..start + len].to_vec())
}

fn l3_data_application(payload: Vec<u8>) -> Result<Vec<u8>> {
    let f = Frame {
        ty: FrameType::Data,
        channel: Channel::Application,
        flags: FrameFlags::FINAL,
        request_id: 0,
        payload,
    };
    f.encode().map_err(Into::into)
}

fn parse_hex_body(s: &str) -> Result<Vec<u8>> {
    let h = s.trim().replace(' ', "");
    // Before slicing by byte offset below, which would split a multi-byte character and panic
    // (#149). This also rejects the sign `from_str_radix` would accept.
    if let Some(bad) = h.chars().find(|c| !c.is_ascii_hexdigit()) {
        anyhow::bail!("invalid hex: {bad:?} is not a hex digit");
    }
    if h.len() % 2 != 0 {
        anyhow::bail!("--hex must have an even number of hex digits");
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("invalid hex: {e}"))
}

fn parse_hex_fixed8(s: &str) -> Result<[u8; 8]> {
    let v = parse_hex_body(s)?;
    if v.len() != 8 {
        anyhow::bail!("expected exactly 8 bytes (16 hex digits)");
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&v);
    Ok(a)
}

fn body_eeprom_read(offset: u16, len: u16) -> Vec<u8> {
    let mut b = Vec::with_capacity(4);
    b.extend_from_slice(&offset.to_be_bytes());
    b.extend_from_slice(&len.to_be_bytes());
    b
}

fn body_sram_read(offset: u32, len: u16) -> Vec<u8> {
    let mut b = Vec::with_capacity(6);
    b.extend_from_slice(&offset.to_be_bytes());
    b.extend_from_slice(&len.to_be_bytes());
    b
}

fn emit_m64t_payload<F: FnMut(String) + Send>(p: &[u8], log: &mut F) {
    if p.len() < 5 || p[0..4] != M64T_MAGIC {
        log_line!(
            log,
            "  (not M64T) hex: {}",
            p.iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join("")
        );
        return;
    }
    let msg = p[4];
    let body = &p[5..];
    match msg {
        0x01 => log_line!(log, "M64T: PING ({} body bytes)", body.len()),
        0x02 => log_line!(log, "M64T: ECHO ({} body bytes)", body.len()),
        0x03 => log_line!(log, "M64T: REQ_VERSION"),
        0x04 => log_line!(log, "M64T: REQ_CONTROLLER"),
        0x0D => {
            if body.len() >= 2 {
                log_line!(log, "M64T: REQ_RUMBLE port={} frames={}", body[0], body[1])
            } else {
                log_line!(log, "M64T: REQ_RUMBLE (short)")
            }
        }
        0x0E => {
            log_line!(log, "M64T: REQ_DISPLAY_TEXT ({} bytes)", body.len());
            if !body.is_empty() {
                log_line!(log, "  text: {:?}", String::from_utf8_lossy(body));
            }
        }
        0x81 => log_line!(log, "M64T: PONG"),
        0x82 => {
            log_line!(log, "M64T: ECHO_REPLY ({} bytes)", body.len());
            log_line!(
                log,
                "  body hex: {}",
                body.iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join("")
            );
        }
        0x83 => {
            let s = String::from_utf8_lossy(body);
            log_line!(log, "M64T: VERSION {:?}", s)
        }
        0x84 => {
            if body.len() >= 9 {
                let raw = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let sx = body[4] as i8;
                let sy = body[5] as i8;
                let port = body[8];
                log_line!(
                    log,
                    "M64T: CONTROLLER raw_buttons=0x{:04X} stick=({}, {}) port={}",
                    raw,
                    sx,
                    sy,
                    port
                )
            } else if body.len() >= 8 {
                let raw = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let sx = body[4] as i8;
                let sy = body[5] as i8;
                log_line!(
                    log,
                    "M64T: CONTROLLER raw_buttons=0x{:04X} stick=({}, {}) (no port byte)",
                    raw,
                    sx,
                    sy
                )
            } else {
                log_line!(log, "M64T: CONTROLLER (truncated, {} bytes)", body.len())
            }
        }
        0x85 => {
            if body.len() >= 16 {
                log_line!(
                    log,
                    "M64T: SESSION_ACK challenge={} session_id={} flags=0x{:08X}",
                    body[0..8]
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(""),
                    u32::from_be_bytes(body[8..12].try_into().unwrap()),
                    u32::from_be_bytes(body[12..16].try_into().unwrap())
                )
            } else {
                log_line!(log, "M64T: SESSION_ACK (short body)")
            }
        }
        0x86 => log_line!(log, "M64T: SESSION_END"),
        0x87 => {
            if body.len() >= 3 {
                log_line!(
                    log,
                    "M64T: EEPROM_INFO type={} blocks={}",
                    body[0],
                    u16::from_be_bytes(body[1..3].try_into().unwrap())
                )
            } else {
                log_line!(log, "M64T: EEPROM_INFO (short)")
            }
        }
        0x88 => {
            if body.len() >= 4 {
                let off = u16::from_be_bytes(body[0..2].try_into().unwrap());
                let n = u16::from_be_bytes(body[2..4].try_into().unwrap()) as usize;
                log_line!(log, "M64T: EEPROM_DATA offset={} len={}", off, n);
                if body.len() >= 4 + n {
                    log_line!(
                        log,
                        "  hex: {}",
                        body[4..4 + n]
                            .iter()
                            .map(|b| format!("{:02x}", b))
                            .collect::<Vec<_>>()
                            .join("")
                    )
                }
            } else {
                log_line!(log, "M64T: EEPROM_DATA (short)")
            }
        }
        0x89 => {
            if !body.is_empty() {
                log_line!(log, "M64T: EEPROM_STATUS code={}", body[0])
            } else {
                log_line!(log, "M64T: EEPROM_STATUS (empty)")
            }
        }
        0x8A => {
            if body.len() >= 8 {
                log_line!(
                    log,
                    "M64T: SRAM_INFO size_bytes={} pi_base=0x{:08X}",
                    u32::from_be_bytes(body[0..4].try_into().unwrap()),
                    u32::from_be_bytes(body[4..8].try_into().unwrap())
                )
            } else {
                log_line!(log, "M64T: SRAM_INFO (short)")
            }
        }
        0x8B => {
            if body.len() >= 6 {
                let off = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let n = u16::from_be_bytes(body[4..6].try_into().unwrap()) as usize;
                log_line!(log, "M64T: SRAM_DATA offset={} len={}", off, n);
                if body.len() >= 6 + n {
                    log_line!(
                        log,
                        "  hex: {}",
                        body[6..6 + n]
                            .iter()
                            .map(|b| format!("{:02x}", b))
                            .collect::<Vec<_>>()
                            .join("")
                    )
                }
            } else {
                log_line!(log, "M64T: SRAM_DATA (short)")
            }
        }
        0x8C => {
            if !body.is_empty() {
                log_line!(log, "M64T: SRAM_STATUS code={}", body[0])
            } else {
                log_line!(log, "M64T: SRAM_STATUS (empty)")
            }
        }
        0x8D => {
            if body.len() >= 3 {
                log_line!(
                    log,
                    "M64T: RUMBLE_ACK port={} frames={} status={}",
                    body[0],
                    body[1],
                    body[2]
                )
            } else {
                log_line!(log, "M64T: RUMBLE_ACK (short)")
            }
        }
        0x8E => {
            if !body.is_empty() {
                log_line!(log, "M64T: DISPLAY_TEXT_ACK status={}", body[0])
            } else {
                log_line!(log, "M64T: DISPLAY_TEXT_ACK (empty)")
            }
        }
        0xE1 => log_line!(log, "M64T: STRESS_LARGE ({} body bytes)", body.len()),
        0xF0 => {
            if body.len() >= 4 {
                let tick = u32::from_be_bytes(body[0..4].try_into().unwrap());
                log_line!(log, "M64T: BENCH_TICK frame={}", tick)
            } else {
                log_line!(log, "M64T: BENCH_TICK (short body)")
            }
        }
        0xF1 => log_line!(log, "M64T: CONTROLLER_POLL_EXIT (ROM left CTRL_POLL mode)"),
        _ => log_line!(log, "M64T: msg=0x{:02X} ({} body bytes)", msg, body.len()),
    }
}

fn emit_l3_frame<F: FnMut(String) + Send>(frame: &Frame, log: &mut F) {
    log_line!(
        log,
        "L3: type={:?} channel={:?} payload_len={}",
        frame.ty,
        frame.channel,
        frame.payload.len()
    );
    if frame.ty == FrameType::Data && frame.channel == Channel::Application {
        emit_m64t_payload(&frame.payload, log);
    }
}

async fn ws_hello<F: FnMut(String) + Send>(
    read: &mut WsRead,
    write: &mut WsWrite,
    log: &mut F,
) -> Result<()> {
    let hello = read
        .next()
        .await
        .context("ws closed before hello")?
        .context("ws hello")?;
    let Message::Text(hello_s) = hello else {
        anyhow::bail!("expected text hello, got {:?}", hello);
    };
    let v: serde_json::Value = serde_json::from_str(&hello_s).context("parse hello json")?;
    if v.get("type").and_then(|x| x.as_str()) != Some("hello") {
        anyhow::bail!("unexpected hello: {}", hello_s);
    }
    log_line!(log, "hello: {}", hello_s.trim());

    write
        .send(Message::text(r#"{"type":"ping"}"#))
        .await
        .context("send json ping")?;
    let pong = read
        .next()
        .await
        .context("ws closed before pong")?
        .context("ws pong")?;
    let Message::Text(pong_s) = pong else {
        anyhow::bail!("expected text pong, got {:?}", pong);
    };
    let pj: serde_json::Value = serde_json::from_str(&pong_s).context("parse pong")?;
    if pj.get("type").and_then(|x| x.as_str()) != Some("pong") {
        anyhow::bail!("unexpected pong: {}", pong_s);
    }
    Ok(())
}

/// Decode binary WS chunks until we emit an M64T `APPLICATION` frame whose `msg` is in `want_msgs`.
/// Decode binary WS chunks until an APPLICATION frame carries `magic` and one of `want_msgs`,
/// and hand back that message's code and body (the payload past the 5-byte application header).
///
/// `Ok(None)` is a timeout or a closed socket, never an error: a cart that says nothing is the
/// normal failure here, and the caller decides what that means. Note that silence does **not**
/// distinguish "the cart did not answer" from "the daemon dropped the request because the link
/// was released or faulted" — writes to a released link are accepted and discarded, so a caller
/// that cares must check `GET /` on the daemon.
async fn recv_app_body<F: FnMut(String) + Send>(
    read: &mut WsRead,
    decoder: &mut StreamDecoder,
    deadline: Instant,
    magic: [u8; 4],
    want_msgs: &[u8],
    log: &mut F,
) -> Result<Option<(u8, Vec<u8>)>> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let next = tokio::time::timeout(remaining, read.next()).await;
        let msg = match next {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => return Err(e.into()),
            Ok(None) => return Ok(None),
            Err(_) => return Ok(None),
        };
        match msg {
            Message::Binary(bin) => {
                let mut got: Option<(u8, Vec<u8>)> = None;
                decoder.push_bytes(&bin, |frame| {
                    emit_l3_frame(&frame, log);
                    if frame.ty == FrameType::Data && frame.channel == Channel::Application {
                        let p = &frame.payload;
                        if got.is_none()
                            && p.len() >= 5
                            && p[0..4] == magic
                            && want_msgs.contains(&p[4])
                        {
                            got = Some((p[4], p[5..].to_vec()));
                        }
                    }
                });
                if got.is_some() {
                    return Ok(got);
                }
            }
            Message::Text(t) => log_line!(log, "ws text: {}", t),
            Message::Close(_) => return Ok(None),
            _ => {}
        }
    }
}

async fn recv_until_m64t_any<F: FnMut(String) + Send>(
    read: &mut WsRead,
    decoder: &mut StreamDecoder,
    deadline: Instant,
    want_msgs: &[u8],
    log: &mut F,
) -> Result<bool> {
    Ok(
        recv_app_body(read, decoder, deadline, M64T_MAGIC, want_msgs, log)
            .await?
            .is_some(),
    )
}

/// Decode binary WS chunks until we emit an M64T `APPLICATION` frame with `msg == want_msg`.
async fn recv_until_m64t<F: FnMut(String) + Send>(
    read: &mut WsRead,
    decoder: &mut StreamDecoder,
    deadline: Instant,
    want_msg: u8,
    log: &mut F,
) -> Result<bool> {
    recv_until_m64t_any(
        read,
        decoder,
        deadline,
        std::slice::from_ref(&want_msg),
        log,
    )
    .await
}

/// Name of a `run_mode`, so a message says which mode rather than a bare number.
pub fn mode_name(mode: u8) -> &'static str {
    match mode {
        0 => "RAW_ECHO",
        1 => "M64T_PROTO",
        2 => "BENCH",
        3 => "CTRL_POLL",
        4 => "MEM_AGENT",
        _ => "unknown",
    }
}

/// Name of a `cart_link_kind` as `DIAG` reports it.
pub fn cart_kind_name(kind: u8) -> &'static str {
    match kind {
        0 => "none",
        1 => "SummerCart64",
        2 => "EverDrive X-series",
        3 => "EverDrive-64 PRO",
        4 => "other/unsupported",
        _ => "unknown",
    }
}

/// The `DIAG` (`0x90`) body — see `docs/spec/test-l3-application-v0.md` §11.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiagSnapshot {
    pub mode: u8,
    pub cart_kind: u8,
    pub frames_handled: u32,
    pub rx_overflow: u32,
    pub rx_resync_bytes: u32,
    pub bad_header_drops: u32,
    pub rx_bytes: u32,
    pub tx_bytes: u32,
    pub scratch_addr: u32,
    pub scratch_len: u32,
    /// Cart writes that gave up before the whole message was sent, **since boot** — unlike the
    /// counters above, a mode change does not reset it. `None` from a version-1 body, which does
    /// not carry it.
    pub tx_failures: Option<u32>,
}

impl DiagSnapshot {
    /// Bytes in a version-1 body. Version 2 appends `tx_failures`.
    pub const BODY_LEN_V1: usize = 36;
    pub const BODY_LEN_V2: usize = 40;

    pub fn parse(body: &[u8]) -> Result<Self> {
        if body.is_empty() {
            anyhow::bail!("DIAG body is empty");
        }
        // Refusing an unknown version is the point of the byte: a later ROM may add fields, and
        // reading them at these offsets would report confident nonsense.
        let need = match body[0] {
            1 => Self::BODY_LEN_V1,
            2 => Self::BODY_LEN_V2,
            v => anyhow::bail!(
                "DIAG body version {v} is not one this build understands (1 or 2) - update the \
                 connector to match the ROM"
            ),
        };
        if body.len() < need {
            anyhow::bail!(
                "DIAG body version {} is {} bytes, expected at least {need}",
                body[0],
                body.len()
            );
        }
        let be =
            |at: usize| u32::from_be_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
        Ok(Self {
            mode: body[1],
            cart_kind: body[2],
            frames_handled: be(4),
            rx_overflow: be(8),
            rx_resync_bytes: be(12),
            bad_header_drops: be(16),
            rx_bytes: be(20),
            tx_bytes: be(24),
            scratch_addr: be(28),
            scratch_len: be(32),
            tx_failures: (body[0] >= 2).then(|| be(36)),
        })
    }

    /// True when nothing desynchronized: a reply arriving proves the round trip, these prove the
    /// stream underneath it stayed in step.
    pub fn is_clean(&self) -> bool {
        self.rx_overflow == 0 && self.rx_resync_bytes == 0 && self.bad_header_drops == 0
    }

    pub fn summary(&self) -> String {
        format!(
            "mode={} cart={} frames={} rx={}B tx={}B overflow={} resync={}B bad_header={} scratch=0x{:08X}+{}",
            mode_name(self.mode),
            cart_kind_name(self.cart_kind),
            self.frames_handled,
            self.rx_bytes,
            self.tx_bytes,
            self.rx_overflow,
            self.rx_resync_bytes,
            self.bad_header_drops,
            self.scratch_addr,
            self.scratch_len
        ) + &self
            .tx_failures
            .map(|n| format!(" tx_failures={n}"))
            .unwrap_or_default()
    }
}

/// Which reply satisfies a round trip: the application magic, and the message codes to accept.
///
/// A list rather than one code because an error reply is still an answer — accepting `ERR`
/// alongside the success code is what turns a cart-side rejection into its own message instead of
/// a timeout that says nothing about why.
#[derive(Clone, Copy)]
struct Expect<'a> {
    magic: [u8; 4],
    msgs: &'a [u8],
}

/// Send one APPLICATION payload and wait for a reply matching `expect`.
async fn app_round_trip<F: FnMut(String) + Send>(
    write: &mut WsWrite,
    read: &mut WsRead,
    payload: Vec<u8>,
    what: &str,
    recv_to: Duration,
    expect: Expect<'_>,
    log: &mut F,
) -> Result<(u8, Vec<u8>)> {
    let wire = l3_data_application(payload)?;
    write
        .send(Message::binary(wire))
        .await
        .with_context(|| format!("send {}", what))?;
    log_line!(log, "sent {}", what);
    let mut decoder = StreamDecoder::new();
    let deadline = Instant::now() + recv_to;
    match recv_app_body(read, &mut decoder, deadline, expect.magic, expect.msgs, log).await? {
        Some(r) => Ok(r),
        None => anyhow::bail!(
            concat!(
                "timeout waiting for a reply to {} - the cart said nothing. ",
                "A write to a released or faulted link is accepted and discarded by the daemon, ",
                "so check GET / : serialActive:false means the request never reached the cart, ",
                "and serialActive:true means the cart itself did not answer (is the ROM booted?).",
            ),
            what
        ),
    }
}

fn hex_of(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{:02x}", x))
        .collect::<Vec<_>>()
        .join("")
}

/// One `PEEKV` region, returning its bytes. Errors carry the `ERR` code's meaning.
/// Which address space a peek reads: RDRAM (`PEEKV`) or the cartridge ROM (`PEEKROM`).
#[derive(Clone, Copy)]
enum PeekSpace {
    Rdram,
    CartRom,
}

/// One region from RDRAM (`PEEKV`).
async fn peek_region(
    write: &mut WsWrite,
    read: &mut WsRead,
    recv_to: Duration,
    addr: u32,
    len: u16,
    what: &str,
) -> Result<Vec<u8>> {
    peek(write, read, recv_to, PeekSpace::Rdram, addr, len, what).await
}

/// One region from either space. The two requests share a body and a reply layout.
async fn peek(
    write: &mut WsWrite,
    read: &mut WsRead,
    recv_to: Duration,
    space: PeekSpace,
    addr: u32,
    len: u16,
    what: &str,
) -> Result<Vec<u8>> {
    let (req, resp, name) = match space {
        PeekSpace::Rdram => (M64P_MSG_PEEKV, M64P_MSG_PEEKV_RESP, "PEEKV"),
        PeekSpace::CartRom => (M64P_MSG_PEEKROM, M64P_MSG_PEEKROM_RESP, "PEEKROM"),
    };
    let mut quiet = |_: String| {};
    let (msg, body) = app_round_trip(
        write,
        read,
        build_m64p_payload(req, &m64p_peek_body(1, &[(addr, len)])),
        what,
        recv_to,
        Expect {
            magic: M64P_MAGIC,
            msgs: &[resp, M64P_MSG_ERR],
        },
        &mut quiet,
    )
    .await?;
    if msg == M64P_MSG_ERR {
        let code = body.get(2).copied().unwrap_or(0);
        anyhow::bail!("{name} rejected: {} ({:#04x})", m64p_err_name(code), code);
    }
    m64p_first_region(&body)
}

/// One `POKEV` region, checking the cart reports exactly that region applied.
async fn poke_region(
    write: &mut WsWrite,
    read: &mut WsRead,
    recv_to: Duration,
    addr: u32,
    data: &[u8],
    what: &str,
) -> Result<()> {
    let mut quiet = |_: String| {};
    let (msg, body) = app_round_trip(
        write,
        read,
        build_m64p_payload(M64P_MSG_POKEV, &m64p_poke_body(1, &[(addr, data)])),
        what,
        recv_to,
        Expect {
            magic: M64P_MAGIC,
            msgs: &[M64P_MSG_POKE_ACK, M64P_MSG_ERR],
        },
        &mut quiet,
    )
    .await?;
    if msg == M64P_MSG_ERR {
        let code = body.get(2).copied().unwrap_or(0);
        anyhow::bail!("POKEV rejected: {} ({:#04x})", m64p_err_name(code), code);
    }
    let applied = body.get(2).copied().unwrap_or(0);
    if applied != 1 {
        anyhow::bail!("POKEV applied {} regions, expected 1", applied);
    }
    Ok(())
}

/// Run a single request/response M64T command (not [`run_listen`]).
pub async fn run_connector_command<F: FnMut(String) + Send>(
    url: &str,
    recv_timeout_secs: f64,
    command: &ConnectorCommand,
    log: &mut F,
) -> Result<()> {
    let recv_to = Duration::from_secs_f64(recv_timeout_secs.max(0.1));

    let (ws_stream, _) = connect_async(url)
        .await
        .with_context(|| format!("connect WebSocket {}", url))?;
    let (mut write, mut read) = ws_stream.split();

    ws_hello(&mut read, &mut write, log).await?;

    match command.clone() {
        ConnectorCommand::Ping => {
            let wire = l3_data_application(build_m64t_payload(M64tMsg::Ping as u8, &[]))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send PING")?;
            log_line!(log, "sent M64T PING");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(&mut read, &mut decoder, deadline, M64tMsg::Pong as u8, log).await? {
                log_line!(log, "OK: saw M64T PONG");
            } else {
                anyhow::bail!("timeout waiting for PONG (is the test ROM in M64T/BENCH mode?)");
            }
        }
        ConnectorCommand::Echo { hex, text } => {
            let body: Vec<u8> = if let Some(h) = hex {
                parse_hex_body(&h)?
            } else if let Some(t) = text {
                t.into_bytes()
            } else {
                vec![0xDE, 0xAD, 0xBE, 0xEF]
            };
            let wire = l3_data_application(build_m64t_payload(M64tMsg::Echo as u8, &body))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send ECHO")?;
            log_line!(log, "sent M64T ECHO ({} body bytes)", body.len());
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::EchoReply as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T ECHO_REPLY");
            } else {
                anyhow::bail!("timeout or missing ECHO_REPLY");
            }
        }
        ConnectorCommand::Version => {
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqVersion as u8, &[]))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_VERSION")?;
            log_line!(log, "sent M64T REQ_VERSION");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::Version as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T VERSION");
            } else {
                anyhow::bail!("timeout waiting for VERSION");
            }
        }
        ConnectorCommand::ReqController => {
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqController as u8, &[]))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_CONTROLLER")?;
            log_line!(log, "sent M64T REQ_CONTROLLER");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::Controller as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T CONTROLLER");
            } else {
                anyhow::bail!("timeout waiting for CONTROLLER");
            }
        }
        ConnectorCommand::SessionOpen { hex_challenge } => {
            let ch = parse_hex_fixed8(&hex_challenge)?;
            let wire = l3_data_application(build_m64t_payload(M64tMsg::SessionOpen as u8, &ch))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send SESSION_OPEN")?;
            log_line!(log, "sent M64T SESSION_OPEN");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::SessionAck as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T SESSION_ACK");
            } else {
                anyhow::bail!("timeout waiting for SESSION_ACK");
            }
        }
        ConnectorCommand::SessionClose => {
            let wire = l3_data_application(build_m64t_payload(M64tMsg::SessionClose as u8, &[]))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send SESSION_CLOSE")?;
            log_line!(log, "sent M64T SESSION_CLOSE");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::SessionEnd as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T SESSION_END");
            } else {
                anyhow::bail!("timeout waiting for SESSION_END");
            }
        }
        ConnectorCommand::EepromInfo => {
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqEepromInfo as u8, &[]))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_EEPROM_INFO")?;
            log_line!(log, "sent M64T REQ_EEPROM_INFO");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::EepromInfo as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T EEPROM_INFO");
            } else {
                anyhow::bail!("timeout waiting for EEPROM_INFO");
            }
        }
        ConnectorCommand::EepromRead { offset, len } => {
            let body = body_eeprom_read(offset, len);
            let wire =
                l3_data_application(build_m64t_payload(M64tMsg::ReqEepromRead as u8, &body))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_EEPROM_READ")?;
            log_line!(log, "sent M64T REQ_EEPROM_READ");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t_any(
                &mut read,
                &mut decoder,
                deadline,
                &[M64tMsg::EepromData as u8, M64tMsg::EepromStatus as u8],
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw EEPROM_DATA or EEPROM_STATUS");
            } else {
                anyhow::bail!("timeout waiting for EEPROM_DATA / EEPROM_STATUS");
            }
        }
        ConnectorCommand::EepromWrite { offset, hex } => {
            let data = parse_hex_body(&hex)?;
            if data.is_empty() || data.len() > 256 {
                anyhow::bail!("--hex must be 1..=256 bytes (even hex digit count)");
            }
            let mut body = Vec::with_capacity(4 + data.len());
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&(data.len() as u16).to_be_bytes());
            body.extend_from_slice(&data);
            let wire =
                l3_data_application(build_m64t_payload(M64tMsg::ReqEepromWrite as u8, &body))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_EEPROM_WRITE")?;
            log_line!(log, "sent M64T REQ_EEPROM_WRITE");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::EepromStatus as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T EEPROM_STATUS");
            } else {
                anyhow::bail!("timeout waiting for EEPROM_STATUS");
            }
        }
        ConnectorCommand::SramInfo => {
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqSramInfo as u8, &[]))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_SRAM_INFO")?;
            log_line!(log, "sent M64T REQ_SRAM_INFO");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::SramInfo as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T SRAM_INFO");
            } else {
                anyhow::bail!("timeout waiting for SRAM_INFO");
            }
        }
        ConnectorCommand::SramRead { offset, len } => {
            let body = body_sram_read(offset, len);
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqSramRead as u8, &body))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_SRAM_READ")?;
            log_line!(log, "sent M64T REQ_SRAM_READ");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t_any(
                &mut read,
                &mut decoder,
                deadline,
                &[M64tMsg::SramData as u8, M64tMsg::SramStatus as u8],
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw SRAM_DATA or SRAM_STATUS");
            } else {
                anyhow::bail!("timeout waiting for SRAM_DATA / SRAM_STATUS");
            }
        }
        ConnectorCommand::SramWrite { offset, hex } => {
            let data = parse_hex_body(&hex)?;
            if data.is_empty() || data.len() > 512 || (data.len() & 1) != 0 {
                anyhow::bail!("--hex must be 1..=512 bytes, even length");
            }
            let mut body = Vec::with_capacity(6 + data.len());
            body.extend_from_slice(&offset.to_be_bytes());
            body.extend_from_slice(&(data.len() as u16).to_be_bytes());
            body.extend_from_slice(&data);
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqSramWrite as u8, &body))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_SRAM_WRITE")?;
            log_line!(log, "sent M64T REQ_SRAM_WRITE");
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::SramStatus as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T SRAM_STATUS");
            } else {
                anyhow::bail!("timeout waiting for SRAM_STATUS");
            }
        }
        ConnectorCommand::Rumble { port, frames } => {
            if port > 3 {
                anyhow::bail!("--port must be 0..=3");
            }
            let body = vec![port, frames];
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqRumble as u8, &body))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_RUMBLE")?;
            log_line!(log, "sent M64T REQ_RUMBLE port={} frames={}", port, frames);
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::RumbleAck as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T RUMBLE_ACK");
            } else {
                anyhow::bail!("timeout waiting for RUMBLE_ACK");
            }
        }
        ConnectorCommand::DisplayText { text } => {
            let b = text.as_bytes();
            if b.len() > 120 {
                anyhow::bail!("text must be at most 120 UTF-8 bytes (got {})", b.len());
            }
            let wire = l3_data_application(build_m64t_payload(M64tMsg::ReqDisplayText as u8, b))?;
            write
                .send(Message::binary(wire))
                .await
                .context("send REQ_DISPLAY_TEXT")?;
            log_line!(log, "sent M64T REQ_DISPLAY_TEXT ({} bytes)", b.len());
            let mut decoder = StreamDecoder::new();
            let deadline = Instant::now() + recv_to;
            if recv_until_m64t(
                &mut read,
                &mut decoder,
                deadline,
                M64tMsg::DisplayTextAck as u8,
                log,
            )
            .await?
            {
                log_line!(log, "OK: saw M64T DISPLAY_TEXT_ACK");
            } else {
                anyhow::bail!("timeout waiting for DISPLAY_TEXT_ACK");
            }
        }
        ConnectorCommand::SetMode { mode } => {
            let (_, body) = app_round_trip(
                &mut write,
                &mut read,
                build_m64t_payload(M64tMsg::ReqSetMode as u8, &[mode]),
                "M64T REQ_SET_MODE",
                recv_to,
                Expect {
                    magic: M64T_MAGIC,
                    msgs: &[M64tMsg::SetModeAck as u8],
                },
                log,
            )
            .await?;
            if body.len() < 2 {
                anyhow::bail!("SET_MODE_ACK body is {} bytes, expected 2", body.len());
            }
            let (status, running) = (body[0], body[1]);
            if status != 0 {
                anyhow::bail!(
                    "cart refused mode {} ({}): status {}, still running {}",
                    mode,
                    mode_name(mode),
                    status,
                    mode_name(running)
                );
            }
            // The ack's second byte is authoritative, not the request: status 0 with a different
            // mode would mean the ROM and this tool disagree about the numbering.
            if running != mode {
                anyhow::bail!(
                    "cart acked mode {} but reports running {}",
                    mode_name(mode),
                    mode_name(running)
                );
            }
            log_line!(log, "OK: cart is in {}", mode_name(running));
        }
        ConnectorCommand::Diag { expect_clean } => {
            let (_, body) = app_round_trip(
                &mut write,
                &mut read,
                build_m64t_payload(M64tMsg::ReqDiag as u8, &[]),
                "M64T REQ_DIAG",
                recv_to,
                Expect {
                    magic: M64T_MAGIC,
                    msgs: &[M64tMsg::Diag as u8],
                },
                log,
            )
            .await?;
            let d = DiagSnapshot::parse(&body)?;
            log_line!(log, "DIAG {}", d.summary());
            if expect_clean && !d.is_clean() {
                anyhow::bail!(
                    "stream health counters are not clean: overflow={} resync={}B bad_header={}",
                    d.rx_overflow,
                    d.rx_resync_bytes,
                    d.bad_header_drops
                );
            }
            log_line!(log, "OK: DIAG read");
        }
        ConnectorCommand::MemHello => {
            let (msg, body) = app_round_trip(
                &mut write,
                &mut read,
                build_m64p_payload(M64P_MSG_HELLO, &[]),
                "M64P HELLO",
                recv_to,
                Expect {
                    magic: M64P_MAGIC,
                    msgs: &[M64P_MSG_HELLO_ACK, M64P_MSG_ERR],
                },
                log,
            )
            .await?;
            if msg == M64P_MSG_ERR {
                anyhow::bail!("M64P HELLO returned ERR");
            }
            if body.len() < 8 {
                anyhow::bail!("HELLO_ACK body is {} bytes, expected 8", body.len());
            }
            // rom_bytes follows the flags only when bit 1 says the agent reads the cart ROM.
            let cart_rom = (body[7] & 0x02) != 0;
            let rom_bytes = match (cart_rom, body.get(8..12)) {
                (true, Some(b)) => {
                    format!("{} bytes", u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                }
                (true, None) => {
                    anyhow::bail!("HELLO_ACK sets the cart ROM flag but carries no rom_bytes")
                }
                (false, _) => "no".to_string(),
            };
            log_line!(
                log,
                "OK: M64P proto={} agent_ver={} rdram={} bytes writable={} cart_rom={}",
                body[0],
                u16::from_be_bytes([body[1], body[2]]),
                u32::from_be_bytes([body[3], body[4], body[5], body[6]]),
                (body[7] & 0x01) != 0,
                rom_bytes
            );
        }
        ConnectorCommand::MemPeek { addr, len } => {
            let data = peek_region(&mut write, &mut read, recv_to, addr, len, "M64P PEEKV").await?;
            log_line!(
                log,
                "OK: read {} bytes at 0x{:08X}: {}",
                data.len(),
                addr,
                hex_of(&data)
            );
        }
        ConnectorCommand::MemRomPeek {
            addr,
            len,
            expect_hex,
        } => {
            let data = peek(
                &mut write,
                &mut read,
                recv_to,
                PeekSpace::CartRom,
                addr,
                len,
                "M64P PEEKROM",
            )
            .await?;
            if let Some(want) = expect_hex {
                let want = parse_hex_body(&want)?;
                if data != want {
                    anyhow::bail!(
                        "ROM 0x{addr:08X} holds {}, expected {}",
                        hex_of(&data),
                        hex_of(&want)
                    );
                }
            }
            log_line!(
                log,
                "OK: read {} bytes of cart ROM at 0x{:08X}: {}",
                data.len(),
                addr,
                hex_of(&data)
            );
        }
        ConnectorCommand::MemPoke { addr, hex } => {
            let data = parse_hex_body(&hex)?;
            if data.is_empty() {
                anyhow::bail!("--hex must supply at least one byte to write");
            }
            poke_region(&mut write, &mut read, recv_to, addr, &data, "M64P POKEV").await?;
            log_line!(log, "OK: wrote {} bytes at 0x{:08X}", data.len(), addr);
        }
        ConnectorCommand::MemRoundTrip { len } => {
            // Where to write comes from the ROM, never from the caller: DIAG reports a region the
            // ROM sets aside and never reads, and every other address in RDRAM belongs to the ROM
            // or to libdragon.
            let (_, diag_body) = app_round_trip(
                &mut write,
                &mut read,
                build_m64t_payload(M64tMsg::ReqDiag as u8, &[]),
                "M64T REQ_DIAG (for the scratch address)",
                recv_to,
                Expect {
                    magic: M64T_MAGIC,
                    msgs: &[M64tMsg::Diag as u8],
                },
                log,
            )
            .await?;
            let diag = DiagSnapshot::parse(&diag_body)?;
            if diag.scratch_len == 0 {
                anyhow::bail!("this ROM reports no M64P scratch region");
            }
            let cap = u16::try_from(diag.scratch_len).unwrap_or(u16::MAX);
            let n = len.min(cap);
            if n == 0 {
                anyhow::bail!("length must be at least 1");
            }
            let addr = diag.scratch_addr;
            log_line!(
                log,
                "scratch 0x{:08X} ({} bytes), using {}",
                addr,
                diag.scratch_len,
                n
            );

            let original = peek_region(
                &mut write,
                &mut read,
                recv_to,
                addr,
                n,
                "M64P PEEKV (original)",
            )
            .await?;
            if original.len() != usize::from(n) {
                anyhow::bail!("PEEKV returned {} bytes, asked for {}", original.len(), n);
            }

            // Neither a constant nor derived from the address, so a read that returns stale or
            // zeroed memory cannot match by coincidence.
            let pattern: Vec<u8> = (0..n).map(|i| (i.wrapping_mul(7) ^ 0x5A) as u8).collect();
            poke_region(
                &mut write,
                &mut read,
                recv_to,
                addr,
                &pattern,
                "M64P POKEV (pattern)",
            )
            .await?;

            let back = peek_region(
                &mut write,
                &mut read,
                recv_to,
                addr,
                n,
                "M64P PEEKV (verify)",
            )
            .await?;
            if back != pattern {
                let at = back
                    .iter()
                    .zip(pattern.iter())
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                anyhow::bail!(
                    "read-back differs at byte {}: wrote {:#04x}, read {:#04x}",
                    at,
                    pattern.get(at).copied().unwrap_or(0),
                    back.get(at).copied().unwrap_or(0)
                );
            }

            // Put back what was there, so a repeated run starts from the same state.
            poke_region(
                &mut write,
                &mut read,
                recv_to,
                addr,
                &original,
                "M64P POKEV (restore)",
            )
            .await?;
            let restored = peek_region(
                &mut write,
                &mut read,
                recv_to,
                addr,
                n,
                "M64P PEEKV (confirm)",
            )
            .await?;
            if restored != original {
                anyhow::bail!("scratch was not restored to its original contents");
            }
            log_line!(
                log,
                "OK: wrote, read back and restored {} bytes at 0x{:08X}",
                n,
                addr
            );
        }
    }

    Ok(())
}

/// Host-driven controller read: send `REQ_CONTROLLER` every `interval_ms` until the ROM sends
/// `CONTROLLER_POLL_EXIT` (0xF1, user held L+R ~5s in **CTRL_POLL** to return to MODE MENU) or `stop` is set.
pub async fn run_controller_poll<F: FnMut(String) + Send>(
    url: &str,
    recv_timeout_secs: f64,
    interval_ms: u64,
    stop: Option<Arc<AtomicBool>>,
    log: &mut F,
) -> Result<()> {
    let recv_to = Duration::from_secs_f64(recv_timeout_secs.max(0.1));
    let interval = Duration::from_millis(interval_ms.max(1));

    let (ws_stream, _) = connect_async(url)
        .await
        .with_context(|| format!("connect WebSocket {}", url))?;
    let (mut write, mut read) = ws_stream.split();

    ws_hello(&mut read, &mut write, log).await?;

    let wire_req = l3_data_application(build_m64t_payload(M64tMsg::ReqController as u8, &[]))?;
    let mut decoder = StreamDecoder::new();

    log_line!(
        log,
        "controller poll: REQ_CONTROLLER every {} ms (ROM mode CTRL_POLL; L+R ~5s -> MODE MENU; Stop to cancel)",
        interval_ms.max(1)
    );

    loop {
        if stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
            log_line!(log, "OK: controller poll stopped");
            break;
        }

        write
            .send(Message::binary(wire_req.clone()))
            .await
            .context("send REQ_CONTROLLER")?;

        let deadline = Instant::now() + recv_to;
        let mut saw_controller = false;
        let mut saw_exit = false;

        while Instant::now() < deadline && !saw_controller && !saw_exit {
            if stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst)) {
                saw_exit = true;
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let msg = tokio::time::timeout(remaining, read.next()).await;
            let msg = match msg {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => return Err(e.into()),
                Ok(None) => return Ok(()),
                Err(_) => break,
            };
            match msg {
                Message::Binary(bin) => {
                    decoder.push_bytes(&bin, |frame| {
                        emit_l3_frame(&frame, log);
                        if frame.ty == FrameType::Data && frame.channel == Channel::Application {
                            let p = &frame.payload;
                            if p.len() >= 5 && p[0..4] == M64T_MAGIC {
                                match p[4] {
                                    x if x == M64tMsg::Controller as u8 => saw_controller = true,
                                    x if x == M64tMsg::ControllerPollExit as u8 => saw_exit = true,
                                    _ => {}
                                }
                            }
                        }
                    });
                }
                Message::Text(t) => log_line!(log, "ws text: {}", t),
                Message::Close(_) => return Ok(()),
                _ => {}
            }
        }

        if saw_exit {
            log_line!(log, "OK: controller poll ended (ROM exit or host stop)");
            break;
        }
        if !saw_controller {
            anyhow::bail!("timeout waiting for CONTROLLER (is the test ROM in CTRL_POLL mode?)");
        }

        tokio::time::sleep(interval).await;
    }

    Ok(())
}

/// Stream inbound L3 APPLICATION / M64T frames (ROM **Listen** mode).
///
/// If `duration_secs > 0`, stops after that many seconds. If `duration_secs == 0` and `stop` is
/// `None`, runs until the WebSocket closes (CLI Ctrl+C). If `stop` is `Some`, also exits when the
/// flag becomes true (GUI stop button).
///
/// When `stop` is set, `read.next()` is wrapped in a short timeout so the task is not stuck
/// waiting for the next frame while the user clicks Stop (otherwise Stop would do nothing until
/// inbound traffic arrives).
pub async fn run_listen<F: FnMut(String) + Send>(
    url: &str,
    duration_secs: f64,
    stop: Option<Arc<AtomicBool>>,
    log: &mut F,
) -> Result<()> {
    const LISTEN_STOP_POLL: Duration = Duration::from_millis(100);
    /// Upper bound when `duration_secs == 0` but we still use `timeout` (with `stop` only).
    const LONG_WAIT: Duration = Duration::from_secs(86400 * 365);

    let (ws_stream, _) = connect_async(url)
        .await
        .with_context(|| format!("connect WebSocket {}", url))?;
    let (mut write, mut read) = ws_stream.split();

    ws_hello(&mut read, &mut write, log).await?;

    log_line!(log, "listening for L3 APPLICATION / M64T (B=STRESS_LARGE in M64T/BENCH; CTRL_POLL uses host REQ_CONTROLLER); Ctrl+C or Stop");
    let mut decoder = StreamDecoder::new();
    let end = if duration_secs > 0.0 {
        Some(Instant::now() + Duration::from_secs_f64(duration_secs))
    } else {
        None
    };
    loop {
        if let Some(s) = stop.as_ref() {
            if s.load(Ordering::SeqCst) {
                break;
            }
        }
        if let Some(t) = end {
            if Instant::now() >= t {
                break;
            }
        }

        let msg = if stop.is_none() && end.is_none() {
            read.next().await
        } else {
            let mut wait = match end {
                Some(t) => {
                    let rem = t.saturating_duration_since(Instant::now());
                    if rem.is_zero() {
                        break;
                    }
                    rem
                }
                None => LONG_WAIT,
            };
            if stop.is_some() {
                wait = wait.min(LISTEN_STOP_POLL);
            }
            match tokio::time::timeout(wait, read.next()).await {
                Ok(m) => m,
                Err(_) => continue,
            }
        };

        let msg = match msg {
            Some(Ok(m)) => m,
            Some(Err(e)) => return Err(e.into()),
            None => break,
        };
        match msg {
            Message::Binary(bin) => {
                decoder.push_bytes(&bin, |frame| emit_l3_frame(&frame, log));
            }
            Message::Text(t) => log_line!(log, "ws text: {}", t),
            Message::Close(_) => break,
            _ => {}
        }
    }
    log_line!(log, "OK: listen finished");
    Ok(())
}

/// WebSocket **binary** round-trip for **RAW_ECHO** test ROM mode via **multi64d** (cart path uses
/// L3 over the daemon — not direct serial). Requires **multi64d** running and ROM in **RAW_ECHO**.
pub async fn run_ws_raw_echo<F: FnMut(String) + Send>(
    url: &str,
    recv_timeout_secs: f64,
    payload: &[u8],
    json_ping: bool,
    log: &mut F,
) -> Result<()> {
    let (ws_stream, _) = connect_async(url)
        .await
        .with_context(|| format!("connect WebSocket {}", url))?;
    let (mut write, mut read) = ws_stream.split();
    let deadline = Duration::from_secs_f64(recv_timeout_secs.max(0.5));

    let first = tokio::time::timeout(deadline, read.next())
        .await
        .context("timeout waiting for hello")?
        .transpose()
        .context("ws error on hello")?;
    let Some(hello) = first else {
        anyhow::bail!("ws closed before hello");
    };
    let Message::Text(hello_s) = hello else {
        anyhow::bail!("expected text hello, got {:?}", hello);
    };
    let v: serde_json::Value = serde_json::from_str(&hello_s).context("parse hello json")?;
    if v.get("type").and_then(|x| x.as_str()) != Some("hello") {
        anyhow::bail!("unexpected hello: {}", hello_s);
    }
    log_line!(log, "hello: {}", hello_s.trim());

    if json_ping {
        write
            .send(Message::text(r#"{"type":"ping"}"#))
            .await
            .context("send json ping")?;
        let pong = tokio::time::timeout(deadline, read.next())
            .await
            .context("timeout waiting for pong")?
            .transpose()
            .context("ws error on pong")?;
        let Some(pong) = pong else {
            anyhow::bail!("ws closed before pong");
        };
        let Message::Text(pong_s) = pong else {
            anyhow::bail!("expected text pong, got {:?}", pong);
        };
        let pj: serde_json::Value = serde_json::from_str(&pong_s).context("parse pong")?;
        if pj.get("type").and_then(|x| x.as_str()) != Some("pong") {
            anyhow::bail!("unexpected pong: {}", pong_s);
        }
        log_line!(log, "pong ok");
    }

    write
        .send(Message::binary(payload.to_vec()))
        .await
        .context("send binary payload")?;
    log_line!(log, "sent binary: {} bytes", payload.len());

    loop {
        let next = tokio::time::timeout(deadline, read.next())
            .await
            .context("timeout waiting for binary response")?
            .transpose()
            .context("ws read error")?;
        let Some(msg) = next else {
            anyhow::bail!("ws closed before binary echo");
        };
        match msg {
            Message::Binary(bin) => {
                log_line!(log, "recv binary: {} bytes", bin.len());
                if bin == payload {
                    log_line!(log, "OK: echo matches payload (ROM RAW_ECHO)");
                } else {
                    anyhow::bail!(
                        "echo mismatch: sent {} bytes, got {} bytes",
                        payload.len(),
                        bin.len()
                    );
                }
                return Ok(());
            }
            Message::Text(t) => log_line!(log, "ws text (waiting for binary): {}", t),
            Message::Close(_) => anyhow::bail!("connection closed before binary echo"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod hex_tests {
    use super::*;

    /// #149: slicing by byte offset split a multi-byte character and panicked inside the GUI's
    /// `run_command` (and aborted the CLI) instead of reporting bad input.
    #[test]
    fn non_ascii_input_is_an_error_not_a_panic() {
        for s in ["0é0", "é0", "00é", "日本", "0０"] {
            assert!(parse_hex_body(s).is_err(), "{s:?}");
        }
        assert!(parse_hex_fixed8("0é00000000000000").is_err());
    }

    #[test]
    fn hex_with_spaces_and_either_case_still_parses() {
        assert_eq!(
            parse_hex_body(" de ad BE ef ").unwrap(),
            [0xde, 0xad, 0xbe, 0xef]
        );
        assert!(parse_hex_body("").unwrap().is_empty());
        assert!(parse_hex_body("abc").is_err());
        assert!(parse_hex_body("0g").is_err());
        assert!(
            parse_hex_body("+1").is_err(),
            "from_str_radix accepts a sign"
        );
    }
    /// A DIAG body as the ROM builds it (spec §11), so the offsets are asserted end to end.
    fn diag_body(version: u8, extra: &[(usize, u32)]) -> Vec<u8> {
        let len = if version >= 2 {
            DiagSnapshot::BODY_LEN_V2
        } else {
            DiagSnapshot::BODY_LEN_V1
        };
        let mut b = vec![0u8; len];
        b[0] = version;
        b[1] = 2; // BENCH
        b[2] = 1; // SummerCart64
        for (at, v) in extra {
            b[*at..*at + 4].copy_from_slice(&v.to_be_bytes());
        }
        b
    }

    #[test]
    fn a_diag_body_is_parsed_at_the_offsets_the_spec_gives() {
        let body = diag_body(
            1,
            &[
                (4, 11),
                (8, 0),
                (12, 0),
                (16, 0),
                (20, 4096),
                (24, 0),
                (28, 0x8034_5678),
                (32, 256),
            ],
        );
        let d = DiagSnapshot::parse(&body).unwrap();
        assert_eq!(d.mode, 2);
        assert_eq!(d.cart_kind, 1);
        assert_eq!(d.frames_handled, 11);
        assert_eq!(d.rx_bytes, 4096);
        assert_eq!(d.scratch_addr, 0x8034_5678);
        assert_eq!(d.scratch_len, 256);
        assert!(d.is_clean());
    }

    /// The counters are the whole point of DIAG: a reply proves the round trip, these prove the
    /// stream underneath it never desynchronized. Any one of them is enough to fail a run.
    #[test]
    fn any_stream_health_counter_makes_a_snapshot_unclean() {
        for at in [8usize, 12, 16] {
            let d = DiagSnapshot::parse(&diag_body(1, &[(at, 1)])).unwrap();
            assert!(
                !d.is_clean(),
                "offset {at} should make the snapshot unclean"
            );
        }
    }

    /// A newer ROM may add fields; reading them at these offsets would report confident nonsense,
    /// so an unknown version is refused rather than parsed.
    #[test]
    fn an_unknown_diag_body_version_is_refused() {
        let e = DiagSnapshot::parse(&diag_body(3, &[]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("version 3"), "{e}");
    }

    #[test]
    fn a_truncated_diag_body_is_refused() {
        let short = diag_body(1, &[])[..DiagSnapshot::BODY_LEN_V1 - 1].to_vec();
        assert!(DiagSnapshot::parse(&short).is_err());
        assert!(DiagSnapshot::parse(&[]).is_err());
        // A version-2 body cut back to version 1's length is still short: the version byte, not
        // the length, says which fields are there.
        let short = diag_body(2, &[])[..DiagSnapshot::BODY_LEN_V1].to_vec();
        assert!(DiagSnapshot::parse(&short).is_err());
    }

    /// Version 2 appends the count of cart writes that gave up part-way, and shows it in the
    /// summary line the suite reads.
    #[test]
    fn a_version_2_diag_body_carries_the_cart_write_failures() {
        let d = DiagSnapshot::parse(&diag_body(2, &[(20, 4096), (36, 3)])).unwrap();
        assert_eq!(
            d.rx_bytes, 4096,
            "the version-1 fields stay where they were"
        );
        assert_eq!(d.tx_failures, Some(3));
        assert!(d.summary().ends_with(" tx_failures=3"), "{}", d.summary());
    }

    #[test]
    fn a_version_1_diag_body_has_no_write_failure_count() {
        let d = DiagSnapshot::parse(&diag_body(1, &[])).unwrap();
        assert_eq!(d.tx_failures, None);
        assert!(!d.summary().contains("tx_failures"), "{}", d.summary());
    }

    #[test]
    fn a_peekv_body_lays_out_rid_count_then_each_region() {
        let b = m64p_peek_body(1, &[(0x8000_0040, 16)]);
        assert_eq!(
            b,
            vec![0x00, 0x01, 0x01, 0x80, 0x00, 0x00, 0x40, 0x00, 0x10]
        );
    }

    #[test]
    fn a_pokev_body_carries_each_regions_bytes_after_its_header() {
        let b = m64p_poke_body(2, &[(0x8000_0010, &[0xAA, 0xBB])]);
        assert_eq!(
            b,
            vec![0x00, 0x02, 0x01, 0x80, 0x00, 0x00, 0x10, 0x00, 0x02, 0xAA, 0xBB]
        );
    }

    #[test]
    fn the_first_region_of_a_peekv_response_is_its_declared_bytes() {
        // rid, n=1, len=3, then the bytes.
        let body = vec![0x00, 0x01, 0x01, 0x00, 0x03, 1, 2, 3];
        assert_eq!(m64p_first_region(&body).unwrap(), vec![1, 2, 3]);
    }

    /// A response claiming more than it carries must be an error, not a short read that a caller
    /// would compare against a shorter pattern and pass.
    #[test]
    fn a_peekv_response_shorter_than_it_declares_is_refused() {
        let body = vec![0x00, 0x01, 0x01, 0x00, 0x08, 1, 2, 3];
        assert!(m64p_first_region(&body).is_err());
        assert!(
            m64p_first_region(&[0x00, 0x01, 0x00]).is_err(),
            "no regions"
        );
        assert!(m64p_first_region(&[0x00]).is_err(), "truncated");
    }

    #[test]
    fn mode_and_cart_names_cover_every_value_the_rom_can_report() {
        assert_eq!(mode_name(0), "RAW_ECHO");
        assert_eq!(mode_name(4), "MEM_AGENT");
        assert_eq!(mode_name(5), "unknown");
        assert_eq!(cart_kind_name(1), "SummerCart64");
        assert_eq!(cart_kind_name(9), "unknown");
    }
}
