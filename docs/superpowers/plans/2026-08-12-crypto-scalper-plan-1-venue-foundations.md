# Crypto-Scalper Plan 1: Venue Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the `hyperliquid` crate everything the scalper's execution layer needs: a WebSocket streaming module (candles, BBO, user fills, with reconnect) and the missing order controls (post-only, reduce-only, IOC, cancel-by-cloid, typed rejections).

**Architecture:** All changes live in `service/crates/hyperliquid` plus one additive enum variant in `service/crates/venue`. Order-action JSON builders become pure functions (unit-testable without HTTP). The WS module is two layers: a pure frame-parsing layer tested against fixtures captured from the live API, and a connection layer (spawned tokio task → mpsc channel) whose reconnect logic is tested against a local mock WS server. Gap-filling after reconnect is the *consumer's* job — the client just signals `Connected`.

**Tech Stack:** Rust, tokio, tokio-tungstenite (rustls), serde, rust_decimal. This is plan 1 of 3 (2: research pipeline, 3: scalper bot crate) from `docs/superpowers/specs/2026-08-12-crypto-scalper-design.md`.

## Global Constraints

- Money and quantities are `rust_decimal::Decimal`, never `f64` (venue crate doctrine).
- Every venue response shape must be covered by a fixture captured from the live API (house style, see `hyperliquid/src/lib.rs:25-33`). Synthetic fixtures only where live capture is impossible, and called out in a comment.
- Existing public behavior must not change: `VenueAdapter::place_order` semantics (Market→IOC aggressive cap at mark ±5%, Limit→GTC), `px_string` 5-significant-figure rule, `cloid` hashing, two-gate live arming.
- Additive changes only to the `venue` crate; no changes at all to `plan`, `executor`, `runner`.
- Workspace deps: use `workspace = true` where the dep exists in `service/Cargo.toml`; tokio has `default-features = false`, so name every feature you need.
- Commit messages follow the repo's plain descriptive style (e.g. "Borrow the slice, not the Vec") — no `feat:`/`fix:` prefixes. End with the Claude co-author trailer.
- Run all commands from `service/` inside the worktree.

---

### Task 1: Typed order rejections

A venue saying "no" is an answer, not a transport failure. The scalper must tell "post-only would have crossed" (expected — reprice) apart from "insufficient margin" (stop). Today both drown in `VenueError::Unreachable`.

**Files:**
- Modify: `crates/venue/src/lib.rs` (add one variant to `VenueError`, ~line 400)
- Modify: `crates/hyperliquid/src/lib.rs` (extract `ack_from_status`, add `is_post_only_rejection`)

**Interfaces:**
- Consumes: existing `Status` enum, `OrderAck`, `OrderState` (shown in Task 2's code too).
- Produces: `VenueError::Rejected { message: String }`; `fn ack_from_status(status: Status, client_order_id: &str) -> Result<OrderAck, VenueError>` (private helper, used by Task 2); `pub fn is_post_only_rejection(e: &VenueError) -> bool`.

- [ ] **Step 1: Write the failing tests** (in `crates/hyperliquid/src/lib.rs` `mod tests`)

```rust
#[test]
fn a_venue_rejection_is_a_rejection_not_a_transport_failure() {
    let e = ack_from_status(Status::Error("Insufficient margin".into()), "id-1").unwrap_err();
    assert!(matches!(&e, VenueError::Rejected { message } if message.contains("margin")));
}

#[test]
fn resting_and_filled_statuses_become_acks() {
    let a = ack_from_status(Status::Resting { oid: 77 }, "id-1").unwrap();
    assert_eq!(a.venue_order_id, "77");
    assert_eq!(a.state, OrderState::Open);
    let f = ack_from_status(
        Status::Filled { oid: 9, total_sz: "0.4".into(), avg_px: "64520.0".into() },
        "id-2",
    )
    .unwrap();
    assert_eq!(f.state, OrderState::Filled);
}

#[test]
fn post_only_rejections_are_recognisable() {
    // Exact live wording is confirmed in Task 7's testnet checklist; both known
    // phrasings share "immediately match".
    let e = VenueError::Rejected {
        message: "Post only order would have immediately matched, bbo was 64520@64521".into(),
    };
    assert!(is_post_only_rejection(&e));
    let other = VenueError::Rejected { message: "Insufficient margin".into() };
    assert!(!is_post_only_rejection(&other));
    assert!(!is_post_only_rejection(&VenueError::Unreachable("timeout".into())));
}
```

`OrderState` needs `PartialEq` for these asserts — it already derives it (`venue/src/lib.rs:211`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hyperliquid`
Expected: FAIL — `ack_from_status` and `is_post_only_rejection` not found.

- [ ] **Step 3: Implement**

In `crates/venue/src/lib.rs`, add to `VenueError` (after `UnknownOrder`):

```rust
    /// The venue understood the order and said no. Not a transport failure:
    /// retrying the identical request will get the identical refusal. The
    /// message is the venue's own wording, carried up verbatim.
    #[error("order rejected: {message}")]
    Rejected { message: String },
```

In `crates/hyperliquid/src/lib.rs`, extract from the `match first` block inside `place_order` (lines 300-322) into a free function, and add the classifier:

```rust
/// Turn one per-order venue status into an ack or a typed refusal.
fn ack_from_status(status: Status, client_order_id: &str) -> Result<OrderAck, VenueError> {
    match status {
        Status::Resting { oid } => Ok(OrderAck {
            venue_order_id: oid.to_string(),
            client_order_id: client_order_id.to_string(),
            state: OrderState::Open,
            accepted_at: OffsetDateTime::now_utc(),
        }),
        Status::Filled { oid, .. } => Ok(OrderAck {
            venue_order_id: oid.to_string(),
            client_order_id: client_order_id.to_string(),
            state: OrderState::Filled,
            accepted_at: OffsetDateTime::now_utc(),
        }),
        Status::Error(msg) => Err(VenueError::Rejected { message: msg }),
    }
}

/// Whether a refusal is the *expected* one for an Alo order that would have
/// taken liquidity. This one is not an error to a scalper — it means "reprice".
pub fn is_post_only_rejection(e: &VenueError) -> bool {
    matches!(e, VenueError::Rejected { message } if message.contains("immediately match"))
}
```

In `place_order`, replace the `match first { ... }` block with `ack_from_status(first, &order.client_order_id)`.

- [ ] **Step 4: Run the whole workspace test suite**

Run: `cargo test`
Expected: PASS (239 baseline tests + 3 new). The executor's error paths treat all `VenueError`s alike, so the changed error variant for rejections cannot break it, but the full run proves it.

- [ ] **Step 5: Commit**

```bash
git add crates/venue/src/lib.rs crates/hyperliquid/src/lib.rs
git commit -m "Name a venue rejection instead of calling it unreachable"
```

---

### Task 2: Post-only, reduce-only, and explicit time-in-force

**Files:**
- Modify: `crates/hyperliquid/src/lib.rs`

**Interfaces:**
- Consumes: `ack_from_status` from Task 1; existing `px_string`, `cloid`, `asset_index`, `exchange`.
- Produces:
  - `pub enum Tif { Gtc, Ioc, Alo }`
  - `pub struct OrderOpts { pub tif: Tif, pub reduce_only: bool }`
  - `pub async fn Hyperliquid::place_order_opts(&self, order: &venue::OrderRequest, opts: OrderOpts) -> Result<venue::OrderAck, venue::VenueError>` — inherent method, NOT on the `VenueAdapter` trait. The scalper drives `Hyperliquid` directly (spec: Approach A), so the trait and the `plan` crate stay untouched.
  - `fn order_action(index: u32, is_buy: bool, limit_px: Decimal, qty: Decimal, opts: OrderOpts, client_order_id: &str) -> serde_json::Value` (private, pure).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_order_action_carries_tif_and_reduce_only() {
    let a = order_action(
        7,
        true,
        "64520.1".parse().unwrap(),
        "0.4".parse().unwrap(),
        OrderOpts { tif: Tif::Alo, reduce_only: true },
        "my-id",
    );
    let o = &a["orders"][0];
    assert_eq!(o["a"], 7);
    assert_eq!(o["b"], true);
    assert_eq!(o["p"], "64520");
    assert_eq!(o["s"], "0.4");
    assert_eq!(o["r"], true);
    assert_eq!(o["t"]["limit"]["tif"], "Alo");
    assert_eq!(o["c"], serde_json::json!(cloid("my-id")));
    assert_eq!(a["grouping"], "na");
}

#[test]
fn every_tif_spells_itself_the_way_the_venue_does() {
    assert_eq!(Tif::Gtc.as_str(), "Gtc");
    assert_eq!(Tif::Ioc.as_str(), "Ioc");
    assert_eq!(Tif::Alo.as_str(), "Alo");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hyperliquid`
Expected: FAIL — `order_action`, `OrderOpts`, `Tif` not found.

- [ ] **Step 3: Implement**

```rust
/// Time-in-force, spelled the way the venue spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tif {
    /// Rest until cancelled.
    Gtc,
    /// Fill what crosses now, cancel the remainder.
    Ioc,
    /// Add-liquidity-only: refused rather than allowed to take. The maker order.
    Alo,
}

impl Tif {
    pub fn as_str(self) -> &'static str {
        match self {
            Tif::Gtc => "Gtc",
            Tif::Ioc => "Ioc",
            Tif::Alo => "Alo",
        }
    }
}

/// Execution options beyond what `plan::OrderType` can express.
#[derive(Debug, Clone, Copy)]
pub struct OrderOpts {
    pub tif: Tif,
    /// May only shrink an existing position — never grow or flip it. What
    /// makes a stop-exit safe to fire twice.
    pub reduce_only: bool,
}

fn order_action(
    index: u32,
    is_buy: bool,
    limit_px: Decimal,
    qty: Decimal,
    opts: OrderOpts,
    client_order_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "order",
        "orders": [{
            "a": index,
            "b": is_buy,
            "p": px_string(limit_px),
            "s": qty.normalize().to_string(),
            "r": opts.reduce_only,
            "t": {"limit": {"tif": opts.tif.as_str()}},
            "c": cloid(client_order_id),
        }],
        "grouping": "na",
    })
}
```

Then add the inherent method on `impl Hyperliquid` and shrink the trait method to a delegation. Move the body of the current trait `place_order` into `place_order_opts`, with the action built by `order_action`:

```rust
impl Hyperliquid {
    /// Place an order with explicit execution options. The scalper's path.
    ///
    /// `order.limit_price = None` keeps the market-order behavior of the trait
    /// method: an aggressive limit capped at mark ±5%.
    pub async fn place_order_opts(
        &self,
        order: &OrderRequest,
        opts: OrderOpts,
    ) -> Result<OrderAck, VenueError> {
        let index = self.asset_index(&order.asset).await?;
        let is_buy = matches!(order.side, Side::Buy);
        let limit_px = match order.limit_price {
            Some(p) => p,
            None => {
                let mark = self.mark_price(&order.asset).await?;
                let slip = Decimal::new(5, 2);
                if is_buy {
                    mark * (Decimal::ONE + slip)
                } else {
                    mark * (Decimal::ONE - slip)
                }
            }
        };
        let action = order_action(index, is_buy, limit_px, order.qty, opts, &order.client_order_id);
        let res = self.exchange(action).await?;
        let statuses = res.statuses().map_err(VenueError::Unreachable)?;
        let first = statuses
            .into_iter()
            .next()
            .ok_or_else(|| VenueError::Unreachable("the venue accepted nothing".into()))?;
        ack_from_status(first, &order.client_order_id)
    }
}
```

Trait method becomes:

```rust
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        let opts = OrderOpts {
            tif: if order.limit_price.is_some() { Tif::Gtc } else { Tif::Ioc },
            reduce_only: false,
        };
        self.place_order_opts(order, opts).await
    }
```

Export the new names in the crate root's `pub use` if you placed them in a submodule (they live in `lib.rs`, so plain `pub` is enough).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hyperliquid && cargo test`
Expected: PASS everywhere; the delegation preserves the trait method's observable behavior exactly (same JSON produced for the same inputs — the `order_action` test in Step 1 pins `"r": false`→was hardcoded, now defaulted by the caller).

- [ ] **Step 5: Commit**

```bash
git add crates/hyperliquid/src/lib.rs
git commit -m "Let an order say maker-only and reduce-only"
```

---

### Task 3: Cancel by client order id

The current `cancel_order` needs the venue's oid plus a REST round trip to rediscover the asset index. The scalper cancels its own resting entries by the id it chose.

**Files:**
- Modify: `crates/hyperliquid/src/lib.rs`

**Interfaces:**
- Consumes: `cloid`, `asset_index`, `exchange`, `VenueError::Rejected` (Task 1).
- Produces:
  - `pub async fn Hyperliquid::cancel_by_cloid(&self, asset: &str, client_order_id: &str) -> Result<(), VenueError>`
  - `fn cancel_by_cloid_action(index: u32, client_order_id: &str) -> serde_json::Value` (private, pure)
  - `fn cancel_outcome(statuses: &[serde_json::Value]) -> Result<(), VenueError>` (private, pure)

- [ ] **Step 1: Write the failing tests**

The venue's cancel response statuses are the literal string `"success"` per cancelled order, or an object `{"error": "..."}`. Note `ExchangeResponse::statuses()` would misread the string as `Status::Error` — so cancels get their own outcome reader over the raw values.

```rust
#[test]
fn the_cancel_by_cloid_action_names_the_asset_and_the_hashed_id() {
    let a = cancel_by_cloid_action(7, "my-id");
    assert_eq!(a["type"], "cancelByCloid");
    assert_eq!(a["cancels"][0]["asset"], 7);
    assert_eq!(a["cancels"][0]["cloid"], serde_json::json!(cloid("my-id")));
}

#[test]
fn a_successful_cancel_is_ok_and_a_refused_one_says_why() {
    assert!(cancel_outcome(&[serde_json::json!("success")]).is_ok());
    let e = cancel_outcome(&[serde_json::json!({"error": "Order was never placed, already canceled, or filled."})])
        .unwrap_err();
    assert!(matches!(&e, VenueError::Rejected { message } if message.contains("already canceled")));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hyperliquid`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement**

```rust
fn cancel_by_cloid_action(index: u32, client_order_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "cancelByCloid",
        "cancels": [{"asset": index, "cloid": cloid(client_order_id)}],
    })
}

/// Cancel statuses are the string `"success"` or `{"error": ...}` — a shape
/// `ExchangeResponse::statuses()` was not built for, so they are read here.
fn cancel_outcome(statuses: &[serde_json::Value]) -> Result<(), VenueError> {
    for s in statuses {
        if let Some(msg) = s.get("error").and_then(|v| v.as_str()) {
            return Err(VenueError::Rejected { message: msg.to_string() });
        }
    }
    Ok(())
}

impl Hyperliquid {
    /// Cancel a resting order by the id *we* chose, with no oid lookup.
    ///
    /// A cancel refused because the order is already gone comes back as
    /// [`VenueError::Rejected`]; for the scalper's cancel-then-reprice loop
    /// that usually means "it filled while you decided", and the fills feed
    /// settles which.
    pub async fn cancel_by_cloid(
        &self,
        asset: &str,
        client_order_id: &str,
    ) -> Result<(), VenueError> {
        let index = self.asset_index(asset).await?;
        let res = self.exchange(cancel_by_cloid_action(index, client_order_id)).await?;
        if res.status != "ok" {
            return Err(VenueError::Unreachable(format!(
                "cancel refused: venue returned status {}",
                res.status
            )));
        }
        let data = res
            .response
            .as_ref()
            .and_then(|r| r.data.as_ref())
            .ok_or_else(|| VenueError::Unreachable("no data in the venue's response".into()))?;
        cancel_outcome(&data.statuses)
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hyperliquid`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hyperliquid/src/lib.rs
git commit -m "Cancel by the id we chose, not the id we must look up"
```

---

### Task 4: Capture live WebSocket fixtures

House rule: response shapes come from captures, not from documentation. This task produces the fixtures Task 5's parser is tested against.

**Files:**
- Create: `crates/hyperliquid/examples/ws_capture.rs`
- Create: `crates/hyperliquid/tests/fixtures/ws/candle.json`, `bbo.json`, `user_fills.json`, `subscription_response.json`, `pong.json` (created by running the example)
- Modify: `crates/hyperliquid/Cargo.toml` (add deps)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: fixture files; `pub fn ws_url(base: &str) -> String` in a new `crates/hyperliquid/src/ws.rs` (the one production symbol this task introduces, so the example and Task 6 share it).

- [ ] **Step 1: Add dependencies**

In `crates/hyperliquid/Cargo.toml` `[dependencies]`:

```toml
# Default features kept: "connect" and "handshake" are both needed
# (connect_async and, in tests, accept_async).
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
futures-util = "0.3"
```

and change the existing tokio line to include the features the WS task needs (`rt` for `tokio::spawn`, `sync` for mpsc):

```toml
tokio = { workspace = true, features = ["time", "rt", "sync"] }
```

`[dev-dependencies]` tokio gains `rt-multi-thread` (for the example and Task 6's mock-server test):

```toml
tokio = { workspace = true, features = ["rt", "rt-multi-thread", "macros", "net", "time", "sync"] }
```

- [ ] **Step 2: Write `ws_url` with its test, in a new `crates/hyperliquid/src/ws.rs`**

Add `pub mod ws;` to `lib.rs` (after `mod sign;`).

```rust
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
```

Run: `cargo test -p hyperliquid` — the new test passes, everything still green.

- [ ] **Step 3: Write the capture example**

```rust
//! Capture raw WS frames for the fixture set. Run with an output directory:
//!
//!     cargo run -p hyperliquid --example ws_capture -- crates/hyperliquid/tests/fixtures/ws
//!
//! Subscribes to BTC 1m candles, BTC bbo, and userFills for an arbitrary
//! address (fills snapshot arrives even when empty). Writes the first frame
//! seen per channel and exits once it has one of each (or after 90s).

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
        serde_json::json!({"type": "userFills", "user": "0x0000000000000000000000000000000000000001"}),
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
```

- [ ] **Step 4: Run the capture against mainnet**

Run (from `service/`): `cargo run -p hyperliquid --example ws_capture -- crates/hyperliquid/tests/fixtures/ws`
Expected: five `captured <channel>` lines within ~90s (candle frames arrive on trades and at minute close; bbo on every top-of-book change). Inspect each file — they must each contain a `"channel"` field and a `"data"` payload. If `userFills` arrives only as a subscription confirmation with no data frame, re-run with a busy address taken from any recent trade on https://app.hyperliquid.xyz — the snapshot frame (`"isSnapshot": true`) counts.

- [ ] **Step 5: Commit**

```bash
git add crates/hyperliquid/Cargo.toml crates/hyperliquid/src/ws.rs crates/hyperliquid/src/lib.rs \
        crates/hyperliquid/examples/ws_capture.rs crates/hyperliquid/tests/fixtures/ws
git commit -m "Capture the venue's streaming frames as fixtures"
```

---

### Task 5: WS frame parsing

**Files:**
- Modify: `crates/hyperliquid/src/ws.rs`
- Test: same file, `mod tests`, reading `tests/fixtures/ws/*.json`

**Interfaces:**
- Consumes: fixtures from Task 4; `crate::Candle` (the REST candle struct — the WS candle payload has the same shape, `hyperliquid/src/info.rs:554`).
- Produces (all `pub` in `hyperliquid::ws`):
  - `enum WsEvent { Candle(crate::Candle), Bbo(Bbo), UserFills(UserFills), SubscriptionResponse, Pong, Other(String) }`
  - `struct Bbo { coin: String, time: i64, bbo: [Option<BboLevel>; 2] }` — index 0 bid, 1 ask; a side can be empty.
  - `struct BboLevel { px: String, sz: String, n: u64 }`
  - `struct UserFills { is_snapshot: bool, user: String, fills: Vec<WsFill> }`
  - `struct WsFill { coin: String, px: String, sz: String, side: String, time: i64, oid: u64, tid: u64, fee: String, fee_token: String, cloid: Option<String>, crossed: bool }` — `side` is the venue's `"B"`/`"A"`.
  - `fn parse_frame(text: &str) -> Result<WsEvent, serde_json::Error>`

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hyperliquid ws::`
Expected: FAIL — types and `parse_frame` not found.

- [ ] **Step 3: Implement in `ws.rs`**

```rust
use serde::Deserialize;

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
```

If a fixture disagrees with a struct (field missing, different casing), the fixture wins: adjust the struct, never the fixture.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hyperliquid`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hyperliquid/src/ws.rs
git commit -m "Parse the streaming frames the fixtures vouch for"
```

---

### Task 6: The connection task — reconnect, resubscribe, ping

**Files:**
- Modify: `crates/hyperliquid/src/ws.rs`
- Test: `crates/hyperliquid/tests/ws_client.rs` (integration test with a local mock WS server)

**Interfaces:**
- Consumes: `parse_frame`, `WsEvent`, `ws_url`.
- Produces (all `pub` in `hyperliquid::ws`):
  - `enum Subscription { Candle { coin: String, interval: String }, Bbo { coin: String }, UserFills { user: String } }` with `fn to_json(&self) -> serde_json::Value`
  - `enum WsMessage { Connected, Event(WsEvent), Disconnected }` — `Connected` arrives after every successful (re)connect+subscribe; the consumer gap-fills candles over REST when it sees one that isn't the first.
  - `struct WsConfig { pub ping_interval: std::time::Duration, pub backoff_start: std::time::Duration, pub backoff_cap: std::time::Duration }` with `impl Default` (30s, 1s, 60s)
  - `fn spawn(url: String, subs: Vec<Subscription>, cfg: WsConfig) -> tokio::sync::mpsc::Receiver<WsMessage>` — spawns the connection task, returns the event channel (capacity 256). Task exits when the receiver is dropped.

- [ ] **Step 1: Write the failing integration test**

`crates/hyperliquid/tests/ws_client.rs`:

```rust
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
```

Note: the server side (`accept_async`) comes with tokio-tungstenite's default `handshake` feature, which Task 4's dependency line keeps enabled.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hyperliquid --test ws_client`
Expected: FAIL — `Subscription`, `WsConfig`, `WsMessage`, `spawn` not found.

- [ ] **Step 3: Implement in `ws.rs`**

```rust
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

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
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            // The venue closes sockets idle for 60s; ping at half that.
            ping_interval: std::time::Duration::from_secs(30),
            backoff_start: std::time::Duration::from_secs(1),
            backoff_cap: std::time::Duration::from_secs(60),
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
        tokio::time::sleep(backoff).await;
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
    let Ok((mut socket, _)) = tokio_tungstenite::connect_async(url).await else {
        return SessionEnd::ConnectFailed;
    };
    for sub in subs {
        let msg = serde_json::json!({"method": "subscribe", "subscription": sub.to_json()});
        if socket.send(Message::Text(msg.to_string())).await.is_err() {
            return SessionEnd::ConnectionLost;
        }
    }
    if tx.send(WsMessage::Connected).await.is_err() {
        return SessionEnd::ReceiverDropped;
    }

    let mut ping = tokio::time::interval(cfg.ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await; // the immediate first tick; pings start one interval in

    loop {
        tokio::select! {
            _ = ping.tick() => {
                let p = serde_json::json!({"method": "ping"}).to_string();
                if socket.send(Message::Text(p)).await.is_err() {
                    return SessionEnd::ConnectionLost;
                }
            }
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let event = match parse_frame(&text) {
                        Ok(e) => e,
                        Err(_) => WsEvent::Other(format!("unparseable: {text}")),
                    };
                    if tx.send(WsMessage::Event(event)).await.is_err() {
                        return SessionEnd::ReceiverDropped;
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    if socket.send(Message::Pong(p)).await.is_err() {
                        return SessionEnd::ConnectionLost;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return SessionEnd::ConnectionLost,
            },
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hyperliquid`
Expected: PASS, including the reconnect integration test.

- [ ] **Step 5: Commit**

```bash
git add crates/hyperliquid/src/ws.rs crates/hyperliquid/tests/ws_client.rs crates/hyperliquid/Cargo.toml
git commit -m "Hold the stream open and get back on when it drops"
```

---

### Task 7: Live smoke — `ws_watch` example and the testnet order checklist

**Files:**
- Create: `crates/hyperliquid/examples/ws_watch.rs`

**Interfaces:**
- Consumes: `ws::spawn`, `ws::Subscription`, `ws::WsConfig`, `ws::WsMessage`.
- Produces: a manual verification tool; no library symbols.

- [ ] **Step 1: Write the example**

```rust
//! Watch the live stream for a coin. A manual smoke test for the WS module:
//!
//!     cargo run -p hyperliquid --example ws_watch -- BTC 60
//!
//! Prints one line per candle close and a bbo line at most once a second.

use hyperliquid::ws::{self, Subscription, WsConfig, WsEvent, WsMessage};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let coin = std::env::args().nth(1).unwrap_or_else(|| "BTC".into());
    let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let subs = vec![
        Subscription::Candle { coin: coin.clone(), interval: "1m".into() },
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
                println!("candle {} o={} h={} l={} c={} v={} n={}", c.s, c.o, c.h, c.l, c.c, c.v, c.n)
            }
            Some(WsMessage::Event(WsEvent::Bbo(b))) => {
                if last_bbo.elapsed() >= std::time::Duration::from_secs(1) {
                    let px = |l: &Option<ws::BboLevel>| {
                        l.as_ref().map(|l| l.px.clone()).unwrap_or_else(|| "-".into())
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
```

- [ ] **Step 2: Run it against mainnet**

Run: `cargo run -p hyperliquid --example ws_watch -- BTC 90`
Expected: `connected`, a steady trickle of `bbo` lines, at least one `candle` line within 90s. This is the end-to-end proof the module speaks to the real venue.

- [ ] **Step 3: Record the testnet order checklist**

The new order paths (`Alo`, `reduce_only`, `cancel_by_cloid`) can only be proven against a funded account. Append this checklist to the bottom of this plan file (it runs when the user has an HL **testnet** account with an approved agent key; testnet needs no `HL_ALLOW_LIVE`):

```markdown
## Testnet verification checklist (manual, needs testnet funds)

Using `Hyperliquid::trading(TESTNET, account, agent_key, None, false)` from a
scratch binary or test marked `#[ignore]`:

- [ ] Alo far from the touch → resting ack; `cancel_by_cloid` on it → Ok(())
- [ ] Alo priced through the touch → `VenueError::Rejected` and
      `is_post_only_rejection` returns true; record the exact message wording
      in a comment next to `is_post_only_rejection`
- [ ] `reduce_only: true` with no position → `VenueError::Rejected` (records wording)
- [ ] `cancel_by_cloid` for an id that never existed → `VenueError::Rejected`
      containing "never placed"
- [ ] Ioc limit priced mid-book → partial or no fill, never resting
```

- [ ] **Step 4: Run the full suite one last time**

Run: `cargo test`
Expected: PASS — baseline plus everything this plan added.

- [ ] **Step 5: Commit**

```bash
git add crates/hyperliquid/examples/ws_watch.rs docs/superpowers/plans/2026-08-12-crypto-scalper-plan-1-venue-foundations.md
git commit -m "Watch the live stream and write down what testnet must prove"
```

## Testnet verification checklist (manual, needs testnet funds)

Using `Hyperliquid::trading(TESTNET, account, agent_key, None, false)` from a
scratch binary or test marked `#[ignore]`:

- [ ] Alo far from the touch → resting ack; `cancel_by_cloid` on it → Ok(())
- [ ] Alo priced through the touch → `VenueError::Rejected` and
      `is_post_only_rejection` returns true; record the exact message wording
      in a comment next to `is_post_only_rejection`
- [ ] `reduce_only: true` with no position → `VenueError::Rejected` (records wording)
- [ ] `cancel_by_cloid` for an id that never existed → `VenueError::Rejected`
      containing "never placed"
- [ ] Ioc limit priced mid-book → partial or no fill, never resting
