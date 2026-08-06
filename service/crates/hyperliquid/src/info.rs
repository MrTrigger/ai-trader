//! The public half of Hyperliquid: everything you can read without a key.
//!
//! Every response shape in this file was captured from the live API rather than
//! recalled, and the fixtures under `tests/fixtures/` are those captures. An
//! adapter written from memory of an exchange's documentation is a guess that
//! compiles.
//!
//! # This is the half that matters first
//!
//! The paper venue is "a fake broker over a *real* price feed", and until now
//! there was no feed — marks came from a hand-written file, so a paper run
//! filled against frozen prices and produced no P&L, no slippage and no
//! drawdown. None of what Phase 2's gate measures. This module is what makes a
//! paper run mean something, and it needs no credentials to do it.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde::Deserialize;
use time::OffsetDateTime;
use venue::{Balance, Capabilities, Fill, Market, OpenOrder, Position, Side, VenueError};

/// Hyperliquid's public endpoints.
pub const MAINNET: &str = "https://api.hyperliquid.xyz";
pub const TESTNET: &str = "https://api.hyperliquid-testnet.xyz";

/// Quote currency for every perp on this venue.
pub const QUOTE: &str = "USDC";
/// Its token index, which is how the unified-account view keys balances.
/// `meta.collateralToken` reports 0, and this is checked against the venue in
/// `live_read::the_collateral_token_is_still_the_one_we_assume`.
pub const QUOTE_TOKEN: u32 = 0;

/// A read-only client. Holds no key and cannot place an order.
#[derive(Debug, Clone)]
pub struct Info {
    base: String,
    http: reqwest::Client,
}

impl Info {
    pub fn new(base: impl Into<String>) -> Result<Self, VenueError> {
        let http = reqwest::Client::builder()
            // A hung request must not hold a trading loop open. Every call here
            // is idempotent, so the runner's retry is safe on a timeout.
            .timeout(std::time::Duration::from_secs(15))
            .user_agent("ai-trader/0.1")
            .build()
            .map_err(|e| VenueError::Unreachable(e.to_string()))?;
        Ok(Self {
            base: base.into(),
            http,
        })
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        body: serde_json::Value,
    ) -> Result<T, VenueError> {
        let url = format!("{}/info", self.base);
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
            // A 4xx or 5xx is the venue being unreachable *for our purposes*,
            // and the runner retries reads. Retrying a rejection would be
            // wrong, but this endpoint has no rejections — it has answers.
            return Err(VenueError::Unreachable(format!(
                "{url}: HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            // Naming the endpoint and quoting the body matters: a shape change
            // at the venue is the most likely cause, and "expected struct" with
            // no context is a bad afternoon.
            VenueError::Unreachable(format!(
                "{url}: cannot read the response ({e}); body began: {}",
                text.chars().take(200).collect::<String>()
            ))
        })
    }

    /// Every perp the venue lists, with its size precision and leverage cap.
    pub async fn meta(&self) -> Result<Vec<Market>, VenueError> {
        let m: MetaResponse = self.post(serde_json::json!({"type": "meta"})).await?;
        Ok(m.universe.iter().map(perp_market).collect())
    }

    /// Mid prices for everything, keyed by coin.
    pub async fn all_mids(&self) -> Result<BTreeMap<String, Decimal>, VenueError> {
        let raw: BTreeMap<String, String> =
            self.post(serde_json::json!({"type": "allMids"})).await?;
        Ok(raw
            .into_iter()
            .filter_map(|(k, v)| v.parse().ok().map(|d| (k, d)))
            .collect())
    }

    /// Mark prices — what the venue values positions at.
    ///
    /// Not the mid. Hyperliquid liquidates and computes unrealised P&L against
    /// the mark, so marking our own book to the mid would put our NAV a tick
    /// away from the number that can actually close us out.
    pub async fn marks(&self) -> Result<BTreeMap<String, Decimal>, VenueError> {
        let (meta, ctxs): (MetaResponse, Vec<AssetCtx>) = self
            .post(serde_json::json!({"type": "metaAndAssetCtxs"}))
            .await?;
        Ok(meta
            .universe
            .iter()
            .zip(ctxs.iter())
            .filter_map(|(u, c)| {
                c.mark_px
                    .as_deref()
                    .and_then(|p| p.parse().ok())
                    .map(|d| (u.name.clone(), d))
            })
            .collect())
    }

    /// Markets and marks together, from one round trip.
    ///
    /// The pair is what a caller actually wants, and asking twice invites the
    /// two to describe different instants.
    pub async fn markets_and_marks(
        &self,
    ) -> Result<(Vec<Market>, BTreeMap<String, Decimal>), VenueError> {
        let (meta, ctxs): (MetaResponse, Vec<AssetCtx>) = self
            .post(serde_json::json!({"type": "metaAndAssetCtxs"}))
            .await?;
        let markets = meta.universe.iter().map(perp_market).collect();
        let marks = meta
            .universe
            .iter()
            .zip(ctxs.iter())
            .filter_map(|(u, c)| {
                c.mark_px
                    .as_deref()
                    .and_then(|p| p.parse().ok())
                    .map(|d| (u.name.clone(), d))
            })
            .collect();
        Ok((markets, marks))
    }

    /// Spendable collateral, whichever kind of account this is.
    ///
    /// # Two kinds of account, and reading only one of them was a bug
    ///
    /// A **classic** account keeps perps and spot in separate pots: perps
    /// collateral is `clearinghouseState.accountValue`, spot is its own balance,
    /// and moving between them is an explicit transfer. A **unified** account
    /// trades from one balance — the UI greys the transfer out and says so —
    /// and the collateral lives on the *spot* side while the perps view reports
    /// zero until a position exists.
    ///
    /// Reading only the perps side therefore reports an empty account for every
    /// unified user, which is how a funded account came back as 0.00.
    ///
    /// The two are told apart by `tokenToAvailableAfterMaintenance`, which the
    /// venue returns only for unified accounts. Verified against three accounts:
    /// a unified one with spot-only collateral, and two classic ones with
    /// separately funded perps.
    ///
    /// What is **not** verified is a unified account holding an open perp
    /// position — nothing reachable had one. If the perps view starts reporting
    /// the same collateral the spot side already does, this would double count,
    /// so it deliberately does not add the two together, and
    /// `live_read::a_unified_account_is_not_double_counted` fails loudly if that
    /// assumption ever stops holding.
    pub async fn balances(&self, user: &str) -> Result<Vec<Balance>, VenueError> {
        let perps: ClearinghouseState = self
            .post(serde_json::json!({"type": "clearinghouseState", "user": user}))
            .await?;
        let spot: SpotState = self
            .post(serde_json::json!({"type": "spotClearinghouseState", "user": user}))
            .await?;

        let dec = |s: &str| s.parse::<Decimal>().unwrap_or_default();

        if let Some(avail) = &spot.token_to_available_after_maintenance {
            // Unified: one pot, held on the spot side. `available after
            // maintenance` is the venue's own answer to "how much of this can
            // still back a trade", which is exactly what `available` means.
            let total = spot
                .balances
                .iter()
                .find(|b| b.coin == QUOTE)
                .map(|b| dec(&b.total))
                .unwrap_or_default();
            let available = avail
                .iter()
                .find(|(token, _)| *token == QUOTE_TOKEN)
                .map(|(_, v)| dec(v))
                .unwrap_or(total);
            return Ok(vec![Balance {
                currency: QUOTE.into(),
                total,
                available,
            }]);
        }

        // Classic: perps collateral only. Spot is a separate pot that cannot
        // back a perp without a transfer, so including it would overstate what
        // this strategy can actually deploy.
        let total = dec(&perps.margin_summary.account_value);
        Ok(vec![Balance {
            currency: QUOTE.into(),
            total,
            available: dec(&perps.withdrawable),
        }])
    }

    /// The perps-side account value on its own, for the double-count check.
    pub async fn raw_perps_account_value(&self, user: &str) -> Result<Decimal, VenueError> {
        let s: ClearinghouseState = self
            .post(serde_json::json!({"type": "clearinghouseState", "user": user}))
            .await?;
        Ok(s.margin_summary.account_value.parse().unwrap_or_default())
    }

    /// Which token perps settle in, as the venue reports it.
    pub async fn collateral_token(&self) -> Result<u32, VenueError> {
        #[derive(Deserialize)]
        struct M {
            #[serde(rename = "collateralToken")]
            collateral_token: u32,
        }
        let m: M = self.post(serde_json::json!({"type": "meta"})).await?;
        Ok(m.collateral_token)
    }

    /// Whether this account trades from a single unified balance.
    pub async fn is_unified(&self, user: &str) -> Result<bool, VenueError> {
        let spot: SpotState = self
            .post(serde_json::json!({"type": "spotClearinghouseState", "user": user}))
            .await?;
        Ok(spot.token_to_available_after_maintenance.is_some())
    }

    /// Open positions, signed.
    pub async fn positions(&self, user: &str) -> Result<Vec<Position>, VenueError> {
        let s: ClearinghouseState = self
            .post(serde_json::json!({"type": "clearinghouseState", "user": user}))
            .await?;
        Ok(s.asset_positions
            .iter()
            .filter_map(|p| {
                let qty: Decimal = p.position.szi.parse().ok()?;
                if qty.is_zero() {
                    return None;
                }
                Some(Position {
                    asset: p.position.coin.clone(),
                    qty,
                    avg_price: p
                        .position
                        .entry_px
                        .as_deref()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or_default(),
                })
            })
            .collect())
    }

    /// Resting orders.
    pub async fn open_orders(&self, user: &str) -> Result<Vec<OpenOrder>, VenueError> {
        let raw: Vec<OpenOrderRaw> = self
            .post(serde_json::json!({"type": "openOrders", "user": user}))
            .await?;
        Ok(raw
            .iter()
            .map(|o| {
                let qty: Decimal = o.sz.parse().unwrap_or_default();
                let px: Decimal = o.limit_px.parse().unwrap_or_default();
                OpenOrder {
                    venue_order_id: o.oid.to_string(),
                    client_order_id: o.cloid.clone().unwrap_or_default(),
                    asset: o.coin.clone(),
                    side: side_of(&o.side),
                    qty,
                    limit_price: px,
                    reserved: qty * px,
                    reserved_currency: QUOTE.into(),
                }
            })
            .collect())
    }

    /// Fills, newest first from the venue, returned oldest first.
    ///
    /// The trait's contract is oldest first because folding a position out of a
    /// fill log only works in order.
    pub async fn fills(
        &self,
        user: &str,
        since: Option<OffsetDateTime>,
    ) -> Result<Vec<Fill>, VenueError> {
        let raw: Vec<FillRaw> = self
            .post(serde_json::json!({"type": "userFills", "user": user}))
            .await?;
        let mut out: Vec<Fill> = raw
            .iter()
            .filter_map(|f| {
                let ts =
                    OffsetDateTime::from_unix_timestamp_nanos((f.time as i128) * 1_000_000).ok()?;
                if let Some(cut) = since {
                    if ts < cut {
                        return None;
                    }
                }
                Some(Fill {
                    venue_fill_id: f.tid.to_string(),
                    venue_order_id: f.oid.to_string(),
                    client_order_id: f.cloid.clone().unwrap_or_default(),
                    asset: f.coin.clone(),
                    side: side_of(&f.side),
                    qty: f.sz.parse().ok()?,
                    price: f.px.parse().ok()?,
                    // Hyperliquid reports a rebate as a negative fee. Ours is
                    // "always positive — a fee is a cost", so a rebate is
                    // recorded as zero cost rather than as negative money that
                    // would quietly inflate NAV.
                    fee: f
                        .fee
                        .parse::<Decimal>()
                        .unwrap_or_default()
                        .max(Decimal::ZERO),
                    fee_currency: f.fee_token.clone().unwrap_or_else(|| QUOTE.into()),
                    ts,
                })
            })
            .collect();
        out.sort_by_key(|f| f.ts);
        Ok(out)
    }

    /// Agent wallets the account has approved, and when each stops working.
    ///
    /// Worth asking before trading rather than after: an unapproved or expired
    /// agent produces signatures the venue rejects without saying whose
    /// signature it disbelieved, and Hyperliquid ages agents out on a timer, so
    /// a setup that worked last quarter can stop without anything changing.
    pub async fn agents(&self, user: &str) -> Result<Vec<ApprovedAgent>, VenueError> {
        self.post(serde_json::json!({"type": "extraAgents", "user": user}))
            .await
    }

    /// Hourly bars, for a feed that wants candles rather than ticks.
    pub async fn candles(
        &self,
        coin: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Candle>, VenueError> {
        self.post(serde_json::json!({
            "type": "candleSnapshot",
            "req": {"coin": coin, "interval": interval, "startTime": start_ms, "endTime": end_ms}
        }))
        .await
    }

    /// Best bid and ask, for measuring the spread we are actually paying.
    pub async fn top_of_book(&self, coin: &str) -> Result<(Decimal, Decimal), VenueError> {
        let b: L2Book = self
            .post(serde_json::json!({"type": "l2Book", "coin": coin}))
            .await?;
        let px = |side: usize| -> Option<Decimal> { b.levels.get(side)?.first()?.px.parse().ok() };
        match (px(0), px(1)) {
            (Some(bid), Some(ask)) => Ok((bid, ask)),
            _ => Err(VenueError::NoPrice(coin.to_string())),
        }
    }
}

/// `A` is ask — the resting order was a sell, so the taker bought into it.
/// Hyperliquid reports the side of the *resting* order in both fills and open
/// orders, and getting this backwards would invert every position we derive.
fn side_of(s: &str) -> Side {
    match s {
        "A" => Side::Sell,
        _ => Side::Buy,
    }
}

/// One perp, in the engine's vocabulary.
///
/// `szDecimals` is the venue's size precision, so the lot is 10^-szDecimals.
/// Price precision on Hyperliquid perps is 5 significant figures capped at
/// (6 - szDecimals) decimal places; the tick is set to the finest that rule
/// ever allows, and a price that violates the significant-figure part is
/// rejected by the venue rather than silently rounded here. The executor does
/// not adjust orders, so refusing is the only correct behaviour.
fn perp_market(u: &Universe) -> Market {
    let lot = Decimal::new(1, u.sz_decimals as u32);
    let price_dp = 6u32.saturating_sub(u.sz_decimals as u32);
    Market {
        asset: u.name.clone(),
        venue_symbol: u.name.clone(),
        quote_currency: QUOTE.into(),
        tick: Decimal::new(1, price_dp),
        lot,
        // Hyperliquid enforces a $10 minimum order value.
        min_notional: Decimal::from(10),
        multiplier: Decimal::ONE,
        expiry: None,
        initial_margin: None,
        asset_class: "crypto".into(),
        capabilities: Capabilities {
            stop_orders: false,
            fractional: true,
            short: true,
            max_leverage: Decimal::from(u.max_leverage),
            funding: true,
        },
    }
}

// --- wire shapes, as captured from the live API -----------------------------

#[derive(Debug, Deserialize)]
pub struct MetaResponse {
    pub universe: Vec<Universe>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Universe {
    pub name: String,
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u8,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: u32,
    #[serde(default, rename = "isDelisted")]
    pub is_delisted: bool,
}

/// Fields beyond the mark are unread today and kept deliberately: this struct
/// documents the response shape, and the funding rate and day volume are the
/// next two things a capacity or funding model will want.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct AssetCtx {
    #[serde(rename = "markPx")]
    pub mark_px: Option<String>,
    #[serde(rename = "midPx")]
    pub mid_px: Option<String>,
    #[serde(rename = "oraclePx")]
    pub oracle_px: Option<String>,
    pub funding: Option<String>,
    #[serde(rename = "dayNtlVlm")]
    pub day_ntl_vlm: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ClearinghouseState {
    #[serde(rename = "marginSummary")]
    pub margin_summary: MarginSummary,
    pub withdrawable: String,
    #[serde(default, rename = "assetPositions")]
    pub asset_positions: Vec<AssetPosition>,
}

/// The spot side. Carries the unified-account marker.
#[derive(Debug, Deserialize)]
pub struct SpotState {
    #[serde(default)]
    pub balances: Vec<SpotBalance>,
    /// Present only for unified accounts: per-token collateral still free after
    /// maintenance margin.
    #[serde(default, rename = "tokenToAvailableAfterMaintenance")]
    pub token_to_available_after_maintenance: Option<Vec<(u32, String)>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SpotBalance {
    pub coin: String,
    pub token: u32,
    pub total: String,
    #[serde(default)]
    pub hold: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct MarginSummary {
    #[serde(rename = "accountValue")]
    pub account_value: String,
    #[serde(rename = "totalNtlPos")]
    pub total_ntl_pos: String,
    #[serde(rename = "totalMarginUsed")]
    pub total_margin_used: String,
}

#[derive(Debug, Deserialize)]
pub struct AssetPosition {
    pub position: PositionRaw,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PositionRaw {
    pub coin: String,
    /// Signed size. Negative is short.
    pub szi: String,
    #[serde(rename = "entryPx")]
    pub entry_px: Option<String>,
    #[serde(default, rename = "unrealizedPnl")]
    pub unrealized_pnl: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenOrderRaw {
    pub coin: String,
    pub side: String,
    #[serde(rename = "limitPx")]
    pub limit_px: String,
    pub sz: String,
    pub oid: u64,
    #[serde(default)]
    pub cloid: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FillRaw {
    pub coin: String,
    pub px: String,
    pub sz: String,
    pub side: String,
    pub time: u64,
    pub fee: String,
    pub oid: u64,
    pub tid: u64,
    #[serde(default)]
    pub cloid: Option<String>,
    #[serde(default, rename = "feeToken")]
    pub fee_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Candle {
    /// Open time, ms.
    pub t: i64,
    /// Close time, ms.
    #[serde(rename = "T")]
    pub close_time: i64,
    pub s: String,
    pub o: String,
    pub h: String,
    pub l: String,
    pub c: String,
    pub v: String,
    pub n: u64,
}

/// One agent wallet the account has approved.
#[derive(Debug, Clone, Deserialize)]
pub struct ApprovedAgent {
    pub name: String,
    pub address: String,
    /// Milliseconds since the epoch.
    #[serde(rename = "validUntil")]
    pub valid_until: i64,
}

impl ApprovedAgent {
    pub fn expires_at(&self) -> Option<OffsetDateTime> {
        OffsetDateTime::from_unix_timestamp_nanos((self.valid_until as i128) * 1_000_000).ok()
    }

    pub fn days_left(&self, now: OffsetDateTime) -> Option<i64> {
        self.expires_at().map(|e| (e - now).whole_days())
    }
}

#[derive(Debug, Deserialize)]
pub struct L2Book {
    pub levels: Vec<Vec<L2Level>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct L2Level {
    pub px: String,
    pub sz: String,
    pub n: u64,
}
