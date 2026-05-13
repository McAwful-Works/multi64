//! Integration tests for HTTP surface (no serial device).

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use multi64d::http_metadata_router;
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
