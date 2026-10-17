//! `/ws` over a real loopback socket, with a down link so no cart is needed. What the daemon would
//! have read from a cart is put on its broadcast directly.

use futures_util::StreamExt;
use multi64d::{build_app, AppState, CartKind, LinkState, SerialConfig};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Serve the full router on a free loopback port, with a broadcast of `capacity` cart chunks.
async fn serve(capacity: usize) -> (String, broadcast::Sender<Vec<u8>>) {
    let (from_cart, _) = broadcast::channel::<Vec<u8>>(capacity);
    let state = Arc::new(AppState::new(
        SerialConfig {
            path: "COM_TEST".into(),
            baud: 115200,
            clear_serial: false,
            cart: CartKind::Sc64,
        },
        LinkState::Faulted,
        from_cart.clone(),
        Vec::new(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, build_app(state)).await.unwrap() });
    (format!("ws://{addr}/ws"), from_cart)
}

/// Connect and read the hello, after which the daemon is subscribed to the cart broadcast.
async fn connect(url: &str) -> Client {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    match next(&mut ws).await {
        Message::Text(t) => assert!(t.contains("\"hello\""), "{t}"),
        other => panic!("expected the hello, got {other:?}"),
    }
    ws
}

/// The next message, or a panic naming what was waited for.
async fn next(ws: &mut Client) -> Message {
    tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .expect("no message within 10 s")
        .expect("socket ended without a Close")
        .expect("websocket error")
}

/// The next message that is not cart data; `data` collects the cart data before it.
async fn next_control(ws: &mut Client, data: &mut Vec<Vec<u8>>) -> Message {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "nothing but {} cart chunks within 10 s: {data:?}",
                    data.len()
                )
            })
            .expect("socket ended without a Close")
            .expect("websocket error");
        match msg {
            Message::Binary(b) => data.push(b.to_vec()),
            other => return other,
        }
    }
}

#[tokio::test]
async fn a_client_that_falls_behind_is_closed_rather_than_given_a_stream_with_a_hole() {
    // A lagging client used to be logged and kept, so it read on past the chunks it had lost and
    // its L3 stream had a gap in it nothing reported. This runtime has one thread, so the daemon
    // cannot forward anything until the test awaits: all ten chunks land in a 4-chunk buffer.
    let (url, from_cart) = serve(4).await;
    let mut ws = connect(&url).await;
    for i in 0..10u8 {
        from_cart.send(vec![i; 8]).unwrap();
    }
    let mut data = Vec::new();
    match next_control(&mut ws, &mut data).await {
        Message::Close(Some(frame)) => {
            assert_eq!(frame.code, CloseCode::Again, "{frame:?}");
            assert!(frame.reason.contains("fell behind"), "{frame:?}");
        }
        other => panic!(
            "expected a Close after {} chunks, got {other:?}",
            data.len()
        ),
    }
    assert!(
        data.is_empty(),
        "nothing after the gap reaches the client: {data:?}"
    );
}

#[tokio::test]
async fn a_client_that_keeps_up_gets_every_chunk_in_order() {
    let (url, from_cart) = serve(4).await;
    let mut ws = connect(&url).await;
    for i in 0..20u8 {
        from_cart.send(vec![i; 8]).unwrap();
        match next(&mut ws).await {
            Message::Binary(b) => assert_eq!(b.as_ref(), [i; 8]),
            other => panic!("expected chunk {i}, got {other:?}"),
        }
    }
}
