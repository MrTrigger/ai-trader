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

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        let index = self.asset_index(&order.asset).await?;
        let is_buy = matches!(order.side, Side::Buy);

        // A market order is sent as an aggressive limit — Hyperliquid has no
        // unconditional market type, and an "IOC at whatever" would be a blank
        // cheque. The cap is the mark moved 5% against us: wide enough to cross
        // a normal book, narrow enough that a broken feed or a flash move
        // cannot fill us anywhere.
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

        let tif = if order.limit_price.is_some() {
            "Gtc"
        } else {
            "Ioc"
        };
        let action = serde_json::json!({
            "type": "order",
            "orders": [{
                "a": index,
                "b": is_buy,
                "p": px_string(limit_px),
                "s": order.qty.normalize().to_string(),
                "r": false,
                "t": {"limit": {"tif": tif}},
                "c": cloid(&order.client_order_id),
            }],
            "grouping": "na",
        });

        let res = self.exchange(action).await?;
        let statuses = res.statuses().map_err(VenueError::Unreachable)?;
        let first = statuses
            .into_iter()
            .next()
            .ok_or_else(|| VenueError::Unreachable("the venue accepted nothing".into()))?;

        match first {
            Status::Resting { oid } => Ok(OrderAck {
                venue_order_id: oid.to_string(),
                client_order_id: order.client_order_id.clone(),
                state: OrderState::Open,
                accepted_at: OffsetDateTime::now_utc(),
            }),
            Status::Filled { oid, .. } => Ok(OrderAck {
                venue_order_id: oid.to_string(),
                client_order_id: order.client_order_id.clone(),
                state: OrderState::Filled,
                accepted_at: OffsetDateTime::now_utc(),
            }),
            // A rejection is an answer, not a transport failure. It must not be
            // retried, and the venue's own words are the most useful thing to
            // carry up.
            Status::Error(msg) => Err(VenueError::InsufficientBalance {
                currency: QUOTE.into(),
                need: order.qty,
                available: Decimal::ZERO,
            })
            .map_err(|_: VenueError| VenueError::Unreachable(format!("order rejected: {msg}"))),
        }
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
}
