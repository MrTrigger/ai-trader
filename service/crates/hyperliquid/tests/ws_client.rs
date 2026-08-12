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
        connect_timeout: std::time::Duration::from_secs(15),
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

/// `Disconnected` is only valid for a session that reached `Connected` first
/// (per the module's contract). A session whose subscribe send fails —
/// which can happen if the server completes the WS handshake and then drops
/// the connection immediately, before the client's subscribe frame lands —
/// must end silently (`ConnectFailed`), never by emitting `Disconnected`.
///
/// The race is inherent (whether the subscribe send observes the drop
/// depends on OS buffering timing), so this drives a few connect cycles
/// against a server that always drops right after the handshake and checks
/// the one invariant that must hold regardless of how the race resolves:
/// the very first message a consumer ever receives, if any, is `Connected`
/// — never `Disconnected`.
#[tokio::test]
async fn the_first_message_a_consumer_receives_is_never_disconnected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let cfg = WsConfig {
        ping_interval: std::time::Duration::from_secs(30),
        backoff_start: std::time::Duration::from_millis(20),
        backoff_cap: std::time::Duration::from_millis(50),
        connect_timeout: std::time::Duration::from_secs(15),
    };
    let subs = vec![Subscription::Bbo { coin: "BTC".into() }];
    let mut rx = ws::spawn(url, subs, cfg);

    let server = tokio::spawn(async move {
        // Accept a handful of connections, completing the WS handshake each
        // time and then dropping the socket immediately without reading
        // anything the client sends — the shape that can race the
        // subscribe send against the connection closing.
        for _ in 0..5 {
            let Ok((stream, _)) = listener.accept().await else { break };
            let Ok(socket) = tokio_tungstenite::accept_async(stream).await else { continue };
            drop(socket);
        }
    });

    let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
    match first {
        Ok(msg) => assert!(
            !matches!(msg, Some(WsMessage::Disconnected)),
            "the first message must never be Disconnected, got {msg:?}"
        ),
        Err(_) => {} // nothing arrived within the window: also consistent with the contract
    }

    // The client may well have gotten what it needed in a single cycle,
    // leaving the mock server blocked in `listener.accept().await` for the
    // connections that never came — awaiting it here would hang the test.
    drop(rx);
    server.abort();
}

/// A half-open link (server accepts, then goes silent without ever closing
/// the socket — no FIN, no RST) is invisible to `socket.next()`: it just
/// never resolves. Left unguarded, that leaves the task pending forever with
/// no `Disconnected` for the consumer, detected only by TCP's own retransmit
/// timeout (15+ minutes). The fix tracks time since the last received frame
/// and treats silence past `2 * ping_interval` as a dead link: our own ping
/// every `ping_interval` guarantees a pong at least that often on a live
/// link, so twice that with nothing back means the link is gone.
#[tokio::test]
async fn a_silently_dead_link_is_detected_by_ping_staleness_not_left_hanging() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let cfg = WsConfig {
        ping_interval: std::time::Duration::from_millis(100),
        backoff_start: std::time::Duration::from_millis(50),
        backoff_cap: std::time::Duration::from_millis(200),
        connect_timeout: std::time::Duration::from_secs(15),
    };
    let subs = vec![Subscription::Candle { coin: "BTC".into(), interval: "1m".into() }];
    let mut rx = ws::spawn(url, subs, cfg);

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        // Read the subscribe, same as `serve_once`.
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(_))) => break,
                Some(Ok(_)) => continue,
                other => panic!("client hung up early: {other:?}"),
            }
        }
        socket.send(Message::Text(CANDLE.to_string())).await.unwrap();
        // Go silent without closing: never read (so the client's pings pile
        // up unanswered) and never drop the socket. Held open for the rest
        // of the test; the outer test aborts this task when it's done.
        std::future::pending::<()>().await;
    });

    assert!(matches!(rx.recv().await, Some(WsMessage::Connected)));
    match rx.recv().await {
        Some(WsMessage::Event(ws::WsEvent::Candle(c))) => assert_eq!(c.s, "BTC"),
        other => panic!("expected the candle, got {other:?}"),
    }
    // Staleness threshold is 2 * ping_interval = 200ms; 1s is a generous
    // margin above that for a deterministic assertion.
    let disconnected = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await;
    assert!(
        matches!(disconnected, Ok(Some(WsMessage::Disconnected))),
        "expected Disconnected within 1s of silence, got {disconnected:?}"
    );

    drop(rx);
    server.abort();
}
