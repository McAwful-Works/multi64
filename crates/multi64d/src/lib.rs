//! Reference **multi64d** server: HTTP metadata, health check, and a **WebSocket** that carries the bidirectional **L3 octet stream** to/from the flash cart.
//!
//! - **`POST /v1/serial/release`** — drop the serial link so another process (e.g. Xfer64) can open the COM port. WebSocket writes are ignored while released.
//! - **`POST /v1/serial/resume`** — reopen the same serial device and continue serving.
//!
//! Full API details: **`docs/spec/daemon-api-v1.md`**.

pub mod config;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use multi64_sc64_l2::Sc64L2Pipe;
use serde::Serialize;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

/// Configuration needed to reopen the serial port after [`LinkState::Released`].
#[derive(Clone)]
pub struct SerialConfig {
    pub path: String,
    pub baud: u32,
    pub clear_serial: bool,
}

/// Cart link state: active L2 pipe or released (COM closed for external tools).
pub enum LinkState {
    Active(Sc64L2Pipe),
    Released,
}

/// Shared Axum state: serial link (optional when released) and broadcast of cart-originated L3 chunks.
#[derive(Clone)]
pub struct AppState {
    pub serial_cfg: SerialConfig,
    pub link: Arc<Mutex<LinkState>>,
    pub from_cart: broadcast::Sender<Vec<u8>>,
}

impl AppState {
    pub fn new(
        serial_cfg: SerialConfig,
        pipe: Sc64L2Pipe,
        from_cart: broadcast::Sender<Vec<u8>>,
    ) -> Self {
        let link = Arc::new(Mutex::new(LinkState::Active(pipe)));
        Self {
            serial_cfg,
            link,
            from_cart,
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
    /// `false` when the COM port has been released (e.g. Xfer64) — host may open the port.
    #[serde(rename = "serialActive")]
    serial_active: bool,
}

/// Full HTTP + WebSocket [`Router`] including `/ws` (requires a real [`AppState`] with an open serial port).
pub fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/ws", get(ws_upgrade))
        .route("/v1/serial/release", post(post_serial_release))
        .route("/v1/serial/resume", post(post_serial_resume))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
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
    let serial_active = matches!(&*state.link.lock().unwrap(), LinkState::Active(_));
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
    let res = tokio::task::spawn_blocking(move || {
        let mut g = link.lock().map_err(|e| e.to_string())?;
        *g = LinkState::Released;
        Ok::<_, String>(())
    })
    .await;
    match res {
        Ok(Ok(())) => (
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"released":true}"#.as_bytes().to_vec(),
        )
            .into_response(),
        Ok(Err(e)) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("join: {e}"),
        )
            .into_response(),
    }
}

async fn post_serial_resume(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cfg = state.serial_cfg.clone();
    let link = state.link.clone();
    let res = tokio::task::spawn_blocking(move || {
        let mut g = link.lock().map_err(|e| e.to_string())?;
        if matches!(&*g, LinkState::Active(_)) {
            // Idempotent: Xfer64 may call resume in nested `withCartDaemonYield` (e.g. copy
            // then refresh list). A second `Sc64L2Pipe::open` would fail with "Access denied" while
            // the first handle is still active.
            return Ok::<_, String>(());
        }
        let mut pipe = Sc64L2Pipe::open(&cfg.path, cfg.baud).map_err(|e| e.to_string())?;
        pipe.set_timeout(Duration::from_millis(50))
            .map_err(|e| e.to_string())?;
        if cfg.clear_serial {
            pipe.clear_serial_buffers().map_err(|e| e.to_string())?;
        }
        *g = LinkState::Active(pipe);
        Ok::<_, String>(())
    })
    .await;
    match res {
        Ok(Ok(())) => (
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"resumed":true}"#.as_bytes().to_vec(),
        )
            .into_response(),
        Ok(Err(e)) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("join: {e}"),
        )
            .into_response(),
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

enum ReadChunk {
    Data(Vec<u8>),
    Empty,
    Released,
}

/// Background task: read L3 chunks from the cart and broadcast them to WebSocket clients.
pub async fn cart_reader_loop(link: Arc<Mutex<LinkState>>, tx: broadcast::Sender<Vec<u8>>) {
    loop {
        let chunk = tokio::task::spawn_blocking({
            let link = link.clone();
            move || -> io::Result<ReadChunk> {
                let mut g = link.lock().unwrap();
                match &mut *g {
                    LinkState::Released => Ok(ReadChunk::Released),
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
                let _ = tx.send(data);
            }
            Ok(Ok(ReadChunk::Empty)) => {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            Ok(Ok(ReadChunk::Released)) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "read from cart");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cart reader join");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
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
                            let mut g = link.lock().unwrap();
                            match &mut *g {
                                LinkState::Released => {
                                    tracing::debug!("write to cart ignored (serial released)");
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
                    Some(Ok(Message::Ping(p))) => {
                        let _ = socket.send(Message::Pong(p)).await;
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
