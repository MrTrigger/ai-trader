//! `VenueAdapter` — the one interface every venue is reached through.
//!
//! Design spec §4.1. This crate holds the interface and the vocabulary; it
//! holds **no venue-specific code at all**, and that separation is the point.
//! The engine reads [`Capabilities`] off a [`Market`] and adapts to what the
//! venue can do. It never branches on which venue it is talking to, because
//! the moment it does, adding the second venue stops being additive and
//! becomes a rewrite.
//!
//! Three things are deliberate:
//!
//! **Positions are derived, never reported truth of ours.** [`derive_positions`]
//! folds an append-only fill log. Our position record is a hypothesis; the
//! venue's is truth, and reconciliation compares the two (spec §0.6, §0.7).
//! A venue's own `get_positions` is the truth side of that comparison — ours
//! always comes from fills.
//!
//! **Money is [`Decimal`], never `f64`.** Every quantity, price and fee here.
//!
//! **Orders carry a caller-chosen id.** [`OrderRequest::client_order_id`] makes
//! `place_order` idempotent, which is what lets a crashed executor re-run and
//! converge instead of double-filling (spec §3.2). An adapter that ignores it
//! is not implementing this interface.
//!
//! The trait is async because every real implementation is an HTTP or
//! websocket client. Making it synchronous now would mean rewriting it, and
//! §4 exists so these interfaces do not get rewritten.

pub mod calendar;

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use time::OffsetDateTime;

/// Re-exported from the Plan contract rather than redeclared.
///
/// These three mean exactly the same thing on both sides of the seam, and a
/// parallel set of identical enums plus a conversion between them is a place
/// for a mis-mapping to hide. The executor hands a plan's order to a venue
/// with no translation layer, which is the point.
pub use plan::{OrderReason, OrderType, Side};

/// Canonical asset id, venue-independent — `BTC`, not `BTC-USDT` or `XBTUSD`.
///
/// The venue's own symbol lives in [`Market::venue_symbol`] and never escapes
/// the adapter. A plan is written in canonical ids (see the schema's `asset`
/// definition), so a plan is portable across venues by construction.
pub type AssetId = String;

// --- markets ---------------------------------------------------------------

/// What a venue can do with a given market.
///
/// The engine reads these. It does not ask "is this Hyperliquid" — it asks
/// "can I short this", which is the question it actually has.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Whether sub-lot sizing is possible at all. Independent of [`Market::lot`],
    /// which says *how* granular: a market can be non-fractional with lot 1.
    pub fractional: bool,
    /// Whether a negative position is permitted.
    pub short: bool,
    /// `1` means spot. Above 1 means the venue will lend against the position.
    pub max_leverage: Decimal,
    /// Whether the venue accepts stop orders. False everywhere today: the
    /// paper venue does not model them and no strategy needs them yet; the
    /// field exists so an order can be REFUSED against it (capabilities are
    /// read, not decorative — step-3 finding).
    #[serde(default)]
    pub stop_orders: bool,
    /// Whether holding accrues or pays funding — a perp, in practice.
    pub funding: bool,
}

impl Capabilities {
    /// Plain spot: fractional, long-only, unlevered, no funding.
    pub fn spot() -> Self {
        Self {
            stop_orders: false,
            fractional: true,
            short: false,
            max_leverage: Decimal::ONE,
            funding: false,
        }
    }
}

/// A tradable market, in the engine's vocabulary rather than the venue's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub asset: AssetId,
    /// The venue's own symbol. Carried so an adapter can map back; nothing
    /// above the adapter may read it for a decision.
    pub venue_symbol: String,
    pub quote_currency: String,
    /// Price increment. A price that is not a multiple of this is refused.
    pub tick: Decimal,
    /// Quantity increment. A quantity that is not a multiple of this is refused.
    pub lot: Decimal,
    /// Smallest order value the venue accepts, in `quote_currency`.
    pub min_notional: Decimal,
    /// What one unit of `qty` IS, financially (step 3: futures vocabulary).
    ///
    /// Crypto/spot: `multiplier = 1`, `expiry = None` — a qty of 1 BTC is one
    /// bitcoin. Futures: an NQ contract has `multiplier = 20` (one point of
    /// price moves $20 of value) and an expiry, after which the market stops
    /// existing. P&L and notional math must use `multiplier`; anything that
    /// assumes qty x price is value silently mis-prices a future 20x.
    #[serde(default = "decimal_one")]
    pub multiplier: Decimal,
    /// Contract expiry for dated instruments; `None` for perpetual/spot.
    #[serde(default)]
    pub expiry: Option<time::Date>,
    /// Margin to OPEN one contract/unit, in `quote_currency`. `None` for
    /// fully-funded spot. Initial vs maintenance is a later refinement; one
    /// number is enough to refuse an order the account cannot carry.
    #[serde(default)]
    pub initial_margin: Option<Decimal>,
    /// Which asset class this market belongs to: "crypto", "futures",
    /// "equity". Adapters declare it; the engine may partition risk by it
    /// but never branches on venue.
    #[serde(default = "default_asset_class")]
    pub asset_class: String,
    pub capabilities: Capabilities,
}

fn default_asset_class() -> String {
    "crypto".into()
}

fn decimal_one() -> Decimal {
    Decimal::ONE
}

impl Market {
    /// Notional value of a quantity at a price, multiplier-aware.
    pub fn notional(&self, qty: Decimal, price: Decimal) -> Decimal {
        qty.abs() * price * self.multiplier
    }

    /// Whether `qty` sits exactly on the lot grid.
    ///
    /// Exact because [`Decimal`] is base-10: `0.39753288 % 0.00000001` is zero
    /// here and would not reliably be in binary floating point. That is the
    /// whole reason this type is used for sizes and not just for money.
    pub fn qty_on_grid(&self, qty: Decimal) -> bool {
        on_grid(qty, self.lot)
    }

    /// Whether `price` sits exactly on the tick grid.
    pub fn price_on_grid(&self, price: Decimal) -> bool {
        on_grid(price, self.tick)
    }
}

fn on_grid(value: Decimal, increment: Decimal) -> bool {
    // A zero increment means the venue declares no grid, so everything is on it.
    increment.is_zero() || (value % increment).is_zero()
}

// --- account state ---------------------------------------------------------

/// A currency or asset balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    /// Quote currency (`USD`) or an [`AssetId`] — a balance does not care which.
    pub currency: String,
    pub total: Decimal,
    /// `total` less whatever resting orders have already claimed.
    ///
    /// Separate from `total` so two resting buys cannot both be affordable
    /// against the same cash. Sizing against `total` is how a book ends up
    /// half-built.
    pub available: Decimal,
}

/// A position, always derived from fills — see [`derive_positions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub asset: AssetId,
    /// Signed. Negative is short.
    pub qty: Decimal,
    /// Volume-weighted entry price of the currently open quantity.
    pub avg_price: Decimal,
}

// --- orders and fills ------------------------------------------------------

/// An order as the engine asks for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRequest {
    /// Caller-chosen, unique, and stable across retries of the *same* order.
    ///
    /// Replaying an identical request must return the original ack rather than
    /// placing a second order. Reusing this id for a *different* order is an
    /// error, not an update — see [`VenueError::ClientOrderIdReused`].
    pub client_order_id: String,
    pub asset: AssetId,
    pub side: Side,
    /// Absolute base-asset quantity, always positive. Direction is `side`.
    pub qty: Decimal,
    pub order_type: OrderType,
    pub limit_price: Option<Decimal>,
    /// Why this order exists. Carried through so the executor can honour a
    /// pause without re-deriving intent — see `plan::OrderReason`.
    pub reason: OrderReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderState {
    /// Accepted and resting at the venue.
    Open,
    /// Some quantity filled, remainder still working (step 3: the enum grew
    /// the variant its own comment promised).
    PartiallyFilled,
    /// Completely filled. Partial fills were unmodelled until step 3; the
    /// needs them, this enum grows a variant rather than `Filled` growing a
    /// meaning.
    Filled,
    Cancelled,
}

/// The venue's acknowledgement. Returning this means the venue has the order —
/// not that it filled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAck {
    pub client_order_id: String,
    pub venue_order_id: String,
    pub state: OrderState,
    #[serde(with = "time::serde::rfc3339")]
    pub accepted_at: OffsetDateTime,
}

/// An order the venue has accepted and not yet filled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenOrder {
    pub venue_order_id: String,
    pub client_order_id: String,
    pub asset: AssetId,
    pub side: Side,
    pub qty: Decimal,
    pub limit_price: Decimal,
    /// What placing it claimed from the account, and in what currency.
    pub reserved: Decimal,
    pub reserved_currency: String,
}

/// An execution. Append-only, everywhere, forever (spec §0.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub venue_fill_id: String,
    pub venue_order_id: String,
    pub client_order_id: String,
    pub asset: AssetId,
    pub side: Side,
    /// Always positive; `side` carries the direction.
    pub qty: Decimal,
    pub price: Decimal,
    /// Always positive — a fee is a cost. Charged in `fee_currency`.
    pub fee: Decimal,
    pub fee_currency: String,
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
}

impl Fill {
    /// The position change this fill caused. Signed.
    pub fn signed_qty(&self) -> Decimal {
        match self.side {
            Side::Buy => self.qty,
            Side::Sell => -self.qty,
        }
    }

    /// Gross value exchanged, before fees.
    pub fn notional(&self) -> Decimal {
        self.qty * self.price
    }
}

/// Fold an append-only fill log into positions.
///
/// This is the derivation spec §0.7 requires: positions are a *view* over
/// fills, never stored truth. Reconciliation compares this against what the
/// venue reports, and a mismatch stops the run rather than being corrected
/// (spec §6.3).
///
/// `fills` must be in execution order. A position that closes to exactly zero
/// is dropped rather than reported as a zero row — "no position" and "a
/// position of size zero" should not be two states downstream.
pub fn derive_positions(fills: &[Fill]) -> Vec<Position> {
    let mut out: Vec<Position> = Vec::new();

    for fill in fills {
        let delta = fill.signed_qty();
        let idx = out.iter().position(|p| p.asset == fill.asset);

        match idx {
            None => out.push(Position {
                asset: fill.asset.clone(),
                qty: delta,
                avg_price: fill.price,
            }),
            Some(i) => {
                let (old_qty, old_avg) = (out[i].qty, out[i].avg_price);
                let new_qty = old_qty + delta;

                if new_qty.is_zero() {
                    // Flat. Drop it rather than carry a zero row.
                    out.remove(i);
                    continue;
                }

                out[i].avg_price = if old_qty.is_sign_negative() == delta.is_sign_negative() {
                    // Increasing: the entry price is the volume-weighted blend.
                    (old_avg * old_qty.abs() + fill.price * fill.qty) / new_qty.abs()
                } else if old_qty.is_sign_negative() != new_qty.is_sign_negative() {
                    // Reduced through zero and out the other side: what remains
                    // was opened at this fill's price, so the old average is gone.
                    fill.price
                } else {
                    // Plain reduction: selling part of a position does not
                    // change what the rest was bought at.
                    old_avg
                };
                out[i].qty = new_qty;
            }
        }
    }

    out
}

// --- failure ---------------------------------------------------------------

/// Everything a venue can refuse, named.
///
/// Every variant is a *refusal to act*, which is the only safe direction for
/// this system to fail in (spec §0.3). None of them is a reason to retry with
/// adjusted parameters — an adapter that silently rounds a quantity onto the
/// lot grid has changed the plan the risk layer cleared.
#[derive(Debug, thiserror::Error)]
pub enum VenueError {
    #[error("no market for asset {0}")]
    UnknownMarket(AssetId),

    #[error("no price available for {0}; refusing to price an order against nothing")]
    NoPrice(AssetId),

    #[error("quantity for {0} must be positive; direction belongs to `side`")]
    NonPositiveQty(AssetId),

    #[error("quantity {qty} for {asset} is not a multiple of lot {lot}")]
    LotSize {
        asset: AssetId,
        qty: Decimal,
        lot: Decimal,
    },

    #[error("price {price} for {asset} is not a multiple of tick {tick}")]
    TickSize {
        asset: AssetId,
        price: Decimal,
        tick: Decimal,
    },

    #[error("order for {asset} is {notional} {quote}, below the venue minimum {min_notional}")]
    BelowMinNotional {
        asset: AssetId,
        notional: Decimal,
        min_notional: Decimal,
        quote: String,
    },

    #[error("{asset} cannot be shorted here; this order would take the position to {resulting}")]
    ShortNotSupported { asset: AssetId, resulting: Decimal },

    #[error("insufficient {currency}: need {need}, have {available} available")]
    InsufficientBalance {
        currency: String,
        need: Decimal,
        available: Decimal,
    },

    #[error("limit order for {0} carries no limit price")]
    MissingLimitPrice(AssetId),

    #[error(
        "client_order_id {id} was already used for a different order; \
         an id identifies one order and is not a way to amend it"
    )]
    ClientOrderIdReused { id: String },

    #[error("no open order {0}")]
    UnknownOrder(String),

    #[error("venue unreachable: {0}")]
    Unreachable(String),
}

// --- the interface ---------------------------------------------------------

/// The one interface every venue is reached through (spec §4.1).
///
/// Implementations in build order: `paper`, then whichever venue Phase 3
/// selects, then an equities broker at Phase 6. `paper` stays first-class
/// forever — it is how the system is run without a venue decision or capital.
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    /// Every market this venue can trade, with its constraints and capabilities.
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError>;

    /// Cash and asset balances as the venue sees them.
    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError>;

    /// Positions as the **venue** reports them. This is the truth side of
    /// reconciliation; our side comes from [`derive_positions`] over our own
    /// fill log.
    async fn get_positions(&self) -> Result<Vec<Position>, VenueError>;

    /// Submit an order. Idempotent on [`OrderRequest::client_order_id`].
    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError>;

    /// Cancel a resting order. Cancelling one that is already gone is an
    /// error, not a no-op: it means our view of the venue is wrong.
    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), VenueError>;

    /// Orders accepted and not yet filled.
    ///
    /// Required rather than optional because flattening depends on it. A
    /// flatten that closed every position and left the resting orders alive
    /// would let one of them fill an hour later and re-open a book somebody had
    /// just decided to be out of — and it would look like it worked.
    async fn get_open_orders(&self) -> Result<Vec<OpenOrder>, VenueError>;

    /// Fills at or after `since`, oldest first. `None` means everything.
    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError>;
}

// --- price and time, as injectable seams -----------------------------------

/// Where a mark price comes from.
///
/// Separate from [`VenueAdapter`] because the two vary independently: the
/// `paper` venue is a fake broker over a *real* price feed, and NAV needs
/// marks regardless of who is executing.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// The current mark for `asset`, or [`VenueError::NoPrice`].
    ///
    /// Returning a stale price silently is the failure this interface must not
    /// have; a source that cannot vouch for freshness returns `NoPrice`.
    async fn mark_price(&self, asset: &str) -> Result<Decimal, VenueError>;
}

#[async_trait]
impl<T: PriceSource + ?Sized> PriceSource for std::sync::Arc<T> {
    async fn mark_price(&self, asset: &str) -> Result<Decimal, VenueError> {
        (**self).mark_price(asset).await
    }
}

/// Prices set explicitly by the caller.
///
/// This is the Phase 0 price source, and it is honest about what it is: there
/// is no live feed in this repo yet. A real-time feed is a second
/// [`PriceSource`] implementation that Phase 2 adds — the `paper` venue does
/// not change when it lands, which is the reason this is an interface.
#[derive(Debug, Default)]
pub struct ManualPrices {
    prices: Mutex<Vec<(String, Decimal)>>,
}

impl ManualPrices {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or replace) the mark for `asset`.
    pub fn set(&self, asset: &str, price: Decimal) {
        let mut prices = self.prices.lock().expect("price lock poisoned");
        match prices.iter_mut().find(|(a, _)| a == asset) {
            Some(entry) => entry.1 = price,
            None => prices.push((asset.to_string(), price)),
        }
    }

    /// Remove a mark, so the asset has no price at all.
    pub fn clear(&self, asset: &str) {
        let mut prices = self.prices.lock().expect("price lock poisoned");
        prices.retain(|(a, _)| a != asset);
    }
}

#[async_trait]
impl PriceSource for ManualPrices {
    async fn mark_price(&self, asset: &str) -> Result<Decimal, VenueError> {
        self.prices
            .lock()
            .expect("price lock poisoned")
            .iter()
            .find(|(a, _)| a == asset)
            .map(|(_, p)| *p)
            .ok_or_else(|| VenueError::NoPrice(asset.to_string()))
    }
}

/// Where "now" comes from.
///
/// Injectable so a test can assert on fill timestamps and on `get_fills(since)`
/// filtering without sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

impl<T: Clock + ?Sized> Clock for std::sync::Arc<T> {
    fn now(&self) -> OffsetDateTime {
        (**self).now()
    }
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A clock the caller advances by hand.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<OffsetDateTime>,
}

impl ManualClock {
    pub fn new(start: OffsetDateTime) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    pub fn advance(&self, by: time::Duration) {
        let mut now = self.now.lock().expect("clock lock poisoned");
        *now += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().expect("clock lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn fill(asset: &str, side: Side, qty: &str, price: &str) -> Fill {
        Fill {
            venue_fill_id: "f".into(),
            venue_order_id: "o".into(),
            client_order_id: "c".into(),
            asset: asset.into(),
            side,
            qty: dec(qty),
            price: dec(price),
            fee: Decimal::ZERO,
            fee_currency: "USD".into(),
            ts: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_single_buy_becomes_a_position() {
        let p = derive_positions(&[fill("BTC", Side::Buy, "0.5", "60000")]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].qty, dec("0.5"));
        assert_eq!(p[0].avg_price, dec("60000"));
    }

    #[test]
    fn increasing_blends_the_entry_price() {
        let p = derive_positions(&[
            fill("BTC", Side::Buy, "1", "60000"),
            fill("BTC", Side::Buy, "1", "70000"),
        ]);
        assert_eq!(p[0].qty, dec("2"));
        assert_eq!(p[0].avg_price, dec("65000"));
    }

    #[test]
    fn reducing_leaves_the_entry_price_alone() {
        // Selling half does not change what the other half was bought at.
        let p = derive_positions(&[
            fill("BTC", Side::Buy, "2", "60000"),
            fill("BTC", Side::Sell, "1", "90000"),
        ]);
        assert_eq!(p[0].qty, dec("1"));
        assert_eq!(p[0].avg_price, dec("60000"));
    }

    #[test]
    fn closing_to_flat_drops_the_position() {
        // Not a zero row. "No position" and "a position of size zero" must not
        // be two states downstream.
        let p = derive_positions(&[
            fill("BTC", Side::Buy, "1", "60000"),
            fill("BTC", Side::Sell, "1", "61000"),
        ]);
        assert!(p.is_empty());
    }

    #[test]
    fn selling_through_zero_reprices_the_remainder() {
        let p = derive_positions(&[
            fill("BTC", Side::Buy, "1", "60000"),
            fill("BTC", Side::Sell, "3", "70000"),
        ]);
        assert_eq!(p[0].qty, dec("-2"));
        assert_eq!(
            p[0].avg_price,
            dec("70000"),
            "the long's cost basis is gone"
        );
    }

    #[test]
    fn a_round_trip_closes_to_exactly_zero() {
        // The float version of this test is the one that leaves 1e-17 of BTC
        // behind and manufactures a reconciliation break.
        let p = derive_positions(&[
            fill("BTC", Side::Buy, "0.1", "60000"),
            fill("BTC", Side::Buy, "0.2", "60000"),
            fill("BTC", Side::Sell, "0.3", "60000"),
        ]);
        assert!(p.is_empty());
    }

    #[test]
    fn positions_are_kept_apart_by_asset() {
        let p = derive_positions(&[
            fill("BTC", Side::Buy, "1", "60000"),
            fill("ETH", Side::Buy, "10", "3000"),
            fill("BTC", Side::Sell, "1", "61000"),
        ]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].asset, "ETH");
    }

    #[test]
    fn the_lot_grid_is_exact_at_eight_decimals() {
        let m = Market {
            asset: "BTC".into(),
            venue_symbol: "BTCUSDT".into(),
            quote_currency: "USD".into(),
            tick: dec("0.01"),
            lot: dec("0.00000001"),
            min_notional: dec("10"),
            multiplier: Decimal::ONE,
            expiry: None,
            initial_margin: None,
            asset_class: "crypto".into(),
            capabilities: Capabilities::spot(),
        };
        assert!(m.qty_on_grid(dec("0.39753288")));
        assert!(!m.qty_on_grid(dec("0.397532881")));
        assert!(m.price_on_grid(dec("64000.50")));
        assert!(!m.price_on_grid(dec("64000.505")));
    }

    #[tokio::test]
    async fn a_missing_price_is_an_error_not_a_zero() {
        let prices = ManualPrices::new();
        prices.set("BTC", dec("60000"));
        assert_eq!(prices.mark_price("BTC").await.unwrap(), dec("60000"));
        assert!(matches!(
            prices.mark_price("ETH").await,
            Err(VenueError::NoPrice(_))
        ));
    }
}
