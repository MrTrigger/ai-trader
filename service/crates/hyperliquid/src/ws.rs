//! Streaming market data over the venue's WebSocket.
//!
//! Two layers: pure frame parsing (tested on captured fixtures) and a
//! connection task (reconnect, resubscribe, ping) feeding an mpsc channel.
//! Gap-filling after a reconnect is the consumer's job — the client only
//! says `Connected`, and the consumer pulls missed candles over REST.

use serde::Deserialize;

/// The WS endpoint for a REST base: `https://api.hyperliquid.xyz` →
/// `wss://api.hyperliquid.xyz/ws`.
pub fn ws_url(base: &str) -> String {
    format!("{}/ws", base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1))
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
/// the stream must survive the venue adding channels.
pub fn parse_frame(text: &str) -> Result<WsEvent, serde_json::Error> {
    let frame: Frame = serde_json::from_str(text)?;
    Ok(match frame.channel.as_str() {
        "candle" => WsEvent::Candle(serde_json::from_value(frame.data)?),
        "bbo" => WsEvent::Bbo(serde_json::from_value(frame.data)?),
        "userFills" => WsEvent::UserFills(serde_json::from_value(frame.data)?),
        "subscriptionResponse" => WsEvent::SubscriptionResponse,
        "pong" => WsEvent::Pong,
        other => WsEvent::Other(other.to_string()),
    })
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
                assert!(bid_px < ask_px, "crossed fixture book: {bid_px} >= {ask_px}");
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
        assert!(matches!(parse_frame(&fixture("pong")).unwrap(), WsEvent::Pong));
        assert!(matches!(
            parse_frame(r#"{"channel":"notifications","data":{}}"#).unwrap(),
            WsEvent::Other(c) if c == "notifications"
        ));
    }
}
