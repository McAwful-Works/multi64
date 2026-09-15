//! Integration tests for HTTP surface (no serial device).

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use multi64d::{
    build_app, http_metadata_router, AppState, CartKind, LinkState, SerialConfig,
    RELEASE_LOCK_TIMEOUT, ROOT_LOCK_TIMEOUT,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tower::ServiceExt;

#[tokio::test]
async fn get_root_returns_service_json() {
    let app = http_metadata_router();
    let res = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["service"], "multi64d");
    assert_eq!(v["websocket_path"], "/ws");
    assert!(v["version"].as_str().is_some());
    assert_eq!(v["serial"], "");
    assert_eq!(v["cart"], "");
    assert_eq!(v["serialActive"], false);
    assert_eq!(v["serialBusy"], false);
}

#[tokio::test]
async fn get_health_returns_ok_json() {
    let app = http_metadata_router();
    let res = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes.as_ref(), br#"{"status":"ok"}"#);
}

/// Full router with a **down** link, so the whole HTTP surface (including `/ws` and the origin
/// guard) is testable on a host with no cart attached.
fn app_with_allowed_origins(allowed: &[&str]) -> Router {
    let (from_cart, _) = broadcast::channel::<Vec<u8>>(16);
    let state = Arc::new(AppState::new(
        SerialConfig {
            path: "COM_TEST".into(),
            baud: 115200,
            clear_serial: false,
            cart: CartKind::Sc64,
        },
        LinkState::Faulted,
        from_cart,
        allowed.iter().map(|s| s.to_string()).collect(),
    ));
    build_app(state)
}

async fn get_with_origin(app: Router, uri: &str, origin: Option<&str>) -> StatusCode {
    let mut b = Request::builder().uri(uri);
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    app.oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn request_without_origin_header_is_allowed() {
    // Xfer64 (`ureq`), Multi64 and `multi64-test-connector` are native clients and send no Origin.
    let status = get_with_origin(app_with_allowed_origins(&[]), "/", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn browser_origin_is_rejected_by_default() {
    let status = get_with_origin(
        app_with_allowed_origins(&[]),
        "/",
        Some("https://evil.example"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn websocket_upgrade_is_rejected_for_disallowed_origin() {
    // A WebSocket handshake is exempt from the CORS response gate, so this must be blocked before
    // the upgrade rather than left to `CorsLayer`.
    let res = app_with_allowed_origins(&[])
        .oneshot(
            Request::builder()
                .uri("/ws")
                .header("origin", "https://evil.example")
                .header("connection", "Upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn serial_release_is_rejected_for_disallowed_origin() {
    let res = app_with_allowed_origins(&[])
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/serial/release")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn allow_listed_origin_passes() {
    let status = get_with_origin(
        app_with_allowed_origins(&["https://ok.example"]),
        "/",
        Some("https://ok.example"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn root_reports_serial_inactive_while_link_is_faulted() {
    // A faulted link must not be advertised as held, or Xfer64 keeps yielding to a dead daemon.
    let res = app_with_allowed_origins(&[])
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["serial"], "COM_TEST");
    assert!(
        matches!(v["cart"].as_str(), Some("sc64" | "ed64" | "ed64pro")),
        "GET / names the cart: {v}"
    );
    assert_eq!(v["serialActive"], false);
}

fn faulted_state() -> Arc<AppState> {
    let (from_cart, _) = broadcast::channel::<Vec<u8>>(16);
    Arc::new(AppState::new(
        SerialConfig {
            path: "COM_TEST".into(),
            baud: 115200,
            clear_serial: false,
            cart: CartKind::Sc64,
        },
        LinkState::Faulted,
        from_cart,
        Vec::new(),
    ))
}

/// Hold the link lock on a plain thread for `hold`, as a long cart write does.
fn hold_link_lock(state: &Arc<AppState>, hold: Duration) -> std::thread::JoinHandle<()> {
    let link = state.link.clone();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _g = link.blocking_lock();
        locked_tx.send(()).unwrap();
        std::thread::sleep(hold);
    });
    locked_rx.recv().unwrap();
    holder
}

async fn post_release(state: &Arc<AppState>) -> StatusCode {
    build_app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/serial/release")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn link_is_released(state: &AppState) -> bool {
    let link = state.link.try_lock().expect("nothing else holds the link");
    matches!(&*link, LinkState::Released)
}

#[test]
fn release_that_cannot_get_the_link_in_time_fails_and_never_applies_late() {
    // One blocking thread, so a refused release that kept one waiting for the lock would starve
    // everything after it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // #137: the release used to wait out the lock however long it took and apply whenever it
        // got it, after Xfer64's 5 s request had given up and skipped its resume, leaving the
        // bridge released for good.
        let state = faulted_state();
        let hold = RELEASE_LOCK_TIMEOUT + Duration::from_millis(1500);
        let holder = hold_link_lock(&state, hold);

        // Two at once, as a client retrying during a long write might send.
        let started = Instant::now();
        let (a, b) = tokio::join!(post_release(&state), post_release(&state));
        let waited = started.elapsed();
        assert_eq!(a, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(b, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            waited < RELEASE_LOCK_TIMEOUT + Duration::from_millis(1000),
            "answered at the bound, not when the lock came free: {waited:?}"
        );

        // #167: a refused release used to keep a blocking-pool thread parked on the lock until
        // the holder let go. The lock is still held here, so a parked waiter would still be
        // parked.
        assert!(!holder.is_finished(), "the lock must still be held");
        let free_thread = tokio::time::timeout(
            Duration::from_millis(500),
            tokio::task::spawn_blocking(|| ()),
        )
        .await;
        assert!(
            free_thread.is_ok(),
            "a release that answered 503 must not leave a thread waiting for the link"
        );

        while !holder.is_finished() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        holder.join().unwrap();
        // Give any abandoned wait time to take the lock it was queued for.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !link_is_released(&state),
            "a release that answered 503 must not take effect afterwards"
        );

        // The next release, with the lock free, works as normal.
        assert_eq!(post_release(&state).await, StatusCode::OK);
        assert!(link_is_released(&state));
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn root_reports_a_busy_link_instead_of_waiting_for_it() {
    // #167: `GET /` used to wait for the link lock with no limit, so it hung for the whole of a
    // long cart write -- past the 2 s Xfer64 gives it.
    let state = faulted_state();
    let hold = ROOT_LOCK_TIMEOUT + Duration::from_millis(2500);
    let holder = hold_link_lock(&state, hold);

    let started = Instant::now();
    let v = get_root(&state).await;
    let waited = started.elapsed();
    assert!(
        waited < ROOT_LOCK_TIMEOUT + Duration::from_millis(1000),
        "answered at the bound, not when the lock came free: {waited:?}"
    );
    assert_eq!(v["serialBusy"], true, "{v}");
    assert_eq!(
        v["serialActive"], true,
        "a busy link is reported as held, so a client releases before opening the port: {v}"
    );
    assert_eq!(v["serial"], "COM_TEST");
    assert_eq!(v["cart"], "sc64");

    while !holder.is_finished() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    holder.join().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn root_waits_out_a_short_lock_hold() {
    // A reader iteration holds the lock for at most 50 ms; `GET /` still reports the real state.
    let state = faulted_state();
    let holder = hold_link_lock(&state, Duration::from_millis(100));
    let v = get_root(&state).await;
    assert_eq!(v["serialBusy"], false, "{v}");
    assert_eq!(v["serialActive"], false, "{v}");
    holder.join().unwrap();
}

async fn get_root(state: &Arc<AppState>) -> serde_json::Value {
    let res = build_app(state.clone())
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn release_waits_out_a_short_lock_hold() {
    // A reader iteration or a normal write holds the lock briefly; release must still succeed.
    let state = faulted_state();
    let holder = hold_link_lock(&state, Duration::from_millis(300));
    assert_eq!(post_release(&state).await, StatusCode::OK);
    assert!(link_is_released(&state));
    holder.join().unwrap();
}

#[tokio::test]
async fn resume_reopens_rather_than_reporting_success_on_a_faulted_link() {
    // The regression this guards: `resume` used to short-circuit on `Active`, and a dead pipe
    // stayed `Active` forever. `COM_TEST` does not exist, so a genuine reopen attempt must fail
    // loudly instead of answering `{"resumed":true}`.
    let res = app_with_allowed_origins(&[])
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/serial/resume")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
