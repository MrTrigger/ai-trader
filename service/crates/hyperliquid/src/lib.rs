//! The Hyperliquid venue adapter.
//!
//! Two halves, deliberately separable:
//!
//! - [`Info`] is public. Marks, markets, candles, and any account's balances,
//!   positions, fills and resting orders. No credentials, and nothing in it can
//!   place an order. This is what makes a *paper* run real — the fake broker
//!   fills against live prices instead of a hand-written file.
//! - [`Hyperliquid`] adds the write path, and only if given an agent key.
//!
//! # Read-only is a first-class mode, not a degraded one
//!
//! Constructed without a key, the adapter answers every read and refuses every
//! write with [`VenueError::Unreachable`] naming the missing credential. That is
//! the state to run in until Phase 2's gate is passed, and it is the default.
//!
//! # Live is gated twice, in different places
//!
//! A key alone is not enough. The caller must also have opted in explicitly
//! (`HL_ALLOW_LIVE=yes`), because a config copied between machines, or a stray
//! `--venue live`, must not be sufficient to start trading real money. One of
//! those gates lives in the environment and the other in a config file, so a
//! mistake in either place fails closed.
//!
//! # What is verified and what is not
//!
//! Every response shape here was captured from the live API, and the fixtures
//! in `tests/fixtures/` are those captures. One exception is called out where it
//! occurs: no account reachable at the time of writing held an **open position**,
//! so the `assetPositions` parse is written to the documented shape and covered
//! only by a synthetic fixture. The first `bot reconcile` against a funded
//! account is what confirms it, and it fails loudly rather than silently if the
//! shape is wrong.

mod info;
mod sign;
pub mod ws;

pub use info::{
    ApprovedAgent, Candle, Info, L2Book, L2Level, MAINNET, QUOTE, QUOTE_TOKEN, TESTNET,
};
pub use sign::{Agent, SignError};

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::Deserialize;
use time::OffsetDateTime;
use venue::{
    Balance, Fill, Market, OpenOrder, OrderAck, OrderRequest, OrderState, Position, PriceSource,
    Side, VenueAdapter, VenueError,
};

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

/// How the adapter was configured, and therefore what it may do.
pub struct Hyperliquid {
    info: Info,
    base: String,
    /// The account whose book we are trading. Public.
    account: String,
    vault: Option<String>,
    /// Absent means read-only, and every write says so.
    agent: Option<Agent>,
    mainnet: bool,
    http: reqwest::Client,
}

impl std::fmt::Debug for Hyperliquid {
    /// Hand-written so a key can never reach a log through a derived `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hyperliquid")
            .field("base", &self.base)
            .field("account", &self.account)
            .field("vault", &self.vault)
            .field("can_trade", &self.agent.is_some())
            .finish()
    }
}

impl Hyperliquid {
    /// Read-only. Answers every query, refuses every order.
    pub fn read_only(base: &str, account: &str) -> Result<Self, VenueError> {
        Ok(Self {
            info: Info::new(base)?,
            base: base.to_string(),
            account: account.to_string(),
            vault: None,
            agent: None,
            mainnet: base == MAINNET,
            http: client()?,
        })
    }

    /// Able to trade.
    ///
    /// `allow_live` is the second gate and it is not advisory: on mainnet,
    /// without it, this refuses to construct. The caller reads it from the
    /// environment so that a config file alone can never be enough.
    pub fn trading(
        base: &str,
        account: &str,
        agent_key: &str,
        vault: Option<String>,
        allow_live: bool,
    ) -> Result<Self, VenueError> {
        let mainnet = base == MAINNET;
        if mainnet && !allow_live {
            return Err(VenueError::Unreachable(
                "refusing to build a mainnet trading client without an explicit opt-in. Set \
                 HL_ALLOW_LIVE=yes once you mean it; a config file on its own is not enough to \
                 start trading real money."
                    .into(),
            ));
        }
        let agent =
            Agent::from_hex(agent_key).map_err(|e| VenueError::Unreachable(e.to_string()))?;
        Ok(Self {
            info: Info::new(base)?,
            base: base.to_string(),
            account: account.to_string(),
            vault,
            agent: Some(agent),
            mainnet,
            http: client()?,
        })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn can_trade(&self) -> bool {
        self.agent.is_some()
    }

    pub fn is_mainnet(&self) -> bool {
        self.mainnet
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// The agent address the venue will attribute our orders to. Useful in a
    /// startup line, because an expired or unapproved agent is the most common
    /// reason a first live order is rejected.
    pub fn agent_address(&self) -> Option<&str> {
        self.agent.as_ref().map(|a| a.address())
    }

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

    fn agent_or_refuse(&self) -> Result<&Agent, VenueError> {
        self.agent.as_ref().ok_or_else(|| {
            VenueError::Unreachable(
                "this venue was opened read-only: no agent key was supplied, so it can show the \
                 account but not trade it. Set HL_AGENT_PRIVATE_KEY to enable orders."
                    .into(),
            )
        })
    }

    /// Post a signed action to `/exchange`.
    async fn exchange(&self, action: serde_json::Value) -> Result<ExchangeResponse, VenueError> {
        let agent = self.agent_or_refuse()?;
        // The nonce must rise, and the venue rejects one far from its clock.
        // Milliseconds since the epoch satisfies both without any state to keep.
        let nonce = OffsetDateTime::now_utc().unix_timestamp_nanos() as u64 / 1_000_000;
        let sig = agent
            .sign_action(&action, nonce, self.vault.as_deref(), self.mainnet)
            .map_err(|e| VenueError::Unreachable(e.to_string()))?;

        let mut body = serde_json::json!({
            "action": action,
            "nonce": nonce,
            "signature": sig,
        });
        if let Some(v) = &self.vault {
            body["vaultAddress"] = serde_json::json!(v);
        }

        let url = format!("{}/exchange", self.base);
        let res = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| VenueError::Unreachable(format!("{url}: {e}")))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| VenueError::Unreachable(format!("{url}: {e}")))?;
        if !status.is_success() {
            return Err(VenueError::Unreachable(format!(
                "{url}: HTTP {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            VenueError::Unreachable(format!("{url}: cannot read the response ({e}): {text}"))
        })
    }

    /// The venue's numeric index for a coin. Orders are placed by index.
    async fn asset_index(&self, coin: &str) -> Result<u32, VenueError> {
        let markets = self.info.meta().await?;
        markets
            .iter()
            .position(|m| m.asset == coin)
            .map(|i| i as u32)
            .ok_or_else(|| VenueError::UnknownMarket(coin.to_string()))
    }
}

fn client() -> Result<reqwest::Client, VenueError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("ai-trader/0.1")
        .build()
        .map_err(|e| VenueError::Unreachable(e.to_string()))
}

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

#[async_trait]
impl PriceSource for Hyperliquid {
    async fn mark_price(&self, asset: &str) -> Result<Decimal, VenueError> {
        self.info
            .marks()
            .await?
            .get(asset)
            .copied()
            .ok_or_else(|| VenueError::NoPrice(asset.to_string()))
    }
}

#[async_trait]
impl VenueAdapter for Hyperliquid {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        self.info.meta().await
    }

    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        self.info.balances(&self.account).await
    }

    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        self.info.positions(&self.account).await
    }

    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError> {
        self.info.open_orders(&self.account).await
    }

    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        self.info.fills(&self.account, since).await
    }

    async fn get_order_book(&self, asset: &str) -> Result<venue::OrderBook, VenueError> {
        let book = self.info.l2_book(asset).await?;
        let side = |i: usize| -> Vec<venue::BookLevel> {
            book.levels
                .get(i)
                .map(|ls| {
                    ls.iter()
                        .filter_map(|l| {
                            Some(venue::BookLevel {
                                price: l.px.parse().ok()?,
                                qty: l.sz.parse().ok()?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(venue::OrderBook {
            bids: side(0),
            asks: side(1),
        })
    }

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        let opts = OrderOpts {
            tif: if order.limit_price.is_some() { Tif::Gtc } else { Tif::Ioc },
            reduce_only: false,
        };
        self.place_order_opts(order, opts).await
    }

    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), VenueError> {
        let oid: u64 = venue_order_id
            .parse()
            .map_err(|_| VenueError::UnknownOrder(venue_order_id.to_string()))?;
        // Cancelling needs the asset index too, and the id alone does not carry
        // it, so the resting order is looked up. An order that is already gone
        // is an error rather than a no-op: it means our view is wrong.
        let open = self.get_open_orders().await?;
        let found = open
            .iter()
            .find(|o| o.venue_order_id == venue_order_id)
            .ok_or_else(|| VenueError::UnknownOrder(venue_order_id.to_string()))?;
        let index = self.asset_index(&found.asset).await?;

        let action = serde_json::json!({
            "type": "cancel",
            "cancels": [{"a": index, "o": oid}],
        });
        let res = self.exchange(action).await?;
        match res.statuses() {
            Ok(_) => Ok(()),
            Err(e) => Err(VenueError::Unreachable(format!("cancel refused: {e}"))),
        }
    }
}

/// Hyperliquid prices carry at most 5 significant figures. Sending more is
/// rejected, so the string is built to that rule rather than trusting the
/// Decimal's own formatting.
fn px_string(p: Decimal) -> String {
    let mut s = p.round_sf(5).unwrap_or(p).normalize();
    s.rescale(s.scale().min(6));
    s.normalize().to_string()
}

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

/// The venue's client order id is a 128-bit hex value, not free text. Ours is a
/// readable string, so it is hashed into that space — deterministically, so a
/// replay still produces the same id and the venue's idempotency still holds.
fn cloid(id: &str) -> String {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(id.as_bytes());
    let out = h.finalize();
    format!(
        "0x{}",
        out[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
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

#[derive(Debug, Deserialize)]
pub struct ExchangeResponse {
    pub status: String,
    #[serde(default)]
    pub response: Option<ResponseBody>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseBody {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub data: Option<ResponseData>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseData {
    #[serde(default)]
    pub statuses: Vec<serde_json::Value>,
}

#[derive(Debug)]
pub enum Status {
    Resting {
        oid: u64,
    },
    Filled {
        oid: u64,
        total_sz: String,
        avg_px: String,
    },
    Error(String),
}

impl ExchangeResponse {
    /// Flatten the venue's nested reply into per-order outcomes.
    ///
    /// A top-level `err` status means the whole action was refused, which is
    /// different from one order in a batch being rejected, and the two must not
    /// be reported the same way.
    pub fn statuses(&self) -> Result<Vec<Status>, String> {
        if self.status != "ok" {
            return Err(format!("venue returned status {}", self.status));
        }
        let data = self
            .response
            .as_ref()
            .and_then(|r| r.data.as_ref())
            .ok_or_else(|| "no data in the venue's response".to_string())?;
        Ok(data
            .statuses
            .iter()
            .map(|s| {
                if let Some(e) = s.get("error").and_then(|v| v.as_str()) {
                    return Status::Error(e.to_string());
                }
                if let Some(r) = s.get("resting") {
                    if let Some(oid) = r.get("oid").and_then(|v| v.as_u64()) {
                        return Status::Resting { oid };
                    }
                }
                if let Some(f) = s.get("filled") {
                    if let Some(oid) = f.get("oid").and_then(|v| v.as_u64()) {
                        return Status::Filled {
                            oid,
                            total_sz: f
                                .get("totalSz")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .into(),
                            avg_px: f.get("avgPx").and_then(|v| v.as_str()).unwrap_or("").into(),
                        };
                    }
                }
                Status::Error(s.to_string())
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_only_client_refuses_to_trade_and_says_why() {
        let v =
            Hyperliquid::read_only(MAINNET, "0x0000000000000000000000000000000000000000").unwrap();
        assert!(!v.can_trade());
        // `unwrap_err` is unavailable here on purpose: it would need `Debug` on
        // the Ok side, and `Agent` deliberately has none so a key cannot reach
        // a log through a derived impl.
        let e = match v.agent_or_refuse() {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a read-only client produced an agent"),
        };
        assert!(e.contains("read-only"), "{e}");
        assert!(e.contains("HL_AGENT_PRIVATE_KEY"), "names the fix: {e}");
    }

    #[test]
    fn mainnet_trading_refuses_without_the_explicit_opt_in() {
        // The second gate. A key on its own must not be enough, because a
        // config copied between machines carries the key with it.
        const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let e = Hyperliquid::trading(MAINNET, "0xabc", KEY, None, false).unwrap_err();
        assert!(e.to_string().contains("HL_ALLOW_LIVE"), "{e}");
        // Testnet is not gated: that is where you are supposed to be.
        assert!(Hyperliquid::trading(TESTNET, "0xabc", KEY, None, false).is_ok());
        assert!(Hyperliquid::trading(MAINNET, "0xabc", KEY, None, true).is_ok());
    }

    #[test]
    fn the_debug_impl_cannot_leak_the_key() {
        const KEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let v = Hyperliquid::trading(TESTNET, "0xabc", KEY, None, false).unwrap();
        let shown = format!("{v:?}");
        assert!(!shown.contains("4c0883"), "the key reached a Debug string");
        assert!(shown.contains("can_trade: true"));
    }

    #[test]
    fn prices_are_cut_to_the_five_significant_figures_the_venue_accepts() {
        // Sending more precision is rejected outright, and the executor does
        // not adjust orders, so the adapter has to produce a sendable price.
        assert_eq!(px_string("64520.123456".parse().unwrap()), "64520");
        assert_eq!(px_string("1.23456789".parse().unwrap()), "1.2346");
        assert_eq!(px_string("0.00012345678".parse().unwrap()), "0.000123");
    }

    #[test]
    fn a_client_order_id_maps_into_the_venues_id_space_deterministically() {
        // Idempotency is the crash-safety property, and it only survives the
        // translation if the same readable id always produces the same cloid.
        let a = cloid("5227e5a9-BTC-B-0.4-s1");
        assert_eq!(a, cloid("5227e5a9-BTC-B-0.4-s1"));
        assert_ne!(a, cloid("5227e5a9-BTC-B-0.4-s2"));
        assert_eq!(a.len(), 34, "0x plus 32 hex characters");
    }

    #[test]
    fn a_rejected_order_is_not_reported_as_a_success() {
        let r: ExchangeResponse = serde_json::from_str(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Order price cannot be more than 80% away from the reference price"}]}}}"#,
        )
        .unwrap();
        match r.statuses().unwrap().into_iter().next().unwrap() {
            Status::Error(m) => assert!(m.contains("80%")),
            other => panic!("expected an error status, got {other:?}"),
        }
    }

    #[test]
    fn resting_and_filled_are_told_apart() {
        let resting: ExchangeResponse = serde_json::from_str(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":77}}]}}}"#,
        )
        .unwrap();
        assert!(matches!(
            resting.statuses().unwrap()[0],
            Status::Resting { oid: 77 }
        ));
        let filled: ExchangeResponse = serde_json::from_str(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"oid":9,"totalSz":"0.4","avgPx":"64520.0"}}]}}}"#,
        )
        .unwrap();
        assert!(matches!(
            filled.statuses().unwrap()[0],
            Status::Filled { oid: 9, .. }
        ));
    }

    #[test]
    fn a_whole_action_failing_is_distinct_from_one_order_being_rejected() {
        let r: ExchangeResponse =
            serde_json::from_str(r#"{"status":"err","response":null}"#).unwrap();
        assert!(r.statuses().is_err());
    }

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
}
