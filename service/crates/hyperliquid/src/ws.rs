//! Streaming market data over the venue's WebSocket.
//!
//! Two layers: pure frame parsing (tested on captured fixtures) and a
//! connection task (reconnect, resubscribe, ping) feeding an mpsc channel.
//! Gap-filling after a reconnect is the consumer's job — the client only
//! says `Connected`, and the consumer pulls missed candles over REST.

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message;

/// The WS endpoint for a REST base: `https://api.hyperliquid.xyz` →
/// `wss://api.hyperliquid.xyz/ws`.
pub fn ws_url(base: &str) -> String {
    format!(
        "{}/ws",
        base.replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    )
}

/// One parsed frame from the stream.
#[derive(Debug)]
pub enum WsEvent {
    Candle(crate::Candle),
    Bbo(Bbo),
    UserFills(UserFills),
    SubscriptionResponse,
    Pong,
    /// A channel this module does not know. Carried, not dropped, so the
    /// consumer's logs can show what the venue started sending.
    Other(String),
    /// A channel this module does know, but whose `data` payload failed to
    /// deserialize into that channel's shape (a venue schema change, most
    /// likely). Distinct from [`Other`](WsEvent::Other) so the consumer's
    /// logs can tell "new channel" apart from "known channel, broken parse".
    Unparseable {
        channel: String,
        raw: String,
    },
}

/// Top of book. `bbo[0]` is the bid, `bbo[1]` the ask; a side can be empty.
#[derive(Debug, Clone, Deserialize)]
pub struct Bbo {
    pub coin: String,
    pub time: i64,
    pub bbo: [Option<BboLevel>; 2],
}

#[derive(Debug, Clone, Deserialize)]
pub struct BboLevel {
    pub px: String,
    pub sz: String,
    pub n: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserFills {
    #[serde(default)]
    pub is_snapshot: bool,
    pub user: String,
    pub fills: Vec<WsFill>,
}

/// A fill as the stream reports it. Numbers arrive as strings and stay
/// strings here; the consumer parses into `Decimal` at its boundary.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsFill {
    pub coin: String,
    pub px: String,
    pub sz: String,
    /// `"B"` buy, `"A"` sell — the venue's vocabulary, translated by the consumer.
    pub side: String,
    pub time: i64,
    pub oid: u64,
    pub tid: u64,
    #[serde(default)]
    pub fee: String,
    #[serde(default)]
    pub fee_token: String,
    #[serde(default)]
    pub cloid: Option<String>,
    #[serde(default)]
    pub crossed: bool,
}

#[derive(Deserialize)]
struct Frame {
    channel: String,
    #[serde(default)]
    data: serde_json::Value,
}

/// Parse one text frame. Unknown channels are [`WsEvent::Other`], not errors:
/// the stream must survive the venue adding channels. A known channel whose
/// `data` fails to deserialize is [`WsEvent::Unparseable`], not an error
/// either — that failure is on the venue's payload, not on this frame being
/// unreadable, and the stream must survive it the same way. Only a frame
/// that isn't even valid JSON, or lacks a `channel`, fails outright.
pub fn parse_frame(text: &str) -> Result<WsEvent, serde_json::Error> {
    let frame: Frame = serde_json::from_str(text)?;
    Ok(match frame.channel.as_str() {
        "candle" => match serde_json::from_value(frame.data) {
            Ok(c) => WsEvent::Candle(c),
            Err(_) => WsEvent::Unparseable {
                channel: frame.channel,
                raw: text.to_string(),
            },
        },
        "bbo" => match serde_json::from_value(frame.data) {
            Ok(b) => WsEvent::Bbo(b),
            Err(_) => WsEvent::Unparseable {
                channel: frame.channel,
                raw: text.to_string(),
            },
        },
        "userFills" => match serde_json::from_value(frame.data) {
            Ok(f) => WsEvent::UserFills(f),
            Err(_) => WsEvent::Unparseable {
                channel: frame.channel,
                raw: text.to_string(),
            },
        },
        "subscriptionResponse" => WsEvent::SubscriptionResponse,
        "pong" => WsEvent::Pong,
        other => WsEvent::Other(other.to_string()),
    })
}

/// What to ask the stream for. Serialized exactly as the venue expects.
#[derive(Debug, Clone)]
pub enum Subscription {
    Candle { coin: String, interval: String },
    Bbo { coin: String },
    UserFills { user: String },
}

impl Subscription {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Subscription::Candle { coin, interval } => {
                serde_json::json!({"type": "candle", "coin": coin, "interval": interval})
            }
            Subscription::Bbo { coin } => serde_json::json!({"type": "bbo", "coin": coin}),
            Subscription::UserFills { user } => {
                serde_json::json!({"type": "userFills", "user": user})
            }
        }
    }
}

/// What the consumer receives. `Connected` after every successful
/// (re)connect+subscribe — a consumer that has seen one before must treat it
/// as "you may have missed frames" and gap-fill over REST.
#[derive(Debug)]
pub enum WsMessage {
    Connected,
    Event(WsEvent),
    Disconnected,
}

#[derive(Debug, Clone, Copy)]
pub struct WsConfig {
    pub ping_interval: std::time::Duration,
    pub backoff_start: std::time::Duration,
    pub backoff_cap: std::time::Duration,
    /// Cap on a single connect attempt (TCP + TLS + WS handshake). A hung
    /// handshake against an unreachable-but-listening peer would otherwise
    /// block this attempt forever, since `connect_async` has no timeout of
    /// its own — wedging the reconnect loop with no `ConnectFailed` and no
    /// retry. A field, not a constant, so tests can drive it short.
    pub connect_timeout: std::time::Duration,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            // The venue closes sockets idle for 60s; ping at half that.
            ping_interval: std::time::Duration::from_secs(30),
            backoff_start: std::time::Duration::from_secs(1),
            backoff_cap: std::time::Duration::from_secs(60),
            // Matches the REST client's timeout (see `client()` in lib.rs).
            connect_timeout: std::time::Duration::from_secs(15),
        }
    }
}

/// Spawn the connection task. It reconnects forever (backoff doubling from
/// `backoff_start` to `backoff_cap`, reset after a connection that lasted
/// longer than the cap) and exits when the receiver is dropped.
pub fn spawn(
    url: String,
    subs: Vec<Subscription>,
    cfg: WsConfig,
) -> tokio::sync::mpsc::Receiver<WsMessage> {
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    tokio::spawn(run(url, subs, cfg, tx));
    rx
}

async fn run(
    url: String,
    subs: Vec<Subscription>,
    cfg: WsConfig,
    tx: tokio::sync::mpsc::Sender<WsMessage>,
) {
    let mut backoff = cfg.backoff_start;
    loop {
        let connected_at = tokio::time::Instant::now();
        match connect_and_stream(&url, &subs, cfg, &tx).await {
            SessionEnd::ReceiverDropped => return,
            SessionEnd::ConnectionLost => {
                if tx.send(WsMessage::Disconnected).await.is_err() {
                    return;
                }
            }
            SessionEnd::ConnectFailed => {}
        }
        if connected_at.elapsed() > cfg.backoff_cap {
            backoff = cfg.backoff_start; // it held for a while; start fresh
        }
        // A venue that's simply unreachable never touches `tx`, so without
        // racing the sleep against the receiver closing, a dropped receiver
        // would never be noticed and this loop would retry forever.
        tokio::select! {
            _ = tx.closed() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(cfg.backoff_cap);
    }
}

enum SessionEnd {
    ConnectFailed,
    ConnectionLost,
    ReceiverDropped,
}

async fn connect_and_stream(
    url: &str,
    subs: &[Subscription],
    cfg: WsConfig,
    tx: &tokio::sync::mpsc::Sender<WsMessage>,
) -> SessionEnd {
    // Race the connect attempt itself against the receiver closing, so a
    // hung or slow handshake against an unreachable venue doesn't stop this
    // task from noticing the consumer is gone. The attempt is also bounded
    // by `connect_timeout`: `connect_async` has no timeout of its own, so a
    // peer that accepts the TCP connection but never completes TLS/WS would
    // otherwise wedge this task forever with no `ConnectFailed` and no retry.
    let mut socket = tokio::select! {
        _ = tx.closed() => return SessionEnd::ReceiverDropped,
        res = tokio::time::timeout(cfg.connect_timeout, tokio_tungstenite::connect_async(url)) => {
            match res {
                Ok(Ok((socket, _))) => socket,
                Ok(Err(_)) | Err(_) => return SessionEnd::ConnectFailed,
            }
        }
    };
    for sub in subs {
        let msg = serde_json::json!({"method": "subscribe", "subscription": sub.to_json()});
        // This send happens strictly before `Connected` is ever emitted, so
        // a failure here is a connection that was never established from the
        // consumer's point of view — ConnectFailed, not ConnectionLost.
        // Reporting ConnectionLost here would emit Disconnected for a
        // session that never emitted Connected, violating the contract.
        if socket.send(Message::Text(msg.to_string())).await.is_err() {
            return SessionEnd::ConnectFailed;
        }
    }
    if tx.send(WsMessage::Connected).await.is_err() {
        return SessionEnd::ReceiverDropped;
    }

    let mut ping = tokio::time::interval(cfg.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // the immediate first tick; pings start one interval in

    // We ping every `ping_interval`, and a live link answers a ping with a
    // pong at least that often — so `2 * ping_interval` of total silence
    // (no frame of any kind, not just pongs) means the link is dead even
    // though `socket.next()` is still pending: no FIN, no RST, just gone.
    // Without this, that half-open state is caught only by TCP's own
    // retransmit timeout, which can take 15+ minutes.
    let mut last_rx = tokio::time::Instant::now();
    let stale_after = cfg.ping_interval * 2;

    loop {
        tokio::select! {
            _ = ping.tick() => {
                let p = serde_json::json!({"method": "ping"}).to_string();
                if socket.send(Message::Text(p)).await.is_err() {
                    return SessionEnd::ConnectionLost;
                }
            }
            _ = tokio::time::sleep_until(last_rx + stale_after) => {
                return SessionEnd::ConnectionLost;
            }
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    last_rx = tokio::time::Instant::now();
                    let event = match parse_frame(&text) {
                        Ok(e) => e,
                        // The frame itself wasn't valid JSON, or had no
                        // `channel` field — parse_frame already routes a
                        // known channel's broken `data` to `Unparseable`
                        // itself, so this arm is only the outer failure.
                        Err(_) => WsEvent::Unparseable { channel: "?".into(), raw: text },
                    };
                    if tx.send(WsMessage::Event(event)).await.is_err() {
                        return SessionEnd::ReceiverDropped;
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    last_rx = tokio::time::Instant::now();
                    if socket.send(Message::Pong(p)).await.is_err() {
                        return SessionEnd::ConnectionLost;
                    }
                }
                Some(Ok(_)) => {
                    last_rx = tokio::time::Instant::now();
                }
                Some(Err(_)) | None => return SessionEnd::ConnectionLost,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ws_url_is_derived_from_the_rest_base() {
        assert_eq!(ws_url(crate::MAINNET), "wss://api.hyperliquid.xyz/ws");
        assert_eq!(ws_url("http://127.0.0.1:9000"), "ws://127.0.0.1:9000/ws");
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/ws/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
    }

    #[test]
    fn a_candle_frame_parses_into_the_rest_candle_shape() {
        match parse_frame(&fixture("candle")).unwrap() {
            WsEvent::Candle(c) => {
                assert_eq!(c.s, "BTC");
                assert!(c.close_time > c.t);
                let _: rust_decimal::Decimal = c.c.parse().expect("close parses as decimal");
            }
            other => panic!("expected a candle, got {other:?}"),
        }
    }

    #[test]
    fn a_bbo_frame_carries_both_sides() {
        match parse_frame(&fixture("bbo")).unwrap() {
            WsEvent::Bbo(b) => {
                assert_eq!(b.coin, "BTC");
                let bid = b.bbo[0].as_ref().expect("bid side present in fixture");
                let ask = b.bbo[1].as_ref().expect("ask side present in fixture");
                let bid_px: rust_decimal::Decimal = bid.px.parse().unwrap();
                let ask_px: rust_decimal::Decimal = ask.px.parse().unwrap();
                assert!(
                    bid_px < ask_px,
                    "crossed fixture book: {bid_px} >= {ask_px}"
                );
            }
            other => panic!("expected a bbo, got {other:?}"),
        }
    }

    #[test]
    fn a_user_fills_snapshot_parses() {
        match parse_frame(&fixture("user_fills")).unwrap() {
            WsEvent::UserFills(f) => {
                assert!(!f.user.is_empty());
                for fill in &f.fills {
                    assert!(fill.side == "B" || fill.side == "A", "side {:?}", fill.side);
                }
            }
            other => panic!("expected user fills, got {other:?}"),
        }
    }

    #[test]
    fn control_frames_are_named_and_unknown_channels_are_kept_not_dropped() {
        assert!(matches!(
            parse_frame(&fixture("subscription_response")).unwrap(),
            WsEvent::SubscriptionResponse
        ));
        assert!(matches!(
            parse_frame(&fixture("pong")).unwrap(),
            WsEvent::Pong
        ));
        assert!(matches!(
            parse_frame(r#"{"channel":"notifications","data":{}}"#).unwrap(),
            WsEvent::Other(c) if c == "notifications"
        ));
    }

    #[test]
    fn a_known_channel_with_an_unparseable_payload_is_distinct_from_an_unknown_channel() {
        match parse_frame(r#"{"channel":"candle","data":{"garbage":true}}"#).unwrap() {
            WsEvent::Unparseable { channel, .. } => assert_eq!(channel, "candle"),
            other => panic!("expected Unparseable, got {other:?}"),
        }
        // An unknown channel is still `Other`, not `Unparseable` — the two
        // must stay distinguishable in the consumer's logs.
        assert!(matches!(
            parse_frame(r#"{"channel":"notifications","data":{}}"#).unwrap(),
            WsEvent::Other(c) if c == "notifications"
        ));
    }

    /// `run` is private, so this test lives here rather than in the
    /// integration test file. A venue that's unreachable (nothing listens on
    /// this port, so every connect attempt fails fast and the task falls
    /// into its backoff sleep) never sends anything on `tx` — the only way
    /// the task can notice the receiver was dropped is by racing the
    /// connect attempt and the backoff sleep against `tx.closed()`, per the
    /// "task exits when the receiver is dropped" contract.
    #[tokio::test]
    async fn the_task_exits_once_the_receiver_is_dropped_even_while_the_venue_is_unreachable() {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let cfg = WsConfig {
            ping_interval: std::time::Duration::from_secs(30),
            backoff_start: std::time::Duration::from_millis(20),
            backoff_cap: std::time::Duration::from_millis(50),
            connect_timeout: std::time::Duration::from_secs(15),
        };
        let handle = tokio::spawn(run("ws://127.0.0.1:1".to_string(), vec![], cfg, tx));

        drop(rx);

        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("run() must exit promptly once the receiver is dropped, even with the venue unreachable")
            .expect("the run() task must not panic");
    }

    /// `connect_async` has no timeout of its own, so a peer that completes
    /// the TCP handshake but never speaks TLS/WS would hang the connect
    /// attempt forever without `connect_timeout` — no `ConnectFailed`, no
    /// retry, the reconnect loop wedged. The mock server here accepts every
    /// TCP connection and then never sends a byte, reproducing exactly that.
    #[tokio::test]
    async fn a_hung_handshake_times_out_instead_of_wedging_the_reconnect_loop() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Accept every connection and hold the socket without ever writing
        // to it — the handshake never completes, on any retry.
        let accept_task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });

        let cfg = WsConfig {
            ping_interval: std::time::Duration::from_secs(30),
            backoff_start: std::time::Duration::from_millis(20),
            backoff_cap: std::time::Duration::from_millis(50),
            connect_timeout: std::time::Duration::from_millis(200),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let handle = tokio::spawn(run(format!("ws://{addr}"), vec![], cfg, tx));

        // The handshake never completes, so nothing is ever sent on `tx` —
        // give it comfortably longer than one connect_timeout and confirm
        // silence, rather than a wedged-forever task masquerading as one
        // that's merely slow.
        let saw_nothing =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await;
        assert!(
            saw_nothing.is_err(),
            "expected no message while every handshake hangs, got {saw_nothing:?}"
        );

        // And the task must still notice the receiver going away and exit,
        // exactly as it would mid any other retry cycle — the timeout must
        // not have replaced one way to get stuck with another.
        drop(rx);
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("run() must exit promptly once the receiver is dropped, even mid a hung handshake retry")
            .expect("the run() task must not panic");

        accept_task.abort();
    }
}
