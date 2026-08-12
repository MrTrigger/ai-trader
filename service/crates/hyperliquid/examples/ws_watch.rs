//! Watch the live stream for a coin. A manual smoke test for the WS module:
//!
//!     cargo run -p hyperliquid --example ws_watch -- BTC 60
//!
//! Prints one line per candle close and a bbo line at most once a second.

use hyperliquid::ws::{self, Subscription, WsConfig, WsEvent, WsMessage};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let coin = std::env::args().nth(1).unwrap_or_else(|| "BTC".into());
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let subs = vec![
        Subscription::Candle {
            coin: coin.clone(),
            interval: "1m".into(),
        },
        Subscription::Bbo { coin: coin.clone() },
    ];
    let mut rx = ws::spawn(ws::ws_url(hyperliquid::MAINNET), subs, WsConfig::default());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut last_bbo = std::time::Instant::now() - std::time::Duration::from_secs(1);

    loop {
        let msg = tokio::select! {
            m = rx.recv() => m,
            _ = tokio::time::sleep_until(deadline) => break,
        };
        match msg {
            Some(WsMessage::Connected) => println!("connected"),
            Some(WsMessage::Disconnected) => println!("disconnected, reconnecting"),
            Some(WsMessage::Event(WsEvent::Candle(c))) => {
                println!(
                    "candle {} o={} h={} l={} c={} v={} n={}",
                    c.s, c.o, c.h, c.l, c.c, c.v, c.n
                )
            }
            Some(WsMessage::Event(WsEvent::Bbo(b))) => {
                if last_bbo.elapsed() >= std::time::Duration::from_secs(1) {
                    let px = |l: &Option<ws::BboLevel>| {
                        l.as_ref()
                            .map(|l| l.px.clone())
                            .unwrap_or_else(|| "-".into())
                    };
                    println!("bbo {} {} / {}", b.coin, px(&b.bbo[0]), px(&b.bbo[1]));
                    last_bbo = std::time::Instant::now();
                }
            }
            Some(WsMessage::Event(_)) => {}
            None => break,
        }
    }
}
