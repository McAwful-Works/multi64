//! The end-to-end suite: every check of the L3 bridge, in order, as a library.
//!
//! It lives here rather than in a script so that the GUI test app and the `suite` subcommand run
//! the *same* checks. Two implementations of the same thirty would drift, and the one that drifted
//! would be the one nobody ran.
//!
//! What a caller must have running: `multi64d` against a cart, and `multi64_test.z64` booted. The
//! controller is never needed — the ROM boots into `RAW_ECHO`, which parses nothing, and the suite
//! drives it out with `REQ_SET_MODE` (`docs/spec/test-l3-application-v0.md` §10).
//!
//! Two properties are worth stating because they are easy to get wrong:
//!
//! - **A timeout does not mean the cart is silent.** `multi64d` accepts and discards writes while
//!   its link is released or faulted, with no error and no close, so a dead link and a silent cart
//!   look identical from the WebSocket. Every failure therefore reads `GET /` at the moment it
//!   fails and says which it was.
//! - **A negative check matches on what the cart said**, not on an error having occurred. Every way
//!   of not reaching the cart also produces an error, so an error-only check would report "the cart
//!   refused this" when nothing was ever asked — a check that cannot fail.

use crate::{run_connector_command, run_listen, ConnectorCommand};
use multi64_ed64_l2::{Ed64L2Pipe, DEFAULT_ED64_CHUNK};
use multi64_ed64pro_l2::Ed64ProL2Pipe;
use multi64_l3::{Channel, Frame, FrameFlags, FrameType};
use multi64_sc64_l2::Sc64L2Pipe;
use serde::Serialize;
use std::time::{Duration, Instant};

pub const DEFAULT_WS_URL: &str = "ws://127.0.0.1:38765/ws";
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:38765";
pub const DEFAULT_PORT: &str = "COM4";

/// The phases a run reports, in order.
///
/// Public so a UI can lay the whole structure out before the run starts, rather than growing
/// headings as results arrive. [`run_suite`] labels every result from this array, so the two
/// cannot disagree about what a phase is called.
pub const PHASES: [&str; 7] = [
    "Preflight",
    "Cart liveness",
    "M64T",
    "M64P",
    "BENCH",
    "Direct serial",
    "Stream health",
];

/// The payload `sc64-echo-test` uses, kept identical so the two agree on hardware.
const SERIAL_ECHO_PAYLOAD: &[u8] = b"multi64_test";
/// The direct-serial checks, named once so a run that skips them lists the same rows as one that
/// does not.
const SERIAL_CHECKS: [&str; 4] = [
    "serial echo round trip",
    "L3 framing over serial, including an 8 KiB frame",
    "the 8 KiB frame again, one USB message at a time",
    "the cart finished every write it started",
];
/// The multi-chunk frame `sc64-l3-framing-e2e --large` sends.
const LARGE_PAYLOAD_LEN: usize = 8292;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail {
        detail: String,
    },
    /// Not run, or run but not verifiable from the host. The reason says which.
    Skip {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    /// Which part of the run this belongs to, for grouping in a UI.
    pub phase: String,
    pub name: String,
    pub outcome: Outcome,
    pub millis: u64,
    /// Extra context worth showing even on a pass, e.g. the decoded `DIAG` line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CheckResult {
    pub fn passed(&self) -> bool {
        matches!(self.outcome, Outcome::Pass)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteOptions {
    pub ws_url: String,
    pub base_url: String,
    /// Serial port for the direct-serial phase. Only used when `skip_serial` is false.
    pub port: String,
    /// The ROM version string the run expects. `None` skips that check rather than passing it: a
    /// version nothing can verify has to stay visible, because it is the check that stops the whole
    /// run being a test of some other ROM.
    pub expect_rom: Option<String>,
    pub skip_serial: bool,
    pub recv_timeout_secs: f64,
}

impl Default for SuiteOptions {
    fn default() -> Self {
        Self {
            ws_url: DEFAULT_WS_URL.into(),
            base_url: DEFAULT_BASE_URL.into(),
            port: DEFAULT_PORT.into(),
            expect_rom: None,
            skip_serial: false,
            recv_timeout_secs: 5.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuiteSummary {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
}

impl SuiteSummary {
    /// 0 when nothing failed, 1 otherwise. A run that could not start is reported separately, as an
    /// `Err` from [`run_suite`], so a caller can tell a broken cart from a run that never happened.
    pub fn exit_code(&self) -> i32 {
        if self.failed == 0 {
            0
        } else {
            1
        }
    }
}

// --- daemon HTTP -------------------------------------------------------------------

fn http_url(base: &str, path: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.starts_with("http://") || b.starts_with("https://") {
        format!("{b}{path}")
    } else {
        format!("http://{b}{path}")
    }
}

/// ureq 3 moved timeouts from the request builder onto agent config, so each call builds a one-shot
/// agent carrying its own deadline — the same shape Xfer64's daemon client uses.
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn get_text(base: &str, path: &str, timeout: Duration) -> Result<String, String> {
    let url = http_url(base, path);
    let mut r = agent(timeout)
        .get(&url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if r.status().as_u16() != 200 {
        return Err(format!("GET {url}: HTTP {}", r.status().as_u16()));
    }
    r.body_mut()
        .read_to_string()
        .map_err(|e| format!("read body: {e}"))
}

fn post_text(base: &str, path: &str, timeout: Duration) -> Result<String, String> {
    let url = http_url(base, path);
    let mut r = agent(timeout)
        .post(&url)
        .send_empty()
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = r.status().as_u16();
    let body = r.body_mut().read_to_string().unwrap_or_default();
    if status != 200 {
        // The daemon answers these with plain text, not JSON, so the body is the message.
        return Err(format!("POST {url}: HTTP {status}: {}", body.trim()));
    }
    Ok(body)
}

/// One field out of the daemon's `GET /`. Small enough not to want a struct, and tolerant of the
/// mixed casing in that response (`websocket_path` beside `serialActive`).
fn json_field(body: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let f = v.get(key)?;
    Some(match f {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// What a running `multi64d` reports about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    /// The serial device the daemon was told to use, e.g. `COM4`.
    pub serial: String,
    /// The cart kind it was configured with — `sc64`, `ed64`, `ed64pro`. Configuration, **not** a
    /// detection: it does not mean a cart of that kind is attached.
    pub cart: String,
    /// Whether the daemon currently holds an open handle on that port. True does not mean the cart
    /// is answering; see the module docs.
    pub serial_active: bool,
}

/// Ask a running daemon which port it is using, so a caller does not have to be told.
///
/// A caller that defaults to a port of its own is a caller that can disagree with the daemon, and
/// on a machine where the cart is not on the usual port that disagreement means opening some other
/// device entirely.
pub fn daemon_info(base_url: &str) -> Result<DaemonInfo, String> {
    let body = get_text(base_url, "/", Duration::from_secs(3))?;
    Ok(DaemonInfo {
        serial: json_field(&body, "serial").unwrap_or_default(),
        cart: json_field(&body, "cart").unwrap_or_default(),
        serial_active: json_field(&body, "serialActive").as_deref() == Some("true"),
    })
}

/// The decoded counter snapshot out of a `diag` command's output, to show beside the result.
fn diag_note(out: &str) -> Option<String> {
    out.lines()
        .find(|l| l.starts_with("DIAG "))
        .map(|l| l.trim_start_matches("DIAG ").to_string())
}

/// `tx_failures` out of a `diag` command's output. `None` when the ROM's `DIAG` does not carry it.
fn tx_failures_in(out: &str) -> Option<u32> {
    diag_note(out)?
        .split_whitespace()
        .find_map(|f| f.strip_prefix("tx_failures="))?
        .parse()
        .ok()
}

/// Judge the cart's count of writes that gave up, read before and after the direct-serial checks.
///
/// The count runs since boot, because those checks change mode on the way in and out and every
/// other counter resets on a mode change. Nothing else sees these failures: the host only notices
/// a malformed message, and the cart's own replies carry no error.
fn write_failures_outcome(before: Option<u32>, after: Option<u32>) -> Outcome {
    let (Some(before), Some(after)) = (before, after) else {
        return Outcome::Skip {
            reason: "the cart's DIAG does not count failed writes (a ROM older than 1.11), or \
                     could not be read"
                .into(),
        };
    };
    match after.wrapping_sub(before) {
        0 => Outcome::Pass,
        n => Outcome::Fail {
            detail: format!("{n} cart write(s) gave up before the whole message was sent"),
        },
    }
}

/// Why a check failed, in the one case the WebSocket cannot distinguish. See the module docs.
fn link_note(base: &str) -> String {
    match get_text(base, "/", Duration::from_secs(3)) {
        Err(_) => " [the daemon did not answer GET / — is multi64d still running?]".into(),
        Ok(root) => {
            let active = json_field(&root, "serialActive").unwrap_or_default();
            let busy = json_field(&root, "serialBusy").unwrap_or_default();
            if active != "true" {
                " [serialActive:false — the link was down, so the request was dropped before it reached the cart]".into()
            } else if busy == "true" {
                " [serialBusy:true — the link was held elsewhere; the cart may never have been asked]".into()
            } else {
                " [the link was up, so the cart itself did not answer]".into()
            }
        }
    }
}

// --- direct serial -----------------------------------------------------------------
// These are what `sc64-echo-test` and `sc64-l3-framing-e2e` (or their `ed64-` twins) do, linked
// rather than spawned so the app needs no binaries beside it. Both need the ROM in RAW_ECHO and
// the daemon's port released.

/// Which L2 pipe the direct-serial checks open: the daemon's, always.
///
/// Each cart frames L3 differently on the wire, so a pipe for the wrong cart does not fail to open
/// — it opens, writes bytes the cart discards, and times out. That reads as a broken cart, so the
/// kind is taken from the daemon rather than assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SerialCart {
    Sc64,
    Ed64,
    Ed64Pro,
}

impl SerialCart {
    /// From the daemon's `cart` field. `None` for anything else, so an unknown kind is skipped
    /// rather than guessed at.
    fn from_daemon(cart: &str) -> Option<Self> {
        match cart {
            "sc64" => Some(SerialCart::Sc64),
            "ed64" => Some(SerialCart::Ed64),
            "ed64pro" => Some(SerialCart::Ed64Pro),
            _ => None,
        }
    }
}

enum SerialPipe {
    Sc64(Sc64L2Pipe),
    Ed64(Ed64L2Pipe),
    Ed64Pro(Ed64ProL2Pipe),
}

macro_rules! each_pipe {
    ($target:expr, $p:ident => $e:expr) => {
        match $target {
            SerialPipe::Sc64($p) => $e,
            SerialPipe::Ed64($p) => $e,
            SerialPipe::Ed64Pro($p) => $e,
        }
    };
}

impl SerialPipe {
    /// Opens the port the way `multi64d` does, then readies it for a round trip.
    fn open(cart: SerialCart, port: &str) -> Result<Self, String> {
        let mut pipe = match cart {
            SerialCart::Sc64 => {
                SerialPipe::Sc64(Sc64L2Pipe::open(port, 115_200).map_err(|e| e.to_string())?)
            }
            SerialCart::Ed64 => {
                SerialPipe::Ed64(Ed64L2Pipe::open(port, 115_200).map_err(|e| e.to_string())?)
            }
            // The PRO runs at its own fixed baud.
            SerialCart::Ed64Pro => {
                SerialPipe::Ed64Pro(Ed64ProL2Pipe::open(port).map_err(|e| e.to_string())?)
            }
        };
        each_pipe!(&mut pipe, p => p.set_timeout(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        each_pipe!(&mut pipe, p => p.clear_serial_buffers()).map_err(|e| e.to_string())?;
        Ok(pipe)
    }

    /// Discard input until the line has been quiet for 500 ms (at most 5 s), then clear.
    fn settle(&mut self) -> Result<(), String> {
        let started = Instant::now();
        let mut quiet_since = Instant::now();
        let mut scratch = [0u8; 512];
        while quiet_since.elapsed() < Duration::from_millis(500) {
            if started.elapsed() > Duration::from_secs(5) {
                return Err("the cart was still sending after 5 s".into());
            }
            // Errors are what is being drained — a half-sent message fails to parse — so they
            // count as traffic, not as a reason to stop.
            match each_pipe!(&mut *self, p => p.read_l3_bytes(&mut scratch)) {
                Ok(0) => {}
                _ => quiet_since = Instant::now(),
            }
        }
        each_pipe!(&mut *self, p => p.clear_serial_buffers()).map_err(|e| e.to_string())
    }
}

/// The two operations the direct-serial checks need, so they can run against a fake in tests.
trait L3Link {
    fn write_l3_stream(&mut self, buf: &[u8]) -> std::io::Result<()>;
    fn read_l3_bytes_exact(&mut self, out: &mut [u8], deadline: Duration) -> std::io::Result<()>;
}

impl L3Link for SerialPipe {
    fn write_l3_stream(&mut self, buf: &[u8]) -> std::io::Result<()> {
        each_pipe!(self, p => p.write_l3_stream(buf))
    }

    fn read_l3_bytes_exact(&mut self, out: &mut [u8], deadline: Duration) -> std::io::Result<()> {
        each_pipe!(self, p => p.read_l3_bytes_exact(out, deadline))
    }
}

fn serial_echo(cart: SerialCart, port: &str, timeout: Duration) -> Result<String, String> {
    let mut pipe = SerialPipe::open(cart, port)?;
    pipe.write_l3_stream(SERIAL_ECHO_PAYLOAD)
        .map_err(|e| e.to_string())?;
    let mut back = vec![0u8; SERIAL_ECHO_PAYLOAD.len()];
    pipe.read_l3_bytes_exact(&mut back, timeout)
        .map_err(|e| e.to_string())?;
    if back != SERIAL_ECHO_PAYLOAD {
        return Err(format!(
            "echoed {} bytes but they differ from what was sent",
            back.len()
        ));
    }
    Ok(format!("{} bytes echoed", SERIAL_ECHO_PAYLOAD.len()))
}

fn serial_framing(cart: SerialCart, port: &str, timeout: Duration) -> Result<String, String> {
    framing_burst(&mut SerialPipe::open(cart, port)?, timeout)
}

fn serial_framing_lockstep(
    cart: SerialCart,
    port: &str,
    timeout: Duration,
) -> Result<String, String> {
    let mut pipe = SerialPipe::open(cart, port)?;
    // If the burst check failed, the cart may still be echoing what is left of it; clearing the
    // host's buffers once does not stop bytes that are yet to arrive.
    pipe.settle()?;
    framing_lockstep(&mut pipe, timeout)
}

/// The frame that spans many USB messages: 8,308 bytes on the wire.
fn large_frame() -> Frame {
    Frame {
        ty: FrameType::Data,
        channel: Channel::Application,
        flags: FrameFlags::FINAL,
        request_id: 0xAABB_CCDD,
        payload: (0..LARGE_PAYLOAD_LEN).map(|i| (i & 0xFF) as u8).collect(),
    }
}

/// Each frame written whole, then read back. The cart echoes every USB message as it arrives, so
/// for the large frame it is sending while the host still is.
fn framing_burst(pipe: &mut impl L3Link, timeout: Duration) -> Result<String, String> {
    let cases = vec![
        (
            "small DATA",
            Frame {
                ty: FrameType::Data,
                channel: Channel::Application,
                flags: FrameFlags::FINAL,
                request_id: 1,
                payload: b"multi64-l3-framing".to_vec(),
            },
        ),
        (
            "HEARTBEAT",
            Frame {
                ty: FrameType::Heartbeat,
                channel: Channel::Control,
                flags: FrameFlags::empty(),
                request_id: 0,
                payload: Vec::new(),
            },
        ),
        ("large DATA across USB chunks", large_frame()),
    ];

    for (label, frame) in cases {
        let wire = frame
            .encode()
            .map_err(|e| format!("{label}: encode: {e}"))?;
        pipe.write_l3_stream(&wire)
            .map_err(|e| format!("{label}: write: {e}"))?;
        let mut back = vec![0u8; wire.len()];
        pipe.read_l3_bytes_exact(&mut back, timeout)
            .map_err(|e| format!("{label}: read: {e}"))?;
        verify_echo(label, &frame, &wire, &back)?;
    }
    Ok("small DATA, HEARTBEAT and an 8,308-byte frame".into())
}

/// The large frame again, one USB message at a time, each echo read before the next is sent.
///
/// Beside [`framing_burst`] this tells apart two failures that look alike. Burst failing while this
/// passes means the cart cannot send while the host is sending — libdragon's EverDrive write gives
/// up after 100 ms and leaves the message half sent — not that large frames are broken. Both
/// failing means the problem is somewhere else.
fn framing_lockstep(pipe: &mut impl L3Link, timeout: Duration) -> Result<String, String> {
    let label = "large DATA, one message at a time";
    let frame = large_frame();
    let wire = frame
        .encode()
        .map_err(|e| format!("{label}: encode: {e}"))?;
    let mut back = Vec::with_capacity(wire.len());
    let messages = wire.chunks(DEFAULT_ED64_CHUNK).len();
    for (i, msg) in wire.chunks(DEFAULT_ED64_CHUNK).enumerate() {
        let at = format!("{label}: message {} of {messages}", i + 1);
        pipe.write_l3_stream(msg)
            .map_err(|e| format!("{at}: write: {e}"))?;
        let mut echo = vec![0u8; msg.len()];
        pipe.read_l3_bytes_exact(&mut echo, timeout)
            .map_err(|e| format!("{at}: read: {e}"))?;
        back.extend_from_slice(&echo);
    }
    verify_echo(label, &frame, &wire, &back)?;
    Ok(format!("{} bytes in {messages} messages", wire.len()))
}

/// An echoed frame must be the same bytes, and decode to the frame that was sent.
fn verify_echo(label: &str, frame: &Frame, wire: &[u8], back: &[u8]) -> Result<(), String> {
    if back != wire {
        return Err(format!("{label}: wire bytes came back different"));
    }
    // Decoding as well as comparing: identical bytes that will not decode would still be a
    // broken frame, and the decode is what every real consumer does.
    let (decoded, consumed) = Frame::decode(back).map_err(|e| format!("{label}: decode: {e}"))?;
    if consumed != back.len() {
        return Err(format!(
            "{label}: decode consumed {consumed} of {} bytes",
            back.len()
        ));
    }
    if decoded.ty != frame.ty
        || decoded.channel != frame.channel
        || decoded.flags != frame.flags
        || decoded.request_id != frame.request_id
        || decoded.payload != frame.payload
    {
        return Err(format!(
            "{label}: the decoded frame does not match the original"
        ));
    }
    Ok(())
}

/// Puts the daemon's serial port back if the suite unwinds between release and resume.
///
/// Without it a panic or a cancelled run would leave `multi64d` holding no port, and Multi64 dead
/// until someone restarted it. This is the last resort; the happy path resumes explicitly and
/// disarms the guard.
struct ResumeGuard {
    base_url: String,
    armed: bool,
}

impl Drop for ResumeGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = post_text(&self.base_url, "/v1/serial/resume", Duration::from_secs(15));
        }
    }
}

// --- the runner --------------------------------------------------------------------

struct Runner<'a, F: FnMut(CheckResult)> {
    opts: &'a SuiteOptions,
    on: &'a mut F,
    summary: SuiteSummary,
    phase: String,
}

impl<'a, F: FnMut(CheckResult)> Runner<'a, F> {
    fn emit(&mut self, name: &str, outcome: Outcome, millis: u64, note: Option<String>) {
        match outcome {
            Outcome::Pass => self.summary.passed += 1,
            Outcome::Fail { .. } => self.summary.failed += 1,
            Outcome::Skip { .. } => self.summary.skipped += 1,
        }
        let r = CheckResult {
            phase: self.phase.clone(),
            name: name.to_string(),
            outcome,
            millis,
            note,
        };
        (self.on)(r);
    }

    fn pass(&mut self, name: &str, millis: u64, note: Option<String>) {
        self.emit(name, Outcome::Pass, millis, note);
    }

    fn fail(&mut self, name: &str, detail: String, millis: u64) {
        self.emit(name, Outcome::Fail { detail }, millis, None);
    }

    fn skip(&mut self, name: &str, reason: &str) {
        self.emit(
            name,
            Outcome::Skip {
                reason: reason.to_string(),
            },
            0,
            None,
        );
    }

    /// Run one connector command, expecting success. Returns its output when it succeeded.
    async fn check(&mut self, name: &str, cmd: ConnectorCommand) -> Option<String> {
        self.check_noting(name, cmd, |_| None).await
    }

    /// [`check`](Self::check), with a chance to lift something out of the command's output and show
    /// it beside the result — the decoded `DIAG` line, say, which is worth seeing on a pass.
    async fn check_noting(
        &mut self,
        name: &str,
        cmd: ConnectorCommand,
        note_from: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let started = Instant::now();
        let mut out = String::new();
        let mut log = |line: String| {
            out.push_str(&line);
            out.push('\n');
        };
        let res = run_connector_command(
            &self.opts.ws_url,
            self.opts.recv_timeout_secs,
            &cmd,
            &mut log,
        )
        .await;
        let ms = started.elapsed().as_millis() as u64;
        match res {
            Ok(()) => {
                let note = note_from(&out);
                self.pass(name, ms, note);
                Some(out)
            }
            Err(e) => {
                let detail = format!("{e}{}", link_note(&self.opts.base_url));
                self.fail(name, detail, ms);
                None
            }
        }
    }

    /// The cart's since-boot count of failed writes, read without reporting a check of its own.
    async fn read_tx_failures(&mut self) -> Option<u32> {
        let mut out = String::new();
        let mut log = |line: String| {
            out.push_str(&line);
            out.push('\n');
        };
        run_connector_command(
            &self.opts.ws_url,
            self.opts.recv_timeout_secs,
            &ConnectorCommand::Diag {
                expect_clean: false,
            },
            &mut log,
        )
        .await
        .ok()?;
        tx_failures_in(&out)
    }

    /// Run one connector command that MUST fail, and whose failure must mention `want`.
    async fn check_refused(&mut self, name: &str, want: &str, cmd: ConnectorCommand) {
        let started = Instant::now();
        let mut log = |_: String| {};
        let res = run_connector_command(
            &self.opts.ws_url,
            self.opts.recv_timeout_secs,
            &cmd,
            &mut log,
        )
        .await;
        let ms = started.elapsed().as_millis() as u64;
        match res {
            Ok(()) => self.fail(
                name,
                "the cart accepted something it should have rejected".into(),
                ms,
            ),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains(want) {
                    self.pass(name, ms, Some("refused, as it must be".into()));
                } else {
                    let detail = format!(
                        "failed, but not with {want:?}: {msg}{}",
                        link_note(&self.opts.base_url)
                    );
                    self.fail(name, detail, ms);
                }
            }
        }
    }

    async fn set_mode(&mut self, mode: u8, label: &str) -> bool {
        self.check(
            &format!("set mode {label}"),
            ConnectorCommand::SetMode { mode },
        )
        .await
        .is_some()
    }
}

/// Run every check, reporting each through `on_result` as it completes.
///
/// `Err` means the run could not start at all — no daemon — which a caller should present
/// differently from a failing check: one is a broken cart, the other a run that never happened.
pub async fn run_suite<F: FnMut(CheckResult)>(
    opts: &SuiteOptions,
    on_result: &mut F,
) -> Result<SuiteSummary, String> {
    let mut r = Runner {
        opts,
        on: on_result,
        summary: SuiteSummary::default(),
        phase: PHASES[0].into(),
    };

    // --- preflight ---------------------------------------------------------------
    if get_text(&opts.base_url, "/health", Duration::from_secs(3)).is_err() {
        return Err(format!(
            "No multi64d at {}. Start Multi64, or run: multi64d --serial {}",
            opts.base_url, opts.port
        ));
    }
    r.pass("daemon answers /health", 0, None);

    let root = get_text(&opts.base_url, "/", Duration::from_secs(3)).unwrap_or_default();
    let cart = json_field(&root, "cart").unwrap_or_else(|| "?".into());
    let serial = json_field(&root, "serial").unwrap_or_else(|| "?".into());
    if json_field(&root, "serialActive").as_deref() == Some("true") {
        r.pass(
            "daemon holds its serial port",
            0,
            Some(format!("serial={serial} cart={cart}")),
        );
    } else {
        // Not fatal by itself — the daemon retries once a second — but every cart check below will
        // fail and each would otherwise blame the cart. Say it once, here.
        r.fail(
            "daemon holds its serial port",
            format!("serialActive:false (serial={serial}); the cart checks cannot pass"),
            0,
        );
    }

    // --- liveness and identity ---------------------------------------------------
    r.phase = PHASES[1].into();
    // Also the proof that REQ_SET_MODE escapes RAW_ECHO: that is the mode the ROM boots into, and
    // this is the first thing sent.
    r.set_mode(1, "M64T_PROTO (from whatever it booted into)")
        .await;
    r.check("cart answers PING", ConnectorCommand::Ping).await;

    if let Some(out) = r
        .check("cart reports its ROM version", ConnectorCommand::Version)
        .await
    {
        match &opts.expect_rom {
            None => r.skip(
                "ROM on the cart is the expected build",
                "no expected version supplied; set one to assert it",
            ),
            Some(want) => {
                if out.contains(want.as_str()) {
                    r.pass(&format!("ROM on the cart is {want}"), 0, None);
                } else {
                    let got = out
                        .lines()
                        .find(|l| l.contains("VERSION"))
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    r.fail(
                        &format!("ROM on the cart is {want}"),
                        format!("got: {got} — rebuild and re-upload multi64_test.z64"),
                        0,
                    );
                }
            }
        }
    }

    r.check_noting(
        "cart reports its counters",
        ConnectorCommand::Diag {
            expect_clean: false,
        },
        diag_note,
    )
    .await;

    // --- M64T --------------------------------------------------------------------
    r.phase = PHASES[2].into();
    r.check(
        "echo returns what was sent",
        ConnectorCommand::Echo {
            hex: None,
            text: Some("multi64 l3 e2e".into()),
        },
    )
    .await;
    r.check(
        "echo of 4 KiB crosses USB chunks",
        ConnectorCommand::Echo {
            hex: Some("5a".repeat(4096)),
            text: None,
        },
    )
    .await;
    r.check("controller snapshot", ConnectorCommand::ReqController)
        .await;
    r.check(
        "session opens",
        ConnectorCommand::SessionOpen {
            hex_challenge: "0011223344556677".into(),
        },
    )
    .await;
    r.check("eeprom reports its geometry", ConnectorCommand::EepromInfo)
        .await;
    r.check(
        "eeprom reads",
        ConnectorCommand::EepromRead { offset: 0, len: 16 },
    )
    .await;
    r.check(
        "eeprom writes inside a session",
        ConnectorCommand::EepromWrite {
            offset: 0,
            hex: "0102030405060708".into(),
        },
    )
    .await;
    r.check("sram reports its geometry", ConnectorCommand::SramInfo)
        .await;
    r.check("session closes", ConnectorCommand::SessionClose)
        .await;

    // Run, but never counted as passes: the effect is on the console and the desk, and a host that
    // cannot observe it would only be asserting that the ROM sent an ack.
    for (name, cmd) in [
        (
            "display text on the ROM's HUD",
            ConnectorCommand::DisplayText {
                text: "multi64 test app".into(),
            },
        ),
        (
            "rumble port 0",
            ConnectorCommand::Rumble {
                port: 0,
                frames: 30,
            },
        ),
    ] {
        let mut log = |_: String| {};
        let acked =
            run_connector_command(&opts.ws_url, opts.recv_timeout_secs, &cmd, &mut log).await;
        let reason = if acked.is_ok() {
            "acted, and acknowledged — but the host cannot see the effect"
        } else {
            "no acknowledgement (the effect is not host-observable either way)"
        };
        r.skip(name, reason);
    }

    // --- M64P --------------------------------------------------------------------
    r.phase = PHASES[3].into();
    r.check("M64P says hello", ConnectorCommand::MemHello).await;
    r.check(
        "write, read back and restore RDRAM",
        ConnectorCommand::MemRoundTrip { len: 64 },
    )
    .await;
    // Every N64 ROM starts with the PI configuration word 80 37 12 40: reading it back proves the
    // agent reached the cartridge ROM, not RDRAM or a buffer of its own.
    r.check(
        "read the cartridge ROM header",
        ConnectorCommand::MemRomPeek {
            addr: 0,
            len: 4,
            expect_hex: Some("80371240".into()),
        },
    )
    .await;
    r.check_refused(
        "a ROM read past the cart window is refused",
        "PEEKROM rejected",
        ConnectorCommand::MemRomPeek {
            addr: 0x0400_0000,
            len: 4,
            expect_hex: None,
        },
    )
    .await;
    // M64P addresses are RDRAM physical offsets, so 0 is the *start* of RDRAM and valid. This has
    // to be past the end of any N64's memory.
    r.check_refused(
        "a read outside RDRAM is refused",
        "PEEKV rejected",
        ConnectorCommand::MemPeek {
            addr: 0x7F00_0000,
            len: 16,
        },
    )
    .await;

    // --- BENCH -------------------------------------------------------------------
    r.phase = PHASES[4].into();
    if r.set_mode(2, "BENCH").await {
        let started = Instant::now();
        let mut ticks = 0u32;
        let mut log = |line: String| {
            if line.contains("BENCH_TICK") {
                ticks += 1;
            }
        };
        let listened = run_listen(&opts.ws_url, 3.0, None, &mut log).await;
        let ms = started.elapsed().as_millis() as u64;
        match listened {
            Err(e) => r.fail("cart sends BENCH_TICK unprompted", e.to_string(), ms),
            Ok(()) if ticks >= 2 => r.pass(
                "cart sends BENCH_TICK unprompted",
                ms,
                Some(format!("{ticks} ticks in 3s")),
            ),
            Ok(()) => {
                let detail = format!(
                    "saw {ticks} in 3s, expected at least 2{}",
                    link_note(&opts.base_url)
                );
                r.fail("cart sends BENCH_TICK unprompted", detail, ms);
            }
        }
        r.set_mode(1, "M64T_PROTO").await;
    }

    // --- direct serial -----------------------------------------------------------
    r.phase = PHASES[5].into();
    if opts.skip_serial {
        r.skip("direct-serial checks", "skipped by request");
    } else if SerialCart::from_daemon(&cart).is_none() {
        r.skip(
            "direct-serial checks",
            &format!("the daemon reports cart={cart}, and the suite has no pipe for it"),
        );
    } else {
        let serial_cart = SerialCart::from_daemon(&cart).expect("checked above");
        let failures_before = r.read_tx_failures().await;
        r.set_mode(0, "RAW_ECHO").await;
        match post_text(
            &opts.base_url,
            "/v1/serial/release",
            Duration::from_secs(10),
        ) {
            Err(e) => {
                r.fail("daemon released the serial port", e, 0);
                for name in SERIAL_CHECKS {
                    r.skip(name, "the port was not released");
                }
                r.set_mode(1, "M64T_PROTO").await;
            }
            Ok(_) => {
                let mut guard = ResumeGuard {
                    base_url: opts.base_url.clone(),
                    armed: true,
                };
                r.pass("daemon released the serial port", 0, None);

                let timeout = Duration::from_secs(10);
                for (name, res) in [
                    (
                        SERIAL_CHECKS[0],
                        serial_echo(serial_cart, &opts.port, timeout),
                    ),
                    (
                        SERIAL_CHECKS[1],
                        serial_framing(serial_cart, &opts.port, timeout),
                    ),
                    (
                        SERIAL_CHECKS[2],
                        serial_framing_lockstep(serial_cart, &opts.port, timeout),
                    ),
                ] {
                    match res {
                        Ok(note) => r.pass(name, 0, Some(note)),
                        Err(e) => r.fail(name, e, 0),
                    }
                }

                match post_text(&opts.base_url, "/v1/serial/resume", Duration::from_secs(15)) {
                    Ok(_) => {
                        guard.armed = false;
                        r.pass("daemon resumed the serial port", 0, None);
                    }
                    Err(e) => r.fail("daemon resumed the serial port", e, 0),
                }

                // A 200 from resume means the handle reopened, not that the cart is back. Prove it.
                let root =
                    get_text(&opts.base_url, "/", Duration::from_secs(3)).unwrap_or_default();
                if json_field(&root, "serialActive").as_deref() == Some("true") {
                    r.pass("link is back up after resume", 0, None);
                } else {
                    r.fail(
                        "link is back up after resume",
                        "serialActive:false — restart multi64d".into(),
                        0,
                    );
                }

                // Back to M64T_PROTO before pinging: the cart is still in RAW_ECHO, which echoes a
                // ping rather than answering it.
                r.set_mode(1, "M64T_PROTO").await;
                r.check(
                    "cart still answers through the daemon",
                    ConnectorCommand::Ping,
                )
                .await;
                let failures_after = r.read_tx_failures().await;
                let outcome = write_failures_outcome(failures_before, failures_after);
                r.emit(SERIAL_CHECKS[3], outcome, 0, None);
            }
        }
    }

    // --- stream health -----------------------------------------------------------
    r.phase = PHASES[6].into();
    // Counters reset on the last mode change, so this covers everything since then. Each check
    // above proves its own round trip; only this proves nothing desynchronised underneath.
    r.check_noting(
        "no overflow, resync or bad headers since the last mode change",
        ConnectorCommand::Diag { expect_clean: true },
        diag_note,
    )
    .await;

    Ok(r.summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_fails_the_run_only_when_something_failed() {
        assert_eq!(
            SuiteSummary {
                passed: 30,
                failed: 0,
                skipped: 1
            }
            .exit_code(),
            0
        );
        assert_eq!(
            SuiteSummary {
                passed: 30,
                failed: 1,
                skipped: 0
            }
            .exit_code(),
            1
        );
    }

    #[test]
    fn http_urls_accept_a_bare_host_and_a_full_one() {
        assert_eq!(http_url("127.0.0.1:38765", "/"), "http://127.0.0.1:38765/");
        assert_eq!(
            http_url("http://127.0.0.1:38765/", "/health"),
            "http://127.0.0.1:38765/health"
        );
    }

    /// The daemon's root mixes snake and camel case, and `serialActive` is a bare boolean rather
    /// than a string — reading it as one would silently never match.
    #[test]
    fn json_fields_come_back_whether_string_or_bool() {
        let body = r#"{"service":"multi64d","serial":"COM4","serialActive":true,"cart":"sc64"}"#;
        assert_eq!(json_field(body, "serial").as_deref(), Some("COM4"));
        assert_eq!(json_field(body, "serialActive").as_deref(), Some("true"));
        assert_eq!(json_field(body, "cart").as_deref(), Some("sc64"));
        assert_eq!(json_field(body, "missing"), None);
        assert_eq!(json_field("not json", "serial"), None);
    }
    #[test]
    fn the_direct_serial_checks_speak_the_daemons_cart() {
        assert_eq!(SerialCart::from_daemon("sc64"), Some(SerialCart::Sc64));
        assert_eq!(SerialCart::from_daemon("ed64"), Some(SerialCart::Ed64));
        assert_eq!(
            SerialCart::from_daemon("ed64pro"),
            Some(SerialCart::Ed64Pro)
        );
        assert_eq!(SerialCart::from_daemon("?"), None);
    }
    /// Echoes like RAW_ECHO, but cannot send while the host is sending: a message that arrives
    /// while an earlier echo is still unread breaks the stream, which is what a truncated X7 write
    /// looks like from the host.
    #[derive(Default)]
    struct HalfDuplexEcho {
        unread: std::collections::VecDeque<u8>,
        broken: bool,
    }

    impl L3Link for HalfDuplexEcho {
        fn write_l3_stream(&mut self, buf: &[u8]) -> std::io::Result<()> {
            // One message per DEFAULT_ED64_CHUNK, as the X7 pipe sends them.
            for msg in buf.chunks(DEFAULT_ED64_CHUNK) {
                if !self.unread.is_empty() {
                    self.broken = true;
                }
                self.unread.extend(msg);
            }
            Ok(())
        }

        fn read_l3_bytes_exact(&mut self, out: &mut [u8], _: Duration) -> std::io::Result<()> {
            if self.broken {
                return Err(std::io::Error::other("ED64: expected CMPH trailer"));
            }
            if self.unread.len() < out.len() {
                return Err(std::io::Error::other("deadline exceeded"));
            }
            for b in out.iter_mut() {
                *b = self.unread.pop_front().expect("length checked");
            }
            Ok(())
        }
    }

    #[test]
    fn a_cart_that_cannot_send_while_receiving_fails_the_burst_check() {
        let err = framing_burst(&mut HalfDuplexEcho::default(), Duration::ZERO).unwrap_err();
        assert!(err.starts_with("large DATA"), "{err}");
    }

    #[test]
    fn the_lockstep_check_passes_on_a_cart_that_cannot_send_while_receiving() {
        let note = framing_lockstep(&mut HalfDuplexEcho::default(), Duration::ZERO).unwrap();
        assert!(note.contains("17 messages"), "{note}");
    }
    #[test]
    fn the_write_failure_count_is_read_from_the_diag_line() {
        let out = "sent M64T REQ_DIAG
DIAG mode=M64T_PROTO cart=EverDrive X-series frames=1 rx=42B tx=0B overflow=0 resync=0B bad_header=0 scratch=0x0002F3D0+256 tx_failures=7
OK: DIAG read
";
        assert_eq!(tx_failures_in(out), Some(7));
        let v1 = "DIAG mode=M64T_PROTO cart=SummerCart64 frames=1 rx=42B tx=0B overflow=0 resync=0B bad_header=0 scratch=0x0002F3D0+256
";
        assert_eq!(tx_failures_in(v1), None);
    }

    #[test]
    fn writes_that_gave_up_during_the_serial_checks_fail_the_run() {
        assert_eq!(write_failures_outcome(Some(2), Some(2)), Outcome::Pass);
        match write_failures_outcome(Some(2), Some(5)) {
            Outcome::Fail { detail } => assert!(detail.starts_with("3 "), "{detail}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// An old ROM, or a DIAG that could not be read, is not a pass: nothing was measured.
    #[test]
    fn a_count_that_could_not_be_read_is_skipped_not_passed() {
        for (before, after) in [(None, Some(1)), (Some(1), None), (None, None)] {
            assert!(
                matches!(write_failures_outcome(before, after), Outcome::Skip { .. }),
                "{before:?} -> {after:?}"
            );
        }
    }
}
