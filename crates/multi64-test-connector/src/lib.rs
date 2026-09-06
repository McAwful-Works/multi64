//! Library for **`multi64_test.z64`** over **`multi64d`** WebSocket (L3 APPLICATION / **M64T**).
//! See [`docs/connectors/test-rom.md`](../../docs/connectors/test-rom.md).

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
async fn recv_until_m64t_any<F: FnMut(String) + Send>(
    read: &mut WsRead,
    decoder: &mut StreamDecoder,
    deadline: Instant,
    want_msgs: &[u8],
    log: &mut F,
) -> Result<bool> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let next = tokio::time::timeout(remaining, read.next()).await;
        let msg = match next {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => return Err(e.into()),
            Ok(None) => return Ok(false),
            Err(_) => return Ok(false),
        };
        match msg {
            Message::Binary(bin) => {
                let mut got = false;
                decoder.push_bytes(&bin, |frame| {
                    emit_l3_frame(&frame, log);
                    if frame.ty == FrameType::Data && frame.channel == Channel::Application {
                        let p = &frame.payload;
                        if p.len() >= 5 && p[0..4] == M64T_MAGIC && want_msgs.contains(&p[4]) {
                            got = true;
                        }
                    }
                });
                if got {
                    return Ok(true);
                }
            }
            Message::Text(t) => log_line!(log, "ws text: {}", t),
            Message::Close(_) => return Ok(false),
            _ => {}
        }
    }
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
