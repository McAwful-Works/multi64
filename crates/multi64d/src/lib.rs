//! Reference **multi64d** server: HTTP metadata, health check, and a **WebSocket** that carries the bidirectional **L3 octet stream** to/from the flash cart.
//!
//! - **`POST /v1/serial/release`** — drop the serial link so another process (e.g. Xfer64) can open the COM port. WebSocket writes are ignored while released.
//! - **`POST /v1/serial/resume`** — reopen the same serial device and continue serving.
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
use multi64_sc64_l2::Sc64L2Pipe;
use serde::Serialize;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

/// Serial read timeout. Bounds how long the cart reader holds the link mutex per iteration, so it
/// also bounds how long `POST /v1/serial/release` waits for its turn.
pub const SERIAL_READ_TIMEOUT: Duration = Duration::from_millis(50);

/// How long to wait before retrying `open` after the link faulted (cart unplugged, device reset).
const REOPEN_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Configuration needed to open — and later reopen — the serial port.
#[derive(Clone)]
pub struct SerialConfig {
    pub path: String,
    pub baud: u32,
    pub clear_serial: bool,
}

/// Open the cart serial port and apply [`SerialConfig`]. Used at startup, by
/// `POST /v1/serial/resume`, and by the reader loop when recovering from [`LinkState::Faulted`].
pub fn open_pipe(cfg: &SerialConfig) -> anyhow::Result<Sc64L2Pipe> {
    let mut pipe = Sc64L2Pipe::open(&cfg.path, cfg.baud)?;
    pipe.set_timeout(SERIAL_READ_TIMEOUT)?;
    if cfg.clear_serial {
        pipe.clear_serial_buffers()?;
    }
    Ok(pipe)
}

/// Cart link state: an open L2 pipe, or one of two distinct down states.
pub enum LinkState {
    Active(Sc64L2Pipe),
    /// Deliberately released by `POST /v1/serial/release` so another process (Xfer64) can open the
    /// port. Never reopened on its own — only `POST /v1/serial/resume` leaves this state.
    Released,
    /// The link failed on I/O (cart unplugged, USB-CDC reset). The dead pipe has been dropped, so
    /// the reader loop retries `open` and `POST /v1/serial/resume` can reopen it.
    Faulted,
}

/// `LinkState` holds no invariant that a panic can break, so recover the guard instead of
/// propagating poison: one panic under the lock would otherwise wedge every later request, and
/// `root` has no error channel to report it on.
fn lock_link(link: &Mutex<LinkState>) -> MutexGuard<'_, LinkState> {
    link.lock().unwrap_or_else(|e| e.into_inner())
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

async fn root_metadata_only() -> impl IntoResponse {
    let body = serde_json::to_vec(&RootResponse {
        service: "multi64d",
        version: env!("CARGO_PKG_VERSION"),
        websocket_path: "/ws",
        serial: String::new(),
        serial_active: false,
    })
    .unwrap_or_else(|_| b"{}".to_vec());
    ([(header::CONTENT_TYPE, "application/json")], body)
}

/// Minimal router: **`GET /`** and **`GET /health`** only (no [`AppState`], no serial — used in integration tests).
pub fn http_metadata_router() -> Router {
    Router::new()
        .route("/", get(root_metadata_only))
        .route("/health", get(health))
}

async fn root(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Configured serial device (same when link is temporarily released for Xfer64).
    let serial = state.serial_cfg.path.clone();
    // The link mutex is held across blocking serial I/O by the reader loop and by
    // `post_serial_resume`, so taking it here would park a runtime worker for that whole window.
    // Xfer64 polls this route before every cart operation.
    let link = state.link.clone();
    let serial_active =
        tokio::task::spawn_blocking(move || matches!(&*lock_link(&link), LinkState::Active(_)))
            .await
            .unwrap_or(false);
    let body = serde_json::to_vec(&RootResponse {
        service: "multi64d",
        version: env!("CARGO_PKG_VERSION"),
        websocket_path: "/ws",
        serial,
        serial_active,
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
    let link = state.link.clone();
    // Assigning over the guard drops the `Sc64L2Pipe` — and closes the COM port — before the lock
    // is released, so the caller cannot see 200 while the handle is still open.
    let res = tokio::task::spawn_blocking(move || *lock_link(&link) = LinkState::Released).await;
    match res {
        Ok(()) => (
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"released":true}"#.as_bytes().to_vec(),
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

async fn post_serial_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.serial_cfg.clone();
    let link = state.link.clone();
    let res = tokio::task::spawn_blocking(move || {
        let mut g = lock_link(&link);
        if matches!(&*g, LinkState::Active(_)) {
            // Idempotent: Xfer64 may call resume in nested `withCartDaemonYield` (e.g. copy
            // then refresh list). A second `Sc64L2Pipe::open` would fail with "Access denied" while
            // the first handle is still active. A link that failed on I/O is `Faulted`, not
            // `Active`, so this early return never hides a dead pipe.
            return Ok::<_, String>(());
        }
        *g = LinkState::Active(open_pipe(&cfg).map_err(|e| e.to_string())?);
        Ok(())
    })
    .await;
    match res {
        Ok(Ok(())) => (
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"resumed":true}"#.as_bytes().to_vec(),
        )
            .into_response(),
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
    /// Link is faulted and `open` just failed; back off before the next attempt.
    RetryOpen,
}

/// Background task: read L3 chunks from the cart and broadcast them to WebSocket clients.
///
/// Also owns fault recovery. A hard `io::Error` from the pipe (as opposed to a timeout, which
/// `read_l3_bytes` reports as `Ok(0)`) moves the link to [`LinkState::Faulted`] and drops the dead
/// handle, so `GET /` stops claiming `serialActive` and the port becomes reopenable. The loop then
/// retries `open` every [`REOPEN_RETRY_INTERVAL`] until the cart comes back.
pub async fn cart_reader_loop(state: Arc<AppState>) {
    loop {
        let chunk = tokio::task::spawn_blocking({
            let link = state.link.clone();
            let cfg = state.serial_cfg.clone();
            move || -> io::Result<ReadChunk> {
                let mut g = lock_link(&link);
                match &mut *g {
                    LinkState::Released => Ok(ReadChunk::Released),
                    LinkState::Faulted => match open_pipe(&cfg) {
                        Ok(p) => {
                            tracing::info!(serial = %cfg.path, "serial link reopened after fault");
                            *g = LinkState::Active(p);
                            Ok(ReadChunk::Empty)
                        }
                        Err(e) => {
                            // Expected while the cart is unplugged; `debug` keeps it out of the
                            // default log at one line per second.
                            tracing::debug!(error = %e, serial = %cfg.path, "reopen serial link");
                            Ok(ReadChunk::RetryOpen)
                        }
                    },
                    LinkState::Active(p) => {
                        let mut buf = vec![0u8; 65536];
                        let n = p.read_l3_bytes(&mut buf)?;
                        if n == 0 {
                            Ok(ReadChunk::Empty)
                        } else {
                            Ok(ReadChunk::Data(buf[..n].to_vec()))
                        }
                    }
                }
            }
        })
        .await;

        match chunk {
            Ok(Ok(ReadChunk::Data(data))) => {
                let _ = state.from_cart.send(data);
            }
            Ok(Ok(ReadChunk::Empty)) => {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok(Ok(ReadChunk::Released)) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(Ok(ReadChunk::RetryOpen)) => {
                tokio::time::sleep(REOPEN_RETRY_INTERVAL).await;
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "read from cart; dropping serial link");
                mark_faulted(&state.link).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cart reader join");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Drop a dead pipe and mark the link faulted, unless it was released in the meantime — a
/// concurrent `POST /v1/serial/release` must win, or the loop would reopen a port Xfer64 wants.
async fn mark_faulted(link: &Arc<Mutex<LinkState>>) {
    let link = link.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let mut g = lock_link(&link);
        if matches!(&*g, LinkState::Active(_)) {
            *g = LinkState::Faulted;
        }
    })
    .await;
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
                        let res = tokio::task::spawn_blocking(move || {
                            let mut g = lock_link(&link);
                            match &mut *g {
                                LinkState::Released => {
                                    tracing::debug!("write to cart ignored (serial released)");
                                    Ok(())
                                }
                                LinkState::Faulted => {
                                    tracing::debug!("write to cart ignored (serial link down)");
                                    Ok(())
                                }
                                LinkState::Active(p) => p.write_l3_stream(&data),
                            }
                        }).await;
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => tracing::warn!(error = %e, "write to cart"),
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
