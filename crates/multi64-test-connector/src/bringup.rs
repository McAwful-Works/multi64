//! Host half of the cart bring-up ROM, `n64/bringup/multi64_bringup.z64`.
//!
//! The ROM runs the cart agent's own drivers and keeps what it measured in a report in RDRAM
//! ([`docs/spec/cart-bringup-report-v0.md`](../../docs/spec/cart-bringup-report-v0.md)). This talks
//! to the ROM through the agent build it is running, over `multi64d`'s WebSocket and M64P: it finds
//! the report, reads it, drives echo, `PEEKROM` and largest-response traffic through the link under
//! each load level (and through both X7 builds), and saves the lot as JSON. Given an earlier run as a
//! baseline, it lists every check whose outcome differs.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use multi64_l3::StreamDecoder;
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::{
    build_m64p_payload, l3_data_application, m64p_err_name, recv_app_body, ws_hello, WsRead,
    WsWrite, M64P_MAGIC, M64P_MSG_ERR, M64P_MSG_HELLO, M64P_MSG_HELLO_ACK, M64P_MSG_PEEKROM,
    M64P_MSG_PEEKROM_RESP, M64P_MSG_PEEKV, M64P_MSG_PEEKV_RESP, M64P_MSG_POKEV, M64P_MSG_POKE_ACK,
};

/// Version of the JSON this writes. Bump it when a field changes meaning.
pub const RUN_FORMAT: u32 = 0;

// ---- report layout (cart-bringup-report-v0.md section 3) -------------------------------------

/// `"CARTBRUP"`.
pub const REPORT_MAGIC: [u8; 8] = *b"CARTBRUP";
/// Report format this reads.
pub const REPORT_FORMAT: u32 = 0;
/// Bytes from the start of the report to the echo area: everything the ROM writes.
pub const REPORT_HEAD: usize = 0x388;
/// Host scratch at the end of the report.
pub const ECHO_BYTES: usize = 4096;
/// Where the report may be: 16-byte aligned, inside this RDRAM range.
pub const SCAN_START: u32 = 0x400;
pub const SCAN_END: u32 = 0x10_0000;

const OFF_CTL_LOAD: u32 = 0x020;
/// A control value the ROM ignores (no such load level or variant), written to re-arm a word.
const CTL_IGNORED: u32 = u32::MAX;
const OFF_CTL_VARIANT: u32 = 0x024;
const OFF_TIMING: usize = 0x0C0;
const OFF_BUFFER: usize = 0x148;
const OFF_LINK: usize = 0x208;
const TIMING_SLOTS: usize = 4;
const BUF_COMBOS: usize = 4;
const VARIANTS: usize = 4;
const LINK_WORDS: usize = 24;

/// M64P section 4 limits.
const MAX_REGION: usize = 4096;
const MAX_TOTAL: usize = 7936;

pub const CART_SC64: u32 = 1;
pub const CART_X_SERIES: u32 = 2;
pub const CART_PRO: u32 = 3;

pub const VARIANT_X7_IO: u32 = 2;
pub const VARIANT_X7_DMA: u32 = 3;

pub const LOAD_OFF: u32 = 0;
pub const LOAD_MODERATE: u32 = 1;
pub const LOAD_HEAVY: u32 = 2;

pub fn cart_name(cart: u32) -> &'static str {
    match cart {
        0 => "none",
        CART_SC64 => "SummerCart64",
        CART_X_SERIES => "EverDrive X-series (X7 or 3.0)",
        CART_PRO => "EverDrive-64 PRO",
        4 => "64drive (no agent driver)",
        _ => "unknown",
    }
}

pub fn variant_name(variant: u32) -> &'static str {
    match variant {
        0 => "none",
        1 => "SC64",
        VARIANT_X7_IO => "X7, CPU words",
        VARIANT_X7_DMA => "X7, PI DMA",
        4 => "PRO",
        _ => "unknown",
    }
}

pub fn load_name(load: u32) -> &'static str {
    match load {
        LOAD_OFF => "off",
        LOAD_MODERATE => "moderate",
        LOAD_HEAVY => "heavy",
        _ => "unknown",
    }
}

fn timing_name(id: u32) -> &'static str {
    match id {
        1 => "PI_STATUS",
        2 => "X7 USBCFG",
        3 => "SC64 SR_CMD",
        4 => "PRO SYSSTAT",
        5 => "PRO FIFOSTAT",
        _ => "unknown",
    }
}

fn buffer_result_name(result: u32) -> &'static str {
    match result {
        0 => "not run",
        1 => "match",
        2 => "mismatch",
        3 => "write reported the PI busy",
        4 => "read reported the PI busy",
        5 => "not applicable",
        _ => "unknown",
    }
}

const COMBO_NAMES: [&str; BUF_COMBOS] = ["IO>IO", "IO>DMA", "DMA>IO", "DMA>DMA"];

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// One register's read cost and the longest a driver wait polling it can last.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Timing {
    pub id: u32,
    pub name: String,
    pub spins: u32,
    pub ticks_total: u32,
    pub ticks_max: u32,
    pub ns_per_read: f64,
    pub wait_limit_ms: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BufferTest {
    pub combo: String,
    pub result: u32,
    pub result_name: String,
    pub first_bad: Option<u32>,
    pub bad_bytes: u32,
    pub ticks_write: u32,
    pub ticks_read: u32,
}

/// One agent build's counters (`struct link_stats`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LinkStats {
    pub init_calls: u32,
    pub init_ok: u32,
    pub recv_calls: u32,
    pub recv_data: u32,
    pub recv_bytes: u32,
    pub recv_lost: u32,
    pub read_calls: u32,
    pub read_failed: u32,
    pub send_calls: u32,
    pub send_ok: u32,
    pub send_bytes: u32,
    pub pio_calls: u32,
    pub pio_failed: u32,
    pub recv_ticks_max: u32,
    pub send_ticks_max: u32,
    pub tick_ticks_max: u32,
    pub tick_ticks_total: u32,
    pub agent_ticks: u32,
    pub agent_frames: u32,
    pub agent_ready: u32,
    pub m64p_requests: u32,
    pub m64p_errors: u32,
    pub m64p_last_error: u32,
}

impl LinkStats {
    fn parse(b: &[u8]) -> Self {
        let w = |i: usize| be32(b, i * 4);
        LinkStats {
            init_calls: w(0),
            init_ok: w(1),
            recv_calls: w(2),
            recv_data: w(3),
            recv_bytes: w(4),
            recv_lost: w(5),
            read_calls: w(6),
            read_failed: w(7),
            send_calls: w(8),
            send_ok: w(9),
            send_bytes: w(10),
            pio_calls: w(11),
            pio_failed: w(12),
            recv_ticks_max: w(13),
            send_ticks_max: w(14),
            tick_ticks_max: w(15),
            tick_ticks_total: w(16),
            agent_ticks: w(17),
            agent_frames: w(18),
            agent_ready: w(19),
            m64p_requests: w(20),
            m64p_errors: w(21),
            m64p_last_error: w(22),
        }
    }
}

/// The decoded report.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    /// RDRAM physical address it was read from.
    pub addr: u32,
    pub format: u32,
    pub size: u32,
    pub rom_version: String,
    pub frame: u32,
    pub count_hz: u32,
    pub ctl_load: u32,
    pub ctl_variant: u32,
    pub ctl_rerun: u32,
    pub cart: u32,
    pub cart_name: String,
    pub variant: u32,
    pub variant_name: String,
    pub load: u32,
    pub rerun_done: u32,
    pub stages: u32,
    pub mem_size: u32,
    pub probe_ok: u32,
    pub d64_magic: u32,
    pub sc64_ident_locked: u32,
    pub ed_reg14_locked: u32,
    pub ed_reg04_locked: u32,
    pub pro_sysstat: [u32; 2],
    pub init_tried: u32,
    pub init_ok: u32,
    pub ed_reg14_unlocked: u32,
    pub ed_usbcfg_after: u32,
    pub sc64_ident_unlocked: u32,
    /// The first 64 bytes of cartridge ROM, hex.
    pub rom_header: String,
    /// `[0]` with no load, `[1]` under `cond_load`.
    pub timing: Vec<Vec<Timing>>,
    pub buf_addr: u32,
    pub cond_load: u32,
    pub buffer: Vec<Vec<BufferTest>>,
    pub load_bytes: u32,
    pub load_period_ticks: u32,
    pub load_started: u32,
    pub load_skipped: u32,
    pub irq_gap_max: u32,
    /// Index variant id - 1.
    pub link: Vec<LinkStats>,
}

impl Report {
    /// Decode the first [`REPORT_HEAD`] bytes of a report found at `addr`.
    pub fn parse(addr: u32, b: &[u8]) -> Result<Self> {
        if b.len() < REPORT_HEAD {
            anyhow::bail!(
                "report is {} bytes, expected at least {REPORT_HEAD}",
                b.len()
            );
        }
        if b[0..8] != REPORT_MAGIC {
            anyhow::bail!("no report magic at {addr:#x}");
        }
        let w = |off: usize| be32(b, off);
        let format = w(0x08);
        if format != REPORT_FORMAT {
            anyhow::bail!("report format {format}, this tool reads {REPORT_FORMAT}");
        }
        let count_hz = w(0x1C).max(1);
        let mut timing = Vec::new();
        for slot in 0..2 {
            let mut v = Vec::new();
            for i in 0..TIMING_SLOTS {
                let o = OFF_TIMING + (slot * TIMING_SLOTS + i) * 16;
                let id = w(o);
                if id == 0 {
                    continue;
                }
                let spins = w(o + 4);
                let ticks_total = w(o + 8);
                let ns_per_read = ticks_total as f64 * 1e9 / (1024.0 * count_hz as f64);
                v.push(Timing {
                    id,
                    name: timing_name(id).into(),
                    spins,
                    ticks_total,
                    ticks_max: w(o + 12),
                    ns_per_read,
                    wait_limit_ms: ns_per_read * spins as f64 / 1e6,
                });
            }
            timing.push(v);
        }
        let mut buffer = Vec::new();
        for slot in 0..2 {
            let mut v = Vec::new();
            for (c, name) in COMBO_NAMES.iter().enumerate() {
                let o = OFF_BUFFER + (slot * BUF_COMBOS + c) * 20;
                let result = w(o);
                let first_bad = w(o + 4);
                v.push(BufferTest {
                    combo: (*name).into(),
                    result,
                    result_name: buffer_result_name(result).into(),
                    first_bad: (first_bad != u32::MAX).then_some(first_bad),
                    bad_bytes: w(o + 8),
                    ticks_write: w(o + 12),
                    ticks_read: w(o + 16),
                });
            }
            buffer.push(v);
        }
        let link = (0..VARIANTS)
            .map(|i| LinkStats::parse(&b[OFF_LINK + i * LINK_WORDS * 4..]))
            .collect();
        let rom_version = w(0x14);
        Ok(Report {
            addr,
            format,
            size: w(0x0C),
            rom_version: format!("{}.{}", rom_version >> 16, rom_version & 0xFFFF),
            frame: w(0x18),
            count_hz,
            ctl_load: w(0x20),
            ctl_variant: w(0x24),
            ctl_rerun: w(0x28),
            cart: w(0x30),
            cart_name: cart_name(w(0x30)).into(),
            variant: w(0x34),
            variant_name: variant_name(w(0x34)).into(),
            load: w(0x38),
            rerun_done: w(0x3C),
            stages: w(0x40),
            mem_size: w(0x44),
            probe_ok: w(0x50),
            d64_magic: w(0x54),
            sc64_ident_locked: w(0x58),
            ed_reg14_locked: w(0x5C),
            ed_reg04_locked: w(0x60),
            pro_sysstat: [w(0x64), w(0x68)],
            init_tried: w(0x6C),
            init_ok: w(0x70),
            ed_reg14_unlocked: w(0x74),
            ed_usbcfg_after: w(0x78),
            sc64_ident_unlocked: w(0x7C),
            rom_header: hex(&b[0x80..0xC0]),
            timing,
            buf_addr: w(0x140),
            cond_load: w(0x144),
            buffer,
            load_bytes: w(0x1E8),
            load_period_ticks: w(0x1EC),
            load_started: w(0x1F0),
            load_skipped: w(0x1F4),
            irq_gap_max: w(0x1F8),
            link,
        })
    }

    /// The counters of the variant driving the link now.
    pub fn current_link(&self) -> Option<&LinkStats> {
        self.variant
            .checked_sub(1)
            .and_then(|i| self.link.get(i as usize))
    }

    fn us(&self, ticks: u32) -> f64 {
        ticks as f64 * 1e6 / self.count_hz as f64
    }
}

// ---- checks ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
    Skip,
    /// A measurement, not a verdict.
    Info,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Skip => "SKIP",
            Outcome::Info => "INFO",
        }
    }
}

/// One named result. Names do not mention the cart, so a run on one cart compares with a run on
/// another check by check.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Check {
    pub name: String,
    pub outcome: Outcome,
    pub detail: String,
    /// A number worth comparing between runs, such as a read cost in ns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

impl Check {
    fn new(name: impl Into<String>, outcome: Outcome, detail: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            outcome,
            detail: detail.into(),
            value: None,
        }
    }

    fn with_value(mut self, v: f64) -> Self {
        self.value = Some(v);
        self
    }
}

/// What the ROM found by itself, before any traffic: stages 1, 2 and 4's offline half.
pub fn report_checks(r: &Report) -> Vec<Check> {
    let mut out = Vec::new();
    let found = matches!(r.cart, CART_SC64 | CART_X_SERIES | CART_PRO);
    out.push(Check::new(
        "identify.cart",
        if found { Outcome::Pass } else { Outcome::Fail },
        format!(
            "{}; X-series VERSION/PRO EDID {:#010x} -> {:#010x}, USBCFG {:#010x} -> {:#010x}, SC64 IDENT {:#010x} -> {:#010x}, 64drive {:#010x}",
            r.cart_name,
            r.ed_reg14_locked,
            r.ed_reg14_unlocked,
            r.ed_reg04_locked,
            r.ed_usbcfg_after,
            r.sc64_ident_locked,
            r.sc64_ident_unlocked,
            r.d64_magic
        ),
    ));
    let default = match r.cart {
        CART_SC64 => Some(1u32),
        CART_X_SERIES => Some(VARIANT_X7_IO),
        CART_PRO => Some(4),
        _ => None,
    };
    if let Some(v) = default {
        let ok = r.init_ok & (1 << v) != 0;
        out.push(Check::new(
            "identify.driver_init",
            if ok { Outcome::Pass } else { Outcome::Fail },
            format!(
                "{} driver init {} (tried mask {:#x}, answered mask {:#x})",
                variant_name(v),
                if ok { "answered" } else { "did not answer" },
                r.init_tried,
                r.init_ok
            ),
        ));
    }

    for (slot, label) in [(0usize, "no_load"), (1, "under_load")] {
        for t in &r.timing[slot] {
            // By role, not register, so an SC64 baseline lines up with an X7 run.
            let which = match t.id {
                1 => "pi_status",
                5 => "cart_fifo",
                _ => "cart_status",
            };
            out.push(
                Check::new(
                    format!("blocks.timing.{which}.{label}"),
                    Outcome::Info,
                    format!(
                        "{}: {:.0} ns per read, slowest {:.1} us; {} spins = {:.1} ms",
                        t.name,
                        t.ns_per_read,
                        r.us(t.ticks_max),
                        t.spins,
                        t.wait_limit_ms
                    ),
                )
                .with_value(t.ns_per_read),
            );
        }
        for b in &r.buffer[slot] {
            let outcome = match b.result {
                1 => Outcome::Pass,
                0 | 5 => Outcome::Skip,
                _ => Outcome::Fail,
            };
            let mut detail = format!("{} at {:#010x}", b.result_name, r.buf_addr);
            if let Some(off) = b.first_bad {
                detail += &format!(", first bad byte {off}, {} bytes differ", b.bad_bytes);
            }
            if b.result == 1 {
                detail += &format!(
                    ", write {:.0} us, read {:.0} us",
                    r.us(b.ticks_write),
                    r.us(b.ticks_read)
                );
            }
            out.push(Check::new(
                format!("blocks.buffer.{}.{label}", b.combo),
                outcome,
                detail,
            ));
        }
    }
    out
}

// ---- the link -------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HelloAck {
    pub proto: u8,
    pub agent_ver: u16,
    pub rdram_bytes: u32,
    pub flags: u8,
    pub rtt_ms: f64,
}

enum Reply {
    Ok(Vec<u8>),
    /// `ERR` with this code.
    Err(u8),
    Timeout,
}

/// One WebSocket to `multi64d`, with a decoder that outlives a request, so a reply that arrives
/// late is matched by `rid` and dropped rather than taken for the next request's.
struct Link {
    write: WsWrite,
    read: WsRead,
    decoder: StreamDecoder,
    rid: u16,
    timeout: Duration,
}

impl Link {
    async fn connect<F: FnMut(String) + Send>(
        url: &str,
        timeout: Duration,
        log: &mut F,
    ) -> Result<Self> {
        let (ws, _) = connect_async(url)
            .await
            .with_context(|| format!("connect WebSocket {url}"))?;
        let (mut write, mut read) = ws.split();
        ws_hello(&mut read, &mut write, log).await?;
        Ok(Link {
            write,
            read,
            decoder: StreamDecoder::new(),
            rid: 0,
            timeout,
        })
    }

    async fn send(&mut self, payload: Vec<u8>) -> Result<()> {
        let wire = l3_data_application(payload)?;
        self.write
            .send(Message::binary(wire))
            .await
            .context("send to multi64d")
    }

    async fn hello(&mut self) -> Result<Option<HelloAck>> {
        let t0 = Instant::now();
        self.send(build_m64p_payload(M64P_MSG_HELLO, &[])).await?;
        let deadline = t0 + self.timeout;
        let mut quiet = |_: String| {};
        loop {
            match recv_app_body(
                &mut self.read,
                &mut self.decoder,
                deadline,
                M64P_MAGIC,
                &[M64P_MSG_HELLO_ACK],
                &mut quiet,
            )
            .await?
            {
                None => return Ok(None),
                Some((_, body)) if body.len() >= 8 => {
                    return Ok(Some(HelloAck {
                        proto: body[0],
                        agent_ver: u16::from_be_bytes([body[1], body[2]]),
                        rdram_bytes: be32(&body, 3),
                        flags: body[7],
                        rtt_ms: t0.elapsed().as_secs_f64() * 1e3,
                    }))
                }
                Some(_) => continue,
            }
        }
    }

    /// HELLO, retried for `wait`: an agent build just selected has to come up first.
    async fn hello_within(&mut self, wait: Duration) -> Result<Option<HelloAck>> {
        let end = Instant::now() + wait;
        loop {
            if let Some(h) = self.hello().await? {
                return Ok(Some(h));
            }
            if Instant::now() >= end {
                return Ok(None);
            }
        }
    }

    /// One request carrying a `rid`, waiting for the reply with that `rid`.
    async fn request(&mut self, msg: u8, body_after_rid: &[u8], want: u8) -> Result<(Reply, f64)> {
        self.rid = self.rid.wrapping_add(1).max(1);
        let rid = self.rid;
        let mut body = rid.to_be_bytes().to_vec();
        body.extend_from_slice(body_after_rid);
        let t0 = Instant::now();
        self.send(build_m64p_payload(msg, &body)).await?;
        let deadline = t0 + self.timeout;
        let mut quiet = |_: String| {};
        loop {
            let got = recv_app_body(
                &mut self.read,
                &mut self.decoder,
                deadline,
                M64P_MAGIC,
                &[want, M64P_MSG_ERR],
                &mut quiet,
            )
            .await?;
            let rtt = t0.elapsed().as_secs_f64() * 1e3;
            match got {
                None => return Ok((Reply::Timeout, rtt)),
                Some((m, b)) if b.len() >= 2 && u16::from_be_bytes([b[0], b[1]]) == rid => {
                    if m == M64P_MSG_ERR {
                        return Ok((Reply::Err(b.get(2).copied().unwrap_or(0)), rtt));
                    }
                    return Ok((Reply::Ok(b), rtt));
                }
                // Another request's reply, arriving late: not this one.
                Some(_) => continue,
            }
        }
    }

    /// PEEKV or PEEKROM of several regions; each region's bytes, in order.
    async fn peek(
        &mut self,
        rom: bool,
        regions: &[(u32, u16)],
    ) -> Result<(Result<Vec<Vec<u8>>, String>, f64)> {
        let mut req = vec![regions.len() as u8];
        for (addr, len) in regions {
            req.extend_from_slice(&addr.to_be_bytes());
            req.extend_from_slice(&len.to_be_bytes());
        }
        let (msg, want) = if rom {
            (M64P_MSG_PEEKROM, M64P_MSG_PEEKROM_RESP)
        } else {
            (M64P_MSG_PEEKV, M64P_MSG_PEEKV_RESP)
        };
        let (reply, rtt) = self.request(msg, &req, want).await?;
        let body = match reply {
            Reply::Timeout => return Ok((Err("timeout".into()), rtt)),
            Reply::Err(c) => return Ok((Err(format!("ERR {} ({c:#04x})", m64p_err_name(c))), rtt)),
            Reply::Ok(b) => b,
        };
        Ok((parse_regions(&body, regions), rtt))
    }

    async fn poke(&mut self, addr: u32, data: &[u8]) -> Result<(Result<(), String>, f64)> {
        let mut req = vec![1u8];
        req.extend_from_slice(&addr.to_be_bytes());
        req.extend_from_slice(&(data.len() as u16).to_be_bytes());
        req.extend_from_slice(data);
        let (reply, rtt) = self
            .request(M64P_MSG_POKEV, &req, M64P_MSG_POKE_ACK)
            .await?;
        Ok((
            match reply {
                Reply::Timeout => Err("timeout".into()),
                Reply::Err(c) => Err(format!("ERR {} ({c:#04x})", m64p_err_name(c))),
                Reply::Ok(b) if b.get(2) == Some(&1) => Ok(()),
                Reply::Ok(b) => Err(format!("applied {:?} regions, expected 1", b.get(2))),
            },
            rtt,
        ))
    }

    async fn read_report(&mut self, addr: u32) -> Result<Report> {
        let (got, _) = self.peek(false, &[(addr, REPORT_HEAD as u16)]).await?;
        let regions = got.map_err(|e| anyhow::anyhow!("reading the report: {e}"))?;
        Report::parse(addr, &regions[0])
    }

    async fn set_word(&mut self, addr: u32, value: u32) -> Result<()> {
        let (got, _) = self.poke(addr, &value.to_be_bytes()).await?;
        got.map_err(|e| anyhow::anyhow!("writing {addr:#x}: {e}"))
    }

    /// Write a control word so the ROM acts on it whatever it held before (spec section 5). The
    /// ROM acts on a change, so writing the value already there would do nothing; a value it
    /// ignores goes first. Each write is its own round trip, so the ROM sees both: the agent
    /// serves one request per frame, and the ROM reads its controls between frames.
    async fn set_control(&mut self, addr: u32, value: u32) -> Result<()> {
        self.set_word(addr, CTL_IGNORED).await?;
        self.set_word(addr, value).await
    }
}

/// The region bytes out of a `PEEKV_RESP`/`PEEKROM_RESP` body (`rid`, `n`, then `len` + bytes each).
fn parse_regions(body: &[u8], asked: &[(u32, u16)]) -> Result<Vec<Vec<u8>>, String> {
    if body.len() < 3 || body[2] as usize != asked.len() {
        return Err(format!(
            "reply carries {:?} regions, asked for {}",
            body.get(2),
            asked.len()
        ));
    }
    let mut at = 3;
    let mut out = Vec::new();
    for (_, want) in asked {
        if body.len() < at + 2 {
            return Err("reply truncated".into());
        }
        let len = u16::from_be_bytes([body[at], body[at + 1]]) as usize;
        at += 2;
        if len != *want as usize || body.len() < at + len {
            return Err(format!("region of {len} bytes, asked for {want}"));
        }
        out.push(body[at..at + len].to_vec());
        at += len;
    }
    Ok(out)
}

/// Read RDRAM from [`SCAN_START`] until the report turns up: its magic at a 16-byte boundary, with
/// `self` naming that same address. A copy elsewhere (the agent's transmit buffer holds one after the
/// first read) carries the wrong address and is passed over.
async fn find_report(link: &mut Link) -> Result<Option<u32>> {
    let mut addr = SCAN_START;
    while addr < SCAN_END {
        let first = MAX_REGION.min((SCAN_END - addr) as usize);
        let second = (MAX_TOTAL - first).min((SCAN_END - addr) as usize - first);
        let mut regions = vec![(addr, first as u16)];
        if second > 0 {
            regions.push((addr + first as u32, second as u16));
        }
        let (got, _) = link.peek(false, &regions).await?;
        let chunk: Vec<u8> = got
            .map_err(|e| anyhow::anyhow!("scanning RDRAM at {addr:#x}: {e}"))?
            .concat();
        for off in (0..chunk.len().saturating_sub(20)).step_by(16) {
            if chunk[off..off + 8] == REPORT_MAGIC {
                let at = addr + off as u32;
                if be32(&chunk, off + 0x10) == at {
                    return Ok(Some(at));
                }
            }
        }
        if addr + chunk.len() as u32 >= SCAN_END {
            break;
        }
        // Overlap the next read by 32 bytes, so a report whose header straddles the boundary is
        // still seen whole once.
        addr += chunk.len() as u32 - 32;
    }
    Ok(None)
}

// ---- a run ----------------------------------------------------------------------------------

/// One variant under one load: what the host saw, and the report afterward.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Phase {
    pub variant: u32,
    pub variant_name: String,
    pub load: u32,
    pub load_name: String,
    pub requests: u32,
    pub timeouts: u32,
    pub busy: u32,
    pub wrong: u32,
    pub rtt_ms_min: f64,
    pub rtt_ms_avg: f64,
    pub rtt_ms_max: f64,
    /// The variant's counters after the phase, minus before.
    pub delta: LinkStats,
    pub after: Option<Report>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BringupRun {
    pub format: u32,
    pub tool: String,
    pub started_unix: u64,
    pub daemon: Option<super::suite::DaemonInfo>,
    pub hello: Option<HelloAck>,
    pub report_addr: Option<u32>,
    pub boot_report: Option<Report>,
    pub phases: Vec<Phase>,
    pub final_report: Option<Report>,
    pub checks: Vec<Check>,
    pub notes: Vec<String>,
}

impl BringupRun {
    pub fn failed(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.outcome == Outcome::Fail)
            .count()
    }
}

pub struct BringupOptions {
    pub ws_url: String,
    pub base_url: String,
    pub recv_timeout_secs: f64,
    /// Echo rounds per phase.
    pub rounds: u32,
    /// Only the cart's default agent build, even on an X7.
    pub default_variant_only: bool,
}

/// Echo sizes per round: one word, a USB packet, an X7 window, and up to the echo area.
const ECHO_SIZES: [usize; 5] = [4, 64, 512, 2048, ECHO_BYTES];
/// Consecutive timeouts after which a phase stops: the link is not coming back by itself.
const GIVE_UP_TIMEOUTS: u32 = 3;

fn pattern(seed: u32, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (seed.wrapping_mul(31).wrapping_add(i as u32 * 7) ^ (i as u32 >> 8)) as u8)
        .collect()
}

struct Tally {
    requests: u32,
    timeouts: u32,
    consecutive: u32,
    busy: u32,
    wrong: u32,
    rtts: Vec<f64>,
    notes: Vec<String>,
}

impl Tally {
    fn new() -> Self {
        Tally {
            requests: 0,
            timeouts: 0,
            consecutive: 0,
            busy: 0,
            wrong: 0,
            rtts: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Record one request. Returns false once the phase should stop.
    fn note(&mut self, what: &str, result: &Result<(), String>, rtt: f64) -> bool {
        self.requests += 1;
        match result {
            Ok(()) => {
                self.consecutive = 0;
                self.rtts.push(rtt);
            }
            Err(e) if e == "timeout" => {
                self.timeouts += 1;
                self.consecutive += 1;
            }
            Err(e) if e.contains("busy") => {
                self.consecutive = 0;
                self.busy += 1;
            }
            Err(e) => {
                self.consecutive = 0;
                self.wrong += 1;
                if self.notes.len() < 8 {
                    self.notes.push(format!("{what}: {e}"));
                }
            }
        }
        self.consecutive < GIVE_UP_TIMEOUTS
    }
}

async fn phase_traffic(
    link: &mut Link,
    report_addr: u32,
    rom_header: Option<&[u8]>,
    rounds: u32,
    seed: u32,
) -> Result<Tally> {
    let echo = report_addr + REPORT_HEAD as u32;
    let mut t = Tally::new();
    'rounds: for round in 0..rounds {
        let mut last = Vec::new();
        for (k, &size) in ECHO_SIZES.iter().enumerate() {
            let data = pattern(seed.wrapping_add(round * 16 + k as u32), size);
            let (poked, rtt) = link.poke(echo, &data).await?;
            if !t.note(&format!("POKEV {size} B"), &poked, rtt) {
                break 'rounds;
            }
            if poked.is_err() {
                continue;
            }
            let (got, rtt) = link.peek(false, &[(echo, size as u16)]).await?;
            let checked = got.and_then(|r| {
                if r[0] == data {
                    Ok(())
                } else {
                    let first = r[0].iter().zip(&data).position(|(a, b)| a != b);
                    Err(format!("echo of {size} B differs, first at {first:?}"))
                }
            });
            if !t.note(&format!("PEEKV {size} B"), &checked, rtt) {
                break 'rounds;
            }
            if size == ECHO_BYTES && checked.is_ok() {
                last = data;
            }
        }

        // PEEKROM goes through pi_io_load_words, so the DMA build reads the ROM by DMA. Only asked
        // of an agent whose HELLO_ACK says it answers PEEKROM.
        if let Some(header) = rom_header {
            let (got, rtt) = link.peek(true, &[(0, 64)]).await?;
            let checked = got.and_then(|r| {
                if r[0] == header {
                    Ok(())
                } else {
                    Err("PEEKROM header differs from the report's".into())
                }
            });
            if !t.note("PEEKROM 64 B", &checked, rtt) {
                break;
            }
        }

        // The largest response M64P allows, which an X7 sends as 16 USB windows.
        if !last.is_empty() {
            let rest = (MAX_TOTAL - ECHO_BYTES - REPORT_HEAD) as u16;
            let regions = [
                (echo, ECHO_BYTES as u16),
                (report_addr, REPORT_HEAD as u16),
                (SCAN_START, rest),
            ];
            let (got, rtt) = link.peek(false, &regions).await?;
            let checked = got.and_then(|r| {
                if r[0] != last {
                    Err("largest response: echo area differs".into())
                } else if r[1][0..8] != REPORT_MAGIC {
                    Err("largest response: report magic missing".into())
                } else {
                    Ok(())
                }
            });
            if !t.note("PEEKV 7936 B", &checked, rtt) {
                break;
            }
        }
    }
    Ok(t)
}

fn delta(after: &LinkStats, before: &LinkStats) -> LinkStats {
    let d = |a: u32, b: u32| a.wrapping_sub(b);
    LinkStats {
        init_calls: d(after.init_calls, before.init_calls),
        init_ok: d(after.init_ok, before.init_ok),
        recv_calls: d(after.recv_calls, before.recv_calls),
        recv_data: d(after.recv_data, before.recv_data),
        recv_bytes: d(after.recv_bytes, before.recv_bytes),
        recv_lost: d(after.recv_lost, before.recv_lost),
        read_calls: d(after.read_calls, before.read_calls),
        read_failed: d(after.read_failed, before.read_failed),
        send_calls: d(after.send_calls, before.send_calls),
        send_ok: d(after.send_ok, before.send_ok),
        send_bytes: d(after.send_bytes, before.send_bytes),
        pio_calls: d(after.pio_calls, before.pio_calls),
        pio_failed: d(after.pio_failed, before.pio_failed),
        // Maxima reset when the load changes, which starts every phase: the phase's own.
        recv_ticks_max: after.recv_ticks_max,
        send_ticks_max: after.send_ticks_max,
        tick_ticks_max: after.tick_ticks_max,
        tick_ticks_total: d(after.tick_ticks_total, before.tick_ticks_total),
        agent_ticks: d(after.agent_ticks, before.agent_ticks),
        agent_frames: d(after.agent_frames, before.agent_frames),
        agent_ready: after.agent_ready,
        m64p_requests: d(after.m64p_requests, before.m64p_requests),
        m64p_errors: d(after.m64p_errors, before.m64p_errors),
        m64p_last_error: after.m64p_last_error,
    }
}

/// The part of a check's name that says which build ran it. The cart's default build is `default`
/// on every cart, so an X7 run's checks line up with an SC64 baseline's.
fn variant_tag(v: u32, default: u32) -> &'static str {
    match v {
        _ if v == default => "default",
        VARIANT_X7_DMA => "x7_dma",
        VARIANT_X7_IO => "x7_io",
        _ => "other",
    }
}

fn record<F: FnMut(String) + Send>(run: &mut BringupRun, c: Check, log: &mut F) {
    log(format!("{}  {}  {}", c.outcome.label(), c.name, c.detail));
    run.checks.push(c);
}

/// What one variant's phases leave for the next: go on, or stop because the link is gone.
enum Next {
    Continue,
    Stop,
}

/// Where a run is up to, for [`run_variant`].
struct Target<'a> {
    addr: u32,
    rom_header: &'a [u8],
    default: u32,
    rounds: u32,
    /// HELLO_ACK said the agent answers PEEKROM.
    cart_rom: bool,
}

/// Every load level through one agent build. `Err` when a control write or a report read failed:
/// the link went away mid-phase, and the caller records that.
async fn run_variant<F: FnMut(String) + Send>(
    link: &mut Link,
    run: &mut BringupRun,
    t: &Target<'_>,
    v: u32,
    seed: &mut u32,
    log: &mut F,
) -> Result<Next> {
    let addr = t.addr;
    let tag = variant_tag(v, t.default);
    if v != t.default {
        link.set_control(addr + OFF_CTL_VARIANT, v).await?;
        // Let the ROM switch before asking: a HELLO the old build answers proves nothing.
        tokio::time::sleep(Duration::from_millis(200)).await;
        if link.hello_within(link.timeout * 2).await?.is_none() {
            let c = Check::new(
                format!("link.{tag}.hello"),
                Outcome::Fail,
                format!(
                    "{} never answered HELLO. It cannot be switched back over the link: press R on the controller for the IO build.",
                    variant_name(v)
                ),
            );
            record(run, c, log);
            return Ok(Next::Stop);
        }
        record(
            run,
            Check::new(format!("link.{tag}.hello"), Outcome::Pass, variant_name(v)),
            log,
        );
    }

    for load in [LOAD_OFF, LOAD_MODERATE, LOAD_HEAVY] {
        let lname = load_name(load);
        link.set_control(addr + OFF_CTL_LOAD, load).await?;
        // The ROM acts on it next frame; read until it says so. Under heavy load a read can go
        // unanswered like any other request, so two of those end the wait rather than the run.
        let mut before: Option<Report> = None;
        let mut settled = false;
        let mut unanswered = 0;
        for _ in 0..10 {
            match link.read_report(addr).await {
                Ok(r) => {
                    settled = r.load == load && r.variant == v;
                    before = Some(r);
                    if settled {
                        break;
                    }
                }
                Err(_) => {
                    unanswered += 1;
                    if unanswered >= 2 {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // A report that answered but never showed the build and level asked for means the traffic
        // would run on something else and be labeled as this. Only unanswered reads, which heavy
        // load can cause, let the phase go ahead unconfirmed.
        if let (false, Some(b)) = (settled, &before) {
            let c = Check::new(
                format!("link.{tag}.load_{lname}.traffic"),
                Outcome::Fail,
                format!(
                    "the ROM never switched: asked for {} under {lname} load, still {} under {}",
                    variant_name(v),
                    b.variant_name,
                    load_name(b.load)
                ),
            );
            record(run, c, log);
            continue;
        }
        let rom_header = t.cart_rom.then_some(t.rom_header);
        let tally = phase_traffic(link, addr, rom_header, t.rounds, *seed).await?;
        *seed = seed.wrapping_add(1000);
        let after = link.read_report(addr).await.ok();

        let rtts = &tally.rtts;
        let (min, max) = rtts
            .iter()
            .fold((f64::MAX, 0f64), |(lo, hi), &x| (lo.min(x), hi.max(x)));
        let min = if rtts.is_empty() { 0.0 } else { min };
        let avg = if rtts.is_empty() {
            0.0
        } else {
            rtts.iter().sum::<f64>() / rtts.len() as f64
        };
        let d = match (&after, before.as_ref().and_then(Report::current_link)) {
            (Some(a), Some(b)) => a.current_link().map(|al| delta(al, b)).unwrap_or_default(),
            _ => LinkStats::default(),
        };

        let outcome = if tally.wrong > 0 {
            Outcome::Fail
        } else if tally.timeouts > 0 {
            // Under heavy load the agent gives up on a busy PI by design, and the host times out.
            if load == LOAD_HEAVY {
                Outcome::Info
            } else {
                Outcome::Fail
            }
        } else {
            Outcome::Pass
        };
        let mut detail = format!(
            "{}: {} requests, {} timeouts, {} busy, {} wrong; rtt {min:.0}/{avg:.0}/{max:.0} ms",
            variant_name(v),
            tally.requests,
            tally.timeouts,
            tally.busy,
            tally.wrong,
        );
        if !tally.notes.is_empty() {
            detail += &format!("; {}", tally.notes.join("; "));
        }
        let c =
            Check::new(format!("link.{tag}.load_{lname}.traffic"), outcome, detail).with_value(avg);
        record(run, c, log);
        if let Some(a) = &after {
            let c = Check::new(
                format!("link.{tag}.load_{lname}.driver"),
                Outcome::Info,
                format!(
                    "lost {} send failed {} read failed {} pi_io busy {}/{} m64p errors {}; longest tick {:.0} us, receive {:.0} us, send {:.0} us; irq gap {:.0} us (period {:.0}), load DMAs {} skipped {}",
                    d.recv_lost,
                    d.send_calls.wrapping_sub(d.send_ok),
                    d.read_failed,
                    d.pio_failed,
                    d.pio_calls,
                    d.m64p_errors,
                    a.us(d.tick_ticks_max),
                    a.us(d.recv_ticks_max),
                    a.us(d.send_ticks_max),
                    a.us(a.irq_gap_max),
                    a.us(a.load_period_ticks),
                    a.load_started,
                    a.load_skipped
                ),
            )
            .with_value(a.us(d.tick_ticks_max));
            record(run, c, log);
        }
        let gave_up = tally.consecutive >= GIVE_UP_TIMEOUTS;
        run.phases.push(Phase {
            variant: v,
            variant_name: variant_name(v).into(),
            load,
            load_name: lname.into(),
            requests: tally.requests,
            timeouts: tally.timeouts,
            busy: tally.busy,
            wrong: tally.wrong,
            rtt_ms_min: min,
            rtt_ms_avg: avg,
            rtt_ms_max: max,
            delta: d,
            after,
        });
        if gave_up {
            run.notes.push(format!(
                "{} stopped answering under {lname} load; its remaining phases were skipped",
                variant_name(v)
            ));
            // Put the load back so the next build, or a person, finds the ROM idle. It may not
            // land: the build that stopped answering is the one that would apply it.
            let _ = link.set_control(addr + OFF_CTL_LOAD, LOAD_OFF).await;
            return Ok(Next::Continue);
        }
    }
    Ok(Next::Continue)
}

/// Run everything against a booted `multi64_bringup.z64`. `Err` only when no run happened at all
/// (no daemon to connect to); a cart that never answers is a run with failed checks.
pub async fn run_bringup<F: FnMut(String) + Send>(
    opts: &BringupOptions,
    log: &mut F,
) -> Result<BringupRun> {
    let timeout = Duration::from_secs_f64(opts.recv_timeout_secs.max(0.5));
    let mut run = BringupRun {
        format: RUN_FORMAT,
        tool: format!("multi64-test-connector {}", env!("CARGO_PKG_VERSION")),
        started_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        daemon: super::suite::daemon_info(&opts.base_url).ok(),
        hello: None,
        report_addr: None,
        boot_report: None,
        phases: Vec::new(),
        final_report: None,
        checks: Vec::new(),
        notes: Vec::new(),
    };

    match &run.daemon {
        Some(d) => log(format!(
            "multi64d: serial {} cart {} serialActive {}",
            d.serial, d.cart, d.serial_active
        )),
        None => run.notes.push(format!(
            "multi64d did not answer GET / at {}",
            opts.base_url
        )),
    }

    let mut quiet = |_: String| {};
    let mut link = Link::connect(&opts.ws_url, timeout, &mut quiet).await?;

    let Some(hello) = link.hello_within(timeout * 2).await? else {
        let c = Check::new(
            "link.hello",
            Outcome::Fail,
            "no HELLO_ACK: the agent never answered. Photograph the bring-up screen; it shows what the ROM found without a link.",
        );
        record(&mut run, c, log);
        return Ok(run);
    };
    let c = Check::new(
        "link.hello",
        Outcome::Pass,
        format!(
            "proto {} agent {} rdram {} B flags {:#04x}, {:.0} ms",
            hello.proto, hello.agent_ver, hello.rdram_bytes, hello.flags, hello.rtt_ms
        ),
    );
    record(&mut run, c, log);
    let cart_rom = hello.flags & 0x02 != 0;
    run.hello = Some(hello);

    let found = match find_report(&mut link).await {
        Ok(f) => f,
        Err(e) => {
            run.notes.push(format!("scan stopped: {e:#}"));
            None
        }
    };
    let Some(addr) = found else {
        let c = Check::new(
            "link.report_found",
            Outcome::Fail,
            "no report in the first 1 MiB of RDRAM: is multi64_bringup.z64 the ROM running?",
        );
        record(&mut run, c, log);
        return Ok(run);
    };
    let boot = match link.read_report(addr).await {
        Ok(b) => b,
        Err(e) => {
            let c = Check::new(
                "link.report_found",
                Outcome::Fail,
                format!("at {addr:#x}, but {e:#}"),
            );
            record(&mut run, c, log);
            return Ok(run);
        }
    };
    let c = Check::new(
        "link.report_found",
        Outcome::Pass,
        format!(
            "at {addr:#x}: bring-up ROM {}, cart {}, link {}",
            boot.rom_version, boot.cart_name, boot.variant_name
        ),
    );
    record(&mut run, c, log);
    run.report_addr = Some(addr);
    for c in report_checks(&boot) {
        record(&mut run, c, log);
    }

    let rom_header: Vec<u8> = (0..64)
        .map(|i| u8::from_str_radix(&boot.rom_header[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect();
    let default = boot.variant;
    let mut variants = vec![default];
    if boot.cart == CART_X_SERIES && !opts.default_variant_only {
        // The other X7 build: the DMA one, unless someone pressed R and it is already running.
        variants.push(if default == VARIANT_X7_DMA {
            VARIANT_X7_IO
        } else {
            VARIANT_X7_DMA
        });
    }
    run.boot_report = Some(boot);

    let target = Target {
        addr,
        rom_header: &rom_header,
        default,
        rounds: opts.rounds,
        cart_rom,
    };
    let mut seed = 1u32;
    for &v in &variants {
        match run_variant(&mut link, &mut run, &target, v, &mut seed, log).await {
            Ok(Next::Continue) => {}
            Ok(Next::Stop) => break,
            Err(e) => {
                let tag = variant_tag(v, default);
                let c = Check::new(
                    format!("link.{tag}.aborted"),
                    Outcome::Fail,
                    format!("{}: {e:#}", variant_name(v)),
                );
                record(&mut run, c, log);
                break;
            }
        }
    }

    // Leave the ROM as it booted.
    let _ = link.set_control(addr + OFF_CTL_LOAD, LOAD_OFF).await;
    let _ = link.set_control(addr + OFF_CTL_VARIANT, default).await;
    let _ = link.hello_within(timeout).await;
    run.final_report = link.read_report(addr).await.ok();
    Ok(run)
}

// ---- comparing two runs ---------------------------------------------------------------------

/// One line per check whose outcome differs from the baseline's, or that only one run has, then
/// the measured values side by side. Empty when everything matches.
pub fn compare(run: &BringupRun, baseline: &BringupRun) -> Vec<String> {
    let mut lines = Vec::new();
    for b in &baseline.checks {
        match run.checks.iter().find(|c| c.name == b.name) {
            None => lines.push(format!("MISSING  {}  (baseline: {:?})", b.name, b.outcome)),
            Some(c) if c.outcome != b.outcome => {
                let kind = match (b.outcome, c.outcome) {
                    (Outcome::Pass, Outcome::Fail) => "REGRESSED",
                    (Outcome::Fail, Outcome::Pass) => "FIXED",
                    _ => "CHANGED",
                };
                lines.push(format!(
                    "{kind}  {}  {:?} -> {:?}: {}",
                    c.name, b.outcome, c.outcome, c.detail
                ));
            }
            Some(_) => {}
        }
    }
    for c in &run.checks {
        if !baseline.checks.iter().any(|b| b.name == c.name) {
            lines.push(format!("NEW  {}  {:?}: {}", c.name, c.outcome, c.detail));
        }
    }
    for c in &run.checks {
        if let (Some(v), Some(bv)) = (
            c.value,
            baseline
                .checks
                .iter()
                .find(|b| b.name == c.name)
                .and_then(|b| b.value),
        ) {
            let ratio = if bv > 0.0 { v / bv } else { 0.0 };
            lines.push(format!(
                "VALUE  {}  {v:.1} vs {bv:.1} ({ratio:.2}x)",
                c.name
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report as the ROM lays it out (report.h), with a few fields set.
    fn synthetic(addr: u32) -> Vec<u8> {
        let mut b = vec![0u8; REPORT_HEAD];
        let mut put = |off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_be_bytes());
        put(0x0C, 0x1388);
        put(0x10, addr);
        put(0x14, 0x0001_0000);
        put(0x1C, 46_875_000);
        put(0x30, CART_X_SERIES);
        put(0x34, VARIANT_X7_IO);
        put(0x6C, (1 << 4) | (1 << 2));
        put(0x70, 1 << 2);
        put(0x74, 0xED64_0013);
        // timing[0][1]: X7 USBCFG, 20000 spins, 1024 reads in 46875 ticks = 1 ms -> ~977 ns each
        put(OFF_TIMING + 16, 2);
        put(OFF_TIMING + 20, 20_000);
        put(OFF_TIMING + 24, 46_875);
        // buffer[0][1]: IO>DMA mismatch at byte 4, 508 bytes
        put(OFF_BUFFER + 20, 2);
        put(OFF_BUFFER + 24, 4);
        put(OFF_BUFFER + 28, 508);
        put(OFF_BUFFER, 1);
        put(OFF_BUFFER + 4, u32::MAX);
        // link[1] (X7 IO): recv_lost
        put(OFF_LINK + LINK_WORDS * 4 + 5 * 4, 7);
        b[0..8].copy_from_slice(&REPORT_MAGIC);
        b
    }

    #[test]
    fn parses_the_layout_the_rom_writes() {
        let r = Report::parse(0x3EB50, &synthetic(0x3EB50)).unwrap();
        assert_eq!(r.cart, CART_X_SERIES);
        assert_eq!(r.variant_name, "X7, CPU words");
        assert_eq!(r.ed_reg14_unlocked, 0xED64_0013);
        assert_eq!(r.timing[0].len(), 1);
        assert_eq!(r.timing[0][0].name, "X7 USBCFG");
        assert!((r.timing[0][0].ns_per_read - 976.6).abs() < 1.0);
        assert!((r.timing[0][0].wait_limit_ms - 19.53).abs() < 0.1);
        assert_eq!(r.buffer[0][0].result, 1);
        assert_eq!(r.buffer[0][0].first_bad, None);
        assert_eq!(r.buffer[0][1].first_bad, Some(4));
        assert_eq!(r.current_link().unwrap().recv_lost, 7);
    }

    #[test]
    fn rejects_a_wrong_magic_or_format() {
        let mut b = synthetic(0x1000);
        b[0] = b'X';
        assert!(Report::parse(0x1000, &b).is_err());
        let mut b = synthetic(0x1000);
        b[0x0B] = 1;
        assert!(Report::parse(0x1000, &b).is_err());
    }

    #[test]
    fn report_checks_fail_a_mismatched_buffer_and_a_silent_driver() {
        let r = Report::parse(0x1000, &synthetic(0x1000)).unwrap();
        let checks = report_checks(&r);
        let get = |n: &str| checks.iter().find(|c| c.name == n).unwrap();
        assert_eq!(get("identify.cart").outcome, Outcome::Pass);
        // init_ok has the X7 bit (1 << 2) set.
        assert_eq!(get("identify.driver_init").outcome, Outcome::Pass);
        assert_eq!(get("blocks.buffer.IO>IO.no_load").outcome, Outcome::Pass);
        assert_eq!(get("blocks.buffer.IO>DMA.no_load").outcome, Outcome::Fail);
        assert_eq!(get("blocks.buffer.DMA>IO.no_load").outcome, Outcome::Skip);
        assert_eq!(
            get("blocks.timing.cart_status.no_load")
                .value
                .map(|v| v.round()),
            Some(977.0)
        );
    }

    #[test]
    fn region_parsing_checks_every_length() {
        let body = [0, 1, 2, 0, 2, 0xAA, 0xBB, 0, 1, 0xCC];
        let r = parse_regions(&body, &[(0, 2), (8, 1)]).unwrap();
        assert_eq!(r, vec![vec![0xAA, 0xBB], vec![0xCC]]);
        assert!(parse_regions(&body, &[(0, 2), (8, 2)]).is_err());
        assert!(parse_regions(&body, &[(0, 2)]).is_err());
    }

    #[test]
    fn compare_names_regressions_and_new_checks() {
        let run = |checks: Vec<Check>| BringupRun {
            format: RUN_FORMAT,
            tool: String::new(),
            started_unix: 0,
            daemon: None,
            hello: None,
            report_addr: None,
            boot_report: None,
            phases: vec![],
            final_report: None,
            checks,
            notes: vec![],
        };
        let base = run(vec![
            Check::new("a", Outcome::Pass, ""),
            Check::new("b", Outcome::Pass, "").with_value(100.0),
        ]);
        let now = run(vec![
            Check::new("a", Outcome::Fail, "broke"),
            Check::new("b", Outcome::Pass, "").with_value(250.0),
            Check::new("c", Outcome::Pass, ""),
        ]);
        let lines = compare(&now, &base);
        assert!(lines.iter().any(|l| l.starts_with("REGRESSED  a")));
        assert!(lines.iter().any(|l| l.starts_with("NEW  c")));
        assert!(lines.iter().any(|l| l.contains("(2.50x)")));
        assert!(compare(&base, &base).iter().all(|l| l.starts_with("VALUE")));
    }

    #[test]
    fn patterns_differ_by_seed() {
        assert_ne!(pattern(1, 64), pattern(2, 64));
        assert_eq!(pattern(5, 4096).len(), 4096);
    }
}
