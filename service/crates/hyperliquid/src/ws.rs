//! Streaming market data over the venue's WebSocket.
//!
//! Two layers: pure frame parsing (tested on captured fixtures) and a
//! connection task (reconnect, resubscribe, ping) feeding an mpsc channel.
//! Gap-filling after a reconnect is the consumer's job — the client only
//! says `Connected`, and the consumer pulls missed candles over REST.

/// The WS endpoint for a REST base: `https://api.hyperliquid.xyz` →
/// `wss://api.hyperliquid.xyz/ws`.
pub fn ws_url(base: &str) -> String {
    format!("{}/ws", base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ws_url_is_derived_from_the_rest_base() {
        assert_eq!(ws_url(crate::MAINNET), "wss://api.hyperliquid.xyz/ws");
        assert_eq!(ws_url("http://127.0.0.1:9000"), "ws://127.0.0.1:9000/ws");
    }
}
