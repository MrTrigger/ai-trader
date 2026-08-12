//! Capture raw WS frames for the fixture set. Run with an output directory:
//!
//!     cargo run -p hyperliquid --example ws_capture -- crates/hyperliquid/tests/fixtures/ws
//!
//! Subscribes to BTC 1m candles, BTC bbo, and userFills for an active
//! address (picked from a recent BTC trade, so the snapshot fixture has
//! non-empty fills rather than just the empty-snapshot shape). Writes the
//! first frame seen per channel and exits once it has one of each (or after
//! 90s).

use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let dir = std::env::args().nth(1).expect("usage: ws_capture <output-dir>");
    std::fs::create_dir_all(&dir).unwrap();
    let url = hyperliquid::ws::ws_url(hyperliquid::MAINNET);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");

    for sub in [
        serde_json::json!({"type": "candle", "coin": "BTC", "interval": "1m"}),
        serde_json::json!({"type": "bbo", "coin": "BTC"}),
        serde_json::json!({"type": "userFills", "user": "0xfd66e330954b1d33772a78a70874cc2600754eec"}),
    ] {
        let msg = serde_json::json!({"method": "subscribe", "subscription": sub});
        socket.send(Message::Text(msg.to_string())).await.unwrap();
    }
    socket
        .send(Message::Text(serde_json::json!({"method": "ping"}).to_string()))
        .await
        .unwrap();

    let wanted: HashSet<&str> =
        ["candle", "bbo", "userFills", "subscriptionResponse", "pong"].into();
    let mut have: HashSet<String> = HashSet::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);

    while have.len() < wanted.len() {
        let frame = tokio::select! {
            f = socket.next() => f,
            _ = tokio::time::sleep_until(deadline) => break,
        };
        let Some(Ok(Message::Text(text))) = frame else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(channel) = v.get("channel").and_then(|c| c.as_str()) else { continue };
        if wanted.contains(channel) && !have.contains(channel) {
            let name = if channel == "userFills" { "user_fills" } else { channel };
            let name = if name == "subscriptionResponse" { "subscription_response" } else { name };
            std::fs::write(format!("{dir}/{name}.json"), &text).unwrap();
            println!("captured {channel}");
            have.insert(channel.to_string());
        }
    }
    println!("done: {have:?}");
}
