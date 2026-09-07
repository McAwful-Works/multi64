//! Integration tests for HTTP surface (no serial device).

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use multi64d::{build_app, http_metadata_router, AppState, LinkState, SerialConfig};
use std::sync::Arc;
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
    assert_eq!(v["serialActive"], false);
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
    assert_eq!(v["serialActive"], false);
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
