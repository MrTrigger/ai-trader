//! The reconnect contract, proven against a local server: connect → Connected,
//! frames → Events, server drop → Disconnected, then a fresh Connected with
//! the subscriptions sent again.

use futures_util::{SinkExt, StreamExt};
use hyperliquid::ws::{self, Subscription, WsConfig, WsMessage};
use tokio_tungstenite::tungstenite::Message;

const CANDLE: &str = r#"{"channel":"candle","data":{"t":1754956800000,"T":1754956860000,"s":"BTC","i":"1m","o":"64000","c":"64010","h":"64020","l":"63990","v":"12.5","n":42}}"#;

/// Accept one WS connection, record what the client sends, push `frames`,
/// then drop the connection.
async fn serve_once(
    listener: &tokio::net::TcpListener,
    frames: &[&str],
) -> Vec<serde_json::Value> {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    let mut received = Vec::new();
    // The client sends its subscribe messages immediately after the handshake.
    while received.len() < 1 {
        match socket.next().await {
            Some(Ok(Message::Text(t))) => received.push(serde_json::from_str(&t).unwrap()),
            Some(Ok(_)) => continue,
            other => panic!("client hung up early: {other:?}"),
        }
    }
    for f in frames {
        socket.send(Message::Text(f.to_string())).await.unwrap();
    }
    received
}

#[tokio::test]
async fn the_client_reconnects_and_resubscribes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let cfg = WsConfig {
        ping_interval: std::time::Duration::from_secs(30),
        backoff_start: std::time::Duration::from_millis(50),
        backoff_cap: std::time::Duration::from_millis(200),
    };
    let subs = vec![Subscription::Candle { coin: "BTC".into(), interval: "1m".into() }];
    let mut rx = ws::spawn(url, subs, cfg);

    let server = tokio::spawn(async move {
        let first = serve_once(&listener, &[CANDLE]).await; // then dropped: disconnect
        let second = serve_once(&listener, &[]).await; // the reconnect
        (first, second)
    });

    assert!(matches!(rx.recv().await, Some(WsMessage::Connected)));
    match rx.recv().await {
        Some(WsMessage::Event(ws::WsEvent::Candle(c))) => assert_eq!(c.s, "BTC"),
        other => panic!("expected the candle, got {other:?}"),
    }
    assert!(matches!(rx.recv().await, Some(WsMessage::Disconnected)));
    assert!(matches!(rx.recv().await, Some(WsMessage::Connected)));

    let (first, second) = server.await.unwrap();
    assert_eq!(first, second, "the resubscribe must repeat the original subscriptions");
    assert_eq!(first[0]["method"], "subscribe");
    assert_eq!(first[0]["subscription"]["type"], "candle");
    assert_eq!(first[0]["subscription"]["coin"], "BTC");
    assert_eq!(first[0]["subscription"]["interval"], "1m");
}
