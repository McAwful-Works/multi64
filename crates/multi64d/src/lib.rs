//! Reference **multi64d** server: HTTP metadata, health check, and a **WebSocket** that carries the bidirectional **L3 octet stream** to/from the flash cart.
//!
//! - **`GET /`** — metadata, including whether the daemon holds the serial link (`serialActive`). Waits at most [`ROOT_LOCK_TIMEOUT`] for the link; one still in use after that is reported as held and busy (`serialBusy`) rather than waited out.
//! - **`POST /v1/serial/release`** — drop the serial link so another process (e.g. Xfer64) can open the COM port. WebSocket writes are ignored while released. If the link is still in use after [`RELEASE_LOCK_TIMEOUT`] (a long write, a slow reopen), answers **`503`** and does not release, then or later.
//! - **`POST /v1/serial/resume`** — reopen the same serial device and continue serving. If the link is still in use after [`RESUME_LOCK_TIMEOUT`], answers **`503`** and does not resume, then or later.
//!
//! Full API details: **`docs/spec/daemon-api-v1.md`**.

pub mod config;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use multi64_ed64_l2::Ed64L2Pipe;
use multi64_ed64pro_l2::Ed64ProL2Pipe;
use multi64_sc64_l2::Sc64L2Pipe;
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, MutexGuard};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

/// Serial read timeout. Bounds how long the cart reader holds the link mutex per iteration.
pub const SERIAL_READ_TIMEOUT: Duration = Duration::from_millis(50);

/// Serial timeout while writing a WebSocket message to the cart. The serial port has one timeout
/// for both directions, so writes swap this in and restore [`SERIAL_READ_TIMEOUT`] afterwards;
/// otherwise a device that took longer than 50 ms to accept a packet would get half of it.
///
/// Governs SC64 and X7 writes only. `Ed64ProL2Pipe::set_timeout` sets its own read polling and
/// never the port, so a PRO write keeps the 2 s operation timeout its link set at open; the fault
/// handling in [`write_to_link`] is the same for every cart.
pub const SERIAL_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Longest `POST /v1/serial/release` waits for the link lock before answering `503` without
/// releasing (`docs/spec/daemon-api-v1.md` §1.2). Well under Xfer64's 5 s request timeout, so the
/// caller hears the refusal instead of timing out while the release is still queued.
pub const RELEASE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest `POST /v1/serial/resume` waits for the link lock before answering `503` without
/// resuming (`docs/spec/daemon-api-v1.md` §1.2). The reopen that follows is not bounded by this;
/// the two together stay well under Xfer64's 10 s request timeout.
pub const RESUME_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest `GET /` waits for the link lock before reporting the link busy (`serialBusy`,
/// `docs/spec/daemon-api-v1.md` §1.1). Outlasts a reader iteration (50 ms) and is well under the
/// 2 s Xfer64 gives this request.
pub const ROOT_LOCK_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait before retrying `open` after the link faulted (cart unplugged, device reset).
const REOPEN_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Ceiling on the EverDrive-64 PRO's reopen backoff; see [`reopen_retry_interval`].
const PRO_REOPEN_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Reader buffer size. One allocation for the life of the loop; see `cart_reader_loop`.
const READ_BUF_BYTES: usize = 65536;

/// Which flash cart's L2 mapping carries the L3 stream (`--cart` / `MULTI64D_CART` / `cart = ...`).
///
/// [`CartKind::Ed64`] selects the EverDrive-64 X7 `DMA@` mapping from
/// `docs/spec/l3-over-everdrive-x7.md` §4. It is **experimental**: transcribed from UNFLoader and
/// libdragon, and run on one X7 so far. Selecting it here is wiring only.
///
/// [`CartKind::Ed64Pro`] selects the EverDrive-64 PRO mapping from
/// `docs/spec/l3-over-everdrive-pro.md`: L3 octets written to the cart FIFO and read back raw. It
/// is **experimental** in the same way, and more so: no reference host or ROM carries a byte stream
/// over the PRO at all, so the mapping is this repository's own design on top of Krikzz's sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum CartKind {
    /// SummerCart64 (`docs/spec/l3-over-sc64.md`); the only backend verified on hardware.
    #[default]
    Sc64,
    /// EverDrive-64 X7 (experimental; run on one cart so far).
    Ed64,
    /// EverDrive-64 PRO (experimental; never run against a cart).
    #[value(name = "ed64pro")]
    Ed64Pro,
}

impl CartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CartKind::Sc64 => "sc64",
            CartKind::Ed64 => "ed64",
            CartKind::Ed64Pro => "ed64pro",
        }
    }
}

impl std::fmt::Display for CartKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration needed to open — and later reopen — the serial port.
#[derive(Clone)]
pub struct SerialConfig {
    pub path: String,
    pub baud: u32,
    pub clear_serial: bool,
    pub cart: CartKind,
}

/// An open L2 pipe for whichever cart [`SerialConfig::cart`] selects. Every pipe exposes the same
/// surface, so everything above this sees one L3 octet stream regardless of the cart.
pub enum CartPipe {
    Sc64(Sc64L2Pipe),
    Ed64(Ed64L2Pipe),
    Ed64Pro(Ed64ProL2Pipe),
    /// Scripted pipe for the unit tests, which have no cart.
    #[cfg(test)]
    Fake(tests::FakePipe),
}

impl CartPipe {
    pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
        match self {
            CartPipe::Sc64(p) => p.set_timeout(t),
            CartPipe::Ed64(p) => p.set_timeout(t),
            CartPipe::Ed64Pro(p) => p.set_timeout(t),
            #[cfg(test)]
            CartPipe::Fake(p) => p.set_timeout(t),
        }
    }

    pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
        match self {
            CartPipe::Sc64(p) => p.clear_serial_buffers(),
            CartPipe::Ed64(p) => p.clear_serial_buffers(),
            CartPipe::Ed64Pro(p) => p.clear_serial_buffers(),
            #[cfg(test)]
            CartPipe::Fake(p) => p.clear_serial_buffers(),
        }
    }

    pub fn write_l3_stream(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            CartPipe::Sc64(p) => p.write_l3_stream(buf),
            CartPipe::Ed64(p) => p.write_l3_stream(buf),
            CartPipe::Ed64Pro(p) => p.write_l3_stream(buf),
            #[cfg(test)]
            CartPipe::Fake(p) => p.write_l3_stream(buf),
        }
    }

    pub fn read_l3_bytes(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            CartPipe::Sc64(p) => p.read_l3_bytes(out),
            CartPipe::Ed64(p) => p.read_l3_bytes(out),
            CartPipe::Ed64Pro(p) => p.read_l3_bytes(out),
            #[cfg(test)]
            CartPipe::Fake(p) => p.read_l3_bytes(out),
        }
    }
}

/// Open the cart serial port and apply [`SerialConfig`]. Used at startup, by
/// `POST /v1/serial/resume`, and by the reader loop when recovering from [`LinkState::Faulted`].
pub fn open_pipe(cfg: &SerialConfig) -> anyhow::Result<CartPipe> {
    let mut pipe = match cfg.cart {
        CartKind::Sc64 => CartPipe::Sc64(Sc64L2Pipe::open(&cfg.path, cfg.baud)?),
        CartKind::Ed64 => CartPipe::Ed64(Ed64L2Pipe::open(&cfg.path, cfg.baud)?),
        // The PRO runs at its own fixed 921600 baud (ed64-pro-usb-host.md §2); `--baud` does not
        // apply to it.
        CartKind::Ed64Pro => CartPipe::Ed64Pro(Ed64ProL2Pipe::open(&cfg.path)?),
    };
    pipe.set_timeout(SERIAL_READ_TIMEOUT)?;
    if cfg.clear_serial {
        pipe.clear_serial_buffers()?;
    }
    Ok(pipe)
}

/// Cart link state: an open L2 pipe, or one of two distinct down states.
pub enum LinkState {
    Active(CartPipe),
    /// Deliberately released by `POST /v1/serial/release` so another process (Xfer64) can open the
    /// port. Never reopened on its own — only `POST /v1/serial/resume` leaves this state.
    Released,
    /// The link failed on I/O (cart unplugged, USB-CDC reset). The dead pipe has been dropped, so
    /// the reader loop retries `open` and `POST /v1/serial/resume` can reopen it.
    Faulted,
}

/// Take the link lock from blocking code (`spawn_blocking`, plain threads); panics on a runtime
/// worker.
///
/// The lock is tokio's rather than std's so that `GET /`, `POST /v1/serial/release` and
/// `POST /v1/serial/resume` can wait for it asynchronously with a deadline: waiters are served in arrival order, and one that gives up
/// leaves the queue instead of keeping a thread parked on the lock. It does not poison either, so
/// one panic under the lock cannot wedge every later request.
fn lock_link(link: &Mutex<LinkState>) -> MutexGuard<'_, LinkState> {
    link.blocking_lock()
}

/// Shared Axum state: serial link (optional when released) and broadcast of cart-originated L3 chunks.
#[derive(Clone)]
pub struct AppState {
    pub serial_cfg: SerialConfig,
    pub link: Arc<Mutex<LinkState>>,
    pub from_cart: broadcast::Sender<Vec<u8>>,
    /// Browser origins allowed to reach the daemon; see [`origin_guard`].
    pub allowed_origins: Arc<Vec<String>>,
}

impl AppState {
    pub fn new(
        serial_cfg: SerialConfig,
        link: LinkState,
        from_cart: broadcast::Sender<Vec<u8>>,
        allowed_origins: Vec<String>,
    ) -> Self {
        Self {
            serial_cfg,
            link: Arc::new(Mutex::new(link)),
            from_cart,
            allowed_origins: Arc::new(allowed_origins),
        }
    }
}

#[derive(Serialize)]
struct RootResponse<'a> {
    service: &'a str,
    version: &'a str,
    websocket_path: &'a str,
    /// Serial device in use when the link is active (same string as `--serial` / config).
    serial: String,
    /// `false` when the COM port has been released (e.g. Xfer64) or the link faulted — host may
    /// open the port.
    #[serde(rename = "serialActive")]
    serial_active: bool,
    /// `true` when the link was still in use after [`ROOT_LOCK_TIMEOUT`], so its state could not be
    /// read; `serialActive` is then `true`.
    #[serde(rename = "serialBusy")]
    serial_busy: bool,
    /// The `--cart` mapping the daemon was started for (`sc64`, `ed64`, `ed64pro`), so a tool that
    /// shares the cart (Xfer64) need not probe ports to learn it.
    cart: &'a str,
}

/// Full HTTP + WebSocket [`Router`] including `/ws`.
pub fn build_app(state: Arc<AppState>) -> Router {
    let cors = cors_layer(&state.allowed_origins);
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/ws", get(ws_upgrade))
        .route("/v1/serial/release", post(post_serial_release))
        .route("/v1/serial/resume", post(post_serial_resume))
        .layer(middleware::from_fn_with_state(state.clone(), origin_guard))
        .with_state(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

/// Reject browser requests from origins that were not explicitly allowed.
///
/// The daemon writes straight to flash-cart hardware and binds loopback by default, so it is
/// reachable from any page the user happens to have open. A WebSocket upgrade is **not** subject to
/// the CORS response gate — the browser sends the handshake and expects the server to validate
/// `Origin` — so `/ws` has to be checked here; [`CorsLayer`] alone would leave it open. A plain
/// `POST /v1/serial/release` is likewise a CORS *simple* request: no preflight, response unreadable
/// by the page, but the side effect still lands.
///
/// Requests with **no** `Origin` header pass: every in-repo client (Xfer64's `ureq`, Multi64,
/// `multi64-test-connector`'s `tokio-tungstenite`) is a native Rust process and sends none, while a
/// browser always does. The allow-list is empty by default; add entries with `--allow-origin` /
/// `MULTI64D_ALLOW_ORIGIN` / `allow_origin` in the config file.
async fn origin_guard(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let Some(origin) = req.headers().get(header::ORIGIN).cloned() else {
        return next.run(req).await;
    };
    let allowed = origin
        .to_str()
        .is_ok_and(|o| state.allowed_origins.iter().any(|a| a == o));
    if allowed {
        return next.run(req).await;
    }
    tracing::warn!(
        origin = ?origin,
        path = %req.uri().path(),
        "rejected cross-origin request (allow it with --allow-origin)"
    );
    (
        StatusCode::FORBIDDEN,
        "origin not allowed; start multi64d with --allow-origin <ORIGIN> to permit it\n",
    )
        .into_response()
}

/// CORS for the allow-listed origins only. With an empty list no `Access-Control-Allow-Origin`
/// header is emitted at all, so a browser cannot read any response — matching [`origin_guard`],
/// which rejects those requests outright.
fn cors_layer(allowed: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = allowed
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    if origins.is_empty() {
        return CorsLayer::new();
    }
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(Any)
}

async fn root(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Configured serial device (same when link is temporarily released for Xfer64).
    let serial = state.serial_cfg.path.clone();
    // The link lock is held across blocking serial I/O -- for the whole of each WebSocket message
    // written to the cart, and while a link is reopened -- so wait for it only briefly. Xfer64
    // polls this route before every cart operation and gives it 2 s. The wait parks no thread, and
    // one that runs out leaves the lock's queue.
    let (serial_active, serial_busy) =
        match tokio::time::timeout(ROOT_LOCK_TIMEOUT, state.link.lock()).await {
            Ok(link) => (matches!(&*link, LinkState::Active(_)), false),
            // Whatever holds the link is using the port or opening it, so report it held: a client
            // then releases before opening the port, and hears 503 if the link is still busy.
            Err(_) => (true, true),
        };
    let body = serde_json::to_vec(&RootResponse {
        service: "multi64d",
        version: env!("CARGO_PKG_VERSION"),
        websocket_path: "/ws",
        serial,
        serial_active,
        serial_busy,
        cart: state.serial_cfg.cart.as_str(),
    })
    .unwrap_or_else(|_| b"{}".to_vec());
    ([(header::CONTENT_TYPE, "application/json")], body)
}

async fn health() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"status":"ok"}"#,
    )
}

async fn post_serial_release(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Queued behind whatever holds the link, in arrival order. A wait that runs out leaves the
    // queue, so a release that answered 503 holds no thread and can never apply later.
    let Ok(mut link) =
        tokio::time::timeout(RELEASE_LOCK_TIMEOUT, state.link.clone().lock_owned()).await
    else {
        tracing::warn!(
            timeout = ?RELEASE_LOCK_TIMEOUT,
            "serial release refused: link busy"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "serial link busy; not released, try again\n",
        )
            .into_response();
    };
    // Closing the COM port can block, so drop the pipe off the runtime. Assigning over the guard
    // drops it -- and closes the port -- before the lock is released, so the caller cannot see 200
    // while the handle is still open.
    match tokio::task::spawn_blocking(move || *link = LinkState::Released).await {
        Ok(()) => (
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"released":true}"#.as_bytes().to_vec(),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

fn resumed_response() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"resumed":true}"#.as_bytes().to_vec(),
    )
        .into_response()
}

async fn post_serial_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Bounded like release: a wait that runs out leaves the lock's queue, so a resume that answered
    // 503 holds no thread and never reopens the port later.
    let Ok(mut link) =
        tokio::time::timeout(RESUME_LOCK_TIMEOUT, state.link.clone().lock_owned()).await
    else {
        tracing::warn!(
            timeout = ?RESUME_LOCK_TIMEOUT,
            "serial resume refused: link busy"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "serial link busy; not resumed, try again\n",
        )
            .into_response();
    };
    if matches!(&*link, LinkState::Active(_)) {
        // Idempotent: Xfer64 may call resume in nested `withCartDaemonYield` (e.g. copy then
        // refresh list). A second `open_pipe` would fail with "Access denied" while the first
        // handle is still active. A link that failed on I/O is `Faulted`, not `Active`, so this
        // early return never hides a dead pipe.
        return resumed_response();
    }
    let cfg = state.serial_cfg.clone();
    // Opening the port blocks (for the PRO, a whole handshake), so do it off the runtime. The
    // guard moves with it, so nothing else sees the link until the open has succeeded or failed.
    let res = tokio::task::spawn_blocking(move || {
        *link = LinkState::Active(open_pipe(&cfg).map_err(|e| e.to_string())?);
        Ok::<_, String>(())
    })
    .await;
    match res {
        Ok(Ok(())) => resumed_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

enum ReadChunk {
    Data(Vec<u8>),
    /// Link is up but had nothing queued this iteration.
    Empty,
    /// Link is down deliberately (`Released`); poll again shortly so resume is picked up promptly.
    Released,
    /// Link is faulted and `open` just failed; back off before the next attempt. `reached_device`
    /// is [`open_reached_device`] of the failure.
    RetryOpen {
        reached_device: bool,
    },
    /// Link is faulted, but the reopen backoff has not run out, so nothing was attempted.
    BackingOff,
}

/// How long the reader loop waits between attempts to reopen a faulted link, after
/// `handshake_failures` consecutive attempts that opened the port but failed on the device.
///
/// A fixed [`REOPEN_RETRY_INTERVAL`] for every cart except the EverDrive-64 PRO. Its `open` runs the
/// edlink handshake, written to whatever answers on the port, so a daemon pointed at the wrong
/// device would otherwise send it every second, under the link lock, for as long as it runs. From
/// the second such failure in a row the PRO's interval doubles, up to [`PRO_REOPEN_BACKOFF_MAX`].
fn reopen_retry_interval(cart: CartKind, handshake_failures: u32) -> Duration {
    if cart != CartKind::Ed64Pro || handshake_failures < 2 {
        return REOPEN_RETRY_INTERVAL;
    }
    let factor = 1u32.checked_shl(handshake_failures - 1).unwrap_or(u32::MAX);
    REOPEN_RETRY_INTERVAL
        .checked_mul(factor)
        .map_or(PRO_REOPEN_BACKOFF_MAX, |d| d.min(PRO_REOPEN_BACKOFF_MAX))
}

/// The reader loop's reopen schedule: [`reopen_retry_interval`] over consecutive failures.
#[derive(Debug, Default)]
struct ReopenBackoff {
    handshake_failures: u32,
    /// No attempt before this; `None` when the loop's own [`REOPEN_RETRY_INTERVAL`] sleep is wait
    /// enough.
    not_before: Option<Instant>,
}

impl ReopenBackoff {
    /// Back to the normal interval: the link is up again, or was released for another tool.
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn may_attempt(&self, now: Instant) -> bool {
        !matches!(self.not_before, Some(t) if now < t)
    }

    /// Record a reopen that failed at `now`. A port that did not open at all sent nothing, so it
    /// ends the run of handshake failures rather than extending it.
    fn failed(&mut self, cart: CartKind, reached_device: bool, now: Instant) {
        self.handshake_failures = if reached_device {
            self.handshake_failures.saturating_add(1)
        } else {
            0
        };
        let wait = reopen_retry_interval(cart, self.handshake_failures);
        self.not_before = (wait > REOPEN_RETRY_INTERVAL).then(|| now + wait);
    }
}

/// Whether a failed [`open_pipe`] opened the port and failed afterwards (handshake, identity or
/// setup), as opposed to not opening the port at all. Only the former wrote to a device.
fn open_reached_device(err: &anyhow::Error) -> bool {
    let port_did_not_open = err.chain().any(|e| {
        e.is::<serialport::Error>()
            || e.downcast_ref::<io::Error>()
                .and_then(|io| io.get_ref())
                .is_some_and(|inner| inner.is::<serialport::Error>())
    });
    !port_did_not_open
}

/// Background task: read L3 chunks from the cart and broadcast them to WebSocket clients.
///
/// Also owns fault recovery. A hard `io::Error` from the pipe (as opposed to a timeout, which
/// `read_l3_bytes` reports as `Ok(0)`) moves the link to [`LinkState::Faulted`] and drops the dead
/// handle, so `GET /` stops claiming `serialActive` and the port becomes reopenable. The loop then
/// retries `open` every [`REOPEN_RETRY_INTERVAL`] until the cart comes back, backing off for an
/// EverDrive-64 PRO that keeps failing its handshake ([`reopen_retry_interval`]).
/// `POST /v1/serial/resume` does not wait for that backoff.
pub async fn cart_reader_loop(state: Arc<AppState>) {
    // Allocated once and passed back and forth with the blocking task. Declaring it inside the
    // loop cost a 64 KiB allocation *and* a 64 KiB zero-fill every iteration -- about 20 times a
    // second while idle, for the whole life of a daemon that starts at login -- to produce bytes
    // `read_l3_bytes` immediately overwrites.
    let mut buf = vec![0u8; READ_BUF_BYTES];
    let mut backoff = ReopenBackoff::default();

    loop {
        let may_reopen = backoff.may_attempt(Instant::now());
        let handoff = tokio::task::spawn_blocking({
            let link = state.link.clone();
            let cfg = state.serial_cfg.clone();
            move || {
                let r = read_once(&link, &cfg, &mut buf, may_reopen);
                // Hand the buffer back so the next iteration reuses this allocation.
                (r, buf)
            }
        })
        .await;

        let chunk = match handoff {
            Ok((r, returned)) => {
                buf = returned;
                Ok(r)
            }
            // A panic in the blocking task takes the buffer with it; allocate a fresh one rather
            // than ending the loop.
            Err(e) => {
                buf = vec![0u8; READ_BUF_BYTES];
                Err(e)
            }
        };

        match chunk {
            Ok(Ok(ReadChunk::Data(data))) => {
                backoff.reset();
                let _ = state.from_cart.send(data);
            }
            Ok(Ok(ReadChunk::Empty)) => {
                backoff.reset();
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok(Ok(ReadChunk::Released)) => {
                backoff.reset();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(Ok(ReadChunk::RetryOpen { reached_device })) => {
                backoff.failed(state.serial_cfg.cart, reached_device, Instant::now());
                tokio::time::sleep(REOPEN_RETRY_INTERVAL).await;
            }
            // Sleep in the same slices as a failed attempt, so a resume or release during a long
            // backoff is picked up just as promptly.
            Ok(Ok(ReadChunk::BackingOff)) => {
                tokio::time::sleep(REOPEN_RETRY_INTERVAL).await;
            }
            // `read_once` has already moved the link to `Faulted`; the next pass reopens it.
            Ok(Err(e)) => {
                tracing::error!(error = %e, "read from cart; dropping serial link");
            }
            Err(e) => {
                tracing::error!(error = %e, "cart reader join");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// One pass over the link: reopen it if faulted (and `may_reopen`), otherwise read whatever is
/// queued into `buf`. A read error faults the link before the lock is released, so no release or
/// resume can land between the failure and the fault.
fn read_once(
    link: &Arc<Mutex<LinkState>>,
    cfg: &SerialConfig,
    buf: &mut [u8],
    may_reopen: bool,
) -> io::Result<ReadChunk> {
    let mut g = lock_link(link);
    match &mut *g {
        LinkState::Released => Ok(ReadChunk::Released),
        LinkState::Faulted if !may_reopen => Ok(ReadChunk::BackingOff),
        LinkState::Faulted => match open_pipe(cfg) {
            Ok(p) => {
                tracing::info!(serial = %cfg.path, "serial link reopened after fault");
                *g = LinkState::Active(p);
                Ok(ReadChunk::Empty)
            }
            Err(e) => {
                // Expected while the cart is unplugged; `debug` keeps it out of the default log
                // at one line per second.
                tracing::debug!(error = %e, serial = %cfg.path, "reopen serial link");
                Ok(ReadChunk::RetryOpen {
                    reached_device: open_reached_device(&e),
                })
            }
        },
        LinkState::Active(p) => match p.read_l3_bytes(buf) {
            Ok(0) => Ok(ReadChunk::Empty),
            // Right-size before this enters the broadcast: a 256-slot channel holding
            // full-capacity 64 KiB buffers would pin 16 MiB.
            Ok(n) => Ok(ReadChunk::Data(buf[..n].to_vec())),
            Err(e) => {
                // Dropping the dead pipe here frees the port for a reopen.
                *g = LinkState::Faulted;
                Err(e)
            }
        },
    }
}

/// Write one WebSocket message's L3 octets to the cart, or drop them while the link is down.
///
/// A write that fails or times out faults the link: an unknown part of the message has gone out,
/// and the cart would read whatever followed as the rest of it.
fn write_to_link(link: &Mutex<LinkState>, data: &[u8]) -> io::Result<()> {
    let mut g = lock_link(link);
    match &mut *g {
        LinkState::Released => {
            tracing::debug!("write to cart ignored (serial released)");
            Ok(())
        }
        LinkState::Faulted => {
            tracing::debug!("write to cart ignored (serial link down)");
            Ok(())
        }
        LinkState::Active(p) => match write_with_write_timeout(p, data) {
            Ok(()) => Ok(()),
            Err(e) => {
                if let Err(clear) = p.clear_serial_buffers() {
                    tracing::debug!(error = %clear, "clear serial buffers after failed write");
                }
                *g = LinkState::Faulted;
                Err(e)
            }
        },
    }
}

/// [`CartPipe::write_l3_stream`] under [`SERIAL_WRITE_TIMEOUT`], restoring the read timeout after.
fn write_with_write_timeout(p: &mut CartPipe, data: &[u8]) -> io::Result<()> {
    p.set_timeout(SERIAL_WRITE_TIMEOUT)?;
    p.write_l3_stream(data)?;
    p.set_timeout(SERIAL_READ_TIMEOUT)
}

async fn handle_ws(mut socket: WebSocket, state: Arc<AppState>) {
    let mut rx = state.from_cart.subscribe();

    let hello = serde_json::json!({
        "type": "hello",
        "service": "multi64d",
        "version": env!("CARGO_PKG_VERSION"),
        "docs": "docs/spec/daemon-api-v1.md",
    });
    if socket.send(Message::text(hello.to_string())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            msg = socket.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let link = state.link.clone();
                        let res = tokio::task::spawn_blocking(move || write_to_link(&link, &data)).await;
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => tracing::error!(error = %e, "write to cart; dropping serial link"),
                            Err(e) => tracing::error!(error = %e, "write join"),
                        }
                    }
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                            if v.get("type").and_then(|x| x.as_str()) == Some("ping") {
                                let _ = socket
                                    .send(Message::text(r#"{"type":"pong"}"#))
                                    .await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        let s = e.to_string();
                        if s.contains("Connection reset without closing handshake") {
                            tracing::debug!(error = %e, "websocket client disconnected (no close handshake)");
                        } else {
                            tracing::warn!(error = %e, "websocket");
                        }
                        break;
                    }
                    // Protocol-level `Ping`/`Pong` are answered by tungstenite underneath axum.
                    _ => {}
                }
            }
            recv = rx.recv() => {
                match recv {
                    Ok(data) => {
                        if socket.send(Message::binary(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "client lagged; dropping old cart data");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The link lock is tokio's; the fake's log is shared with plain test code.
    use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard};

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// What a [`FakePipe`] has been asked to do, shared with the test that scripted it.
    #[derive(Debug, Default)]
    pub struct FakeLog {
        /// Timeout most recently passed to `set_timeout`.
        pub timeout: Duration,
        /// Bytes that reached the "wire", including the front of a write that was cut off.
        pub written: Vec<u8>,
        pub clears: usize,
        /// A write shorter than this timeout is cut off halfway, as serialport does when a stalled
        /// device outlasts `WriteTotalTimeoutConstant` / `poll`.
        pub write_needs: Duration,
        /// `read_l3_bytes` fails with a hard I/O error, as for an unplugged cart.
        pub read_fails: bool,
    }

    pub struct FakePipe(pub Arc<StdMutex<FakeLog>>);

    impl FakePipe {
        fn log(&self) -> StdMutexGuard<'_, FakeLog> {
            self.0.lock().unwrap()
        }

        pub fn set_timeout(&mut self, t: Duration) -> io::Result<()> {
            self.log().timeout = t;
            Ok(())
        }

        pub fn clear_serial_buffers(&mut self) -> io::Result<()> {
            self.log().clears += 1;
            Ok(())
        }

        pub fn write_l3_stream(&mut self, buf: &[u8]) -> io::Result<()> {
            let mut log = self.log();
            if log.timeout < log.write_needs {
                log.written.extend_from_slice(&buf[..buf.len() / 2]);
                return Err(io::Error::new(io::ErrorKind::TimedOut, "write timed out"));
            }
            log.written.extend_from_slice(buf);
            Ok(())
        }

        pub fn read_l3_bytes(&mut self, _out: &mut [u8]) -> io::Result<usize> {
            if self.log().read_fails {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "device gone"));
            }
            Ok(0)
        }
    }

    /// An active link over a fake pipe, set up the way [`open_pipe`] leaves a real one.
    fn fake_link(log: FakeLog) -> (Arc<Mutex<LinkState>>, Arc<StdMutex<FakeLog>>) {
        let log = Arc::new(StdMutex::new(log));
        let mut pipe = CartPipe::Fake(FakePipe(log.clone()));
        pipe.set_timeout(SERIAL_READ_TIMEOUT).unwrap();
        (Arc::new(Mutex::new(LinkState::Active(pipe))), log)
    }

    fn state_name(link: &Mutex<LinkState>) -> &'static str {
        match &*lock_link(link) {
            LinkState::Active(_) => "active",
            LinkState::Released => "released",
            LinkState::Faulted => "faulted",
        }
    }

    fn test_cfg() -> SerialConfig {
        SerialConfig {
            path: "multi64d-no-such-port".into(),
            baud: 115_200,
            clear_serial: false,
            cart: CartKind::Sc64,
        }
    }

    #[test]
    fn a_failed_read_faults_the_link_before_the_lock_is_released() {
        // #139: the fault used to be recorded by a second lock acquisition, so a release + resume
        // in between got its fresh link faulted, and a resume alone was told the dead link was up.
        let (link, _log) = fake_link(FakeLog {
            read_fails: true,
            ..FakeLog::default()
        });
        let mut buf = [0u8; 16];
        assert!(read_once(&link, &test_cfg(), &mut buf, true).is_err());
        assert_eq!(state_name(&link), "faulted");
    }

    #[test]
    fn a_write_slower_than_the_read_timeout_is_not_cut_off() {
        // #138: the 50 ms read timeout used to govern writes too, so a device that took longer
        // than that to accept a packet got half of it.
        let (link, log) = fake_link(FakeLog {
            write_needs: Duration::from_millis(200),
            ..FakeLog::default()
        });
        let data: Vec<u8> = (0..=255).collect();
        write_to_link(&link, &data).expect("write within the write timeout succeeds");
        let log = log.lock().unwrap();
        assert_eq!(log.written, data, "the whole message reached the wire");
        assert_eq!(
            log.timeout, SERIAL_READ_TIMEOUT,
            "the reader gets its short timeout back"
        );
        drop(log);
        assert_eq!(state_name(&link), "active");
    }

    #[test]
    fn a_cut_off_write_faults_the_link_and_clears_its_buffers() {
        // #138: a partial write used to be logged and nothing else, leaving the cart to read the
        // next packet's header as payload.
        let (link, log) = fake_link(FakeLog {
            write_needs: Duration::MAX,
            ..FakeLog::default()
        });
        assert!(write_to_link(&link, &[0u8; 64]).is_err());
        assert_eq!(log.lock().unwrap().clears, 1, "buffers cleared");
        assert_eq!(state_name(&link), "faulted");
    }

    #[test]
    fn pro_reopen_interval_doubles_from_the_second_handshake_failure_up_to_30s() {
        let schedule: Vec<Duration> = (0..=8)
            .map(|n| reopen_retry_interval(CartKind::Ed64Pro, n))
            .collect();
        assert_eq!(schedule, [1, 1, 2, 4, 8, 16, 30, 30, 30].map(secs));
        assert_eq!(reopen_retry_interval(CartKind::Ed64Pro, 40), secs(30));
        assert_eq!(reopen_retry_interval(CartKind::Ed64Pro, u32::MAX), secs(30));
    }

    #[test]
    fn sc64_and_x7_reopen_every_second_however_often_they_fail() {
        for cart in [CartKind::Sc64, CartKind::Ed64] {
            for n in [0, 1, 2, 5, 40, u32::MAX] {
                assert_eq!(reopen_retry_interval(cart, n), REOPEN_RETRY_INTERVAL);
            }
        }
    }

    #[test]
    fn backoff_holds_off_after_repeated_handshake_failures_until_reset() {
        let t0 = Instant::now();
        let mut b = ReopenBackoff::default();
        assert!(b.may_attempt(t0));
        b.failed(CartKind::Ed64Pro, true, t0);
        assert!(
            b.may_attempt(t0),
            "one failure keeps the loop's own 1 s sleep"
        );
        for _ in 0..3 {
            b.failed(CartKind::Ed64Pro, true, t0);
        }
        assert!(
            !b.may_attempt(t0 + secs(7)),
            "four failures in a row wait 8 s"
        );
        assert!(b.may_attempt(t0 + secs(8)));

        b.reset();
        assert!(b.may_attempt(t0), "a successful open, or a release, resets");

        for _ in 0..4 {
            b.failed(CartKind::Ed64Pro, true, t0);
        }
        b.failed(CartKind::Ed64Pro, false, t0);
        assert!(b.may_attempt(t0), "a port that did not open sent nothing");

        let mut sc64 = ReopenBackoff::default();
        for _ in 0..10 {
            sc64.failed(CartKind::Sc64, true, t0);
        }
        assert!(sc64.may_attempt(t0));
    }

    #[test]
    fn only_a_port_that_opened_counts_as_reaching_the_device() {
        let no_port = anyhow::Error::from(io::Error::other(serialport::Error::new(
            serialport::ErrorKind::NoDevice,
            "gone",
        )));
        assert!(!open_reached_device(&no_port));
        let silent = anyhow::Error::from(io::Error::new(io::ErrorKind::TimedOut, "no answer"));
        assert!(open_reached_device(&silent));
        let wrong = anyhow::Error::from(io::Error::other("not an EverDrive-64 PRO"));
        assert!(open_reached_device(&wrong));

        // The real chain, through `Ed64ProL2Pipe::open`, for a port that is not there.
        let Err(missing) = open_pipe(&SerialConfig {
            path: "multi64d-no-such-port".into(),
            baud: 115_200,
            clear_serial: false,
            cart: CartKind::Ed64Pro,
        }) else {
            panic!("a missing port must not open");
        };
        assert!(!open_reached_device(&missing), "{missing:#}");
    }
}
