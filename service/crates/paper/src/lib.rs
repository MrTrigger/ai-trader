//! The `paper` venue — a fake broker over a real price feed.
//!
//! Design spec §4.1 lists this first among venue adapters and says it stays
//! first-class forever: it is what lets the whole system be built, run and
//! operated with no venue account and no capital. Phase 2's gate runs against
//! it for weeks.
//!
//! **It is not the backtest's fill simulator, and the distinction is
//! load-bearing** (spec §3.5). The `sim` model lives in Python inside the
//! harness and fills against historical bars. This is Rust, it runs inside the
//! real executor, and its whole purpose is to exercise the real execution and
//! reconciliation path — order placement, idempotent retry, resting orders,
//! the fill log, balance derivation. Running Phase 2 through the Python
//! simulator instead would test nothing Phase 1 had not already tested.
//!
//! ## What it models, and how pessimistically
//!
//! - A **market order** fills immediately at the mark, moved against us by
//!   `slippage_bps` and rounded to the tick grid in the direction that costs
//!   us. Taker fee.
//! - A **limit order** fills at its limit price — never better — the moment
//!   the mark reaches it. Marketable on arrival means an immediate taker fill;
//!   otherwise it rests and pays the maker fee when it eventually goes.
//! - Every refusal in [`venue::VenueError`] is enforced: lot and tick grids,
//!   min notional, short capability, and affordability against *available*
//!   balance rather than total.
//!
//! Every one of those choices errs against the account, because a paper venue
//! that flatters the strategy is worse than no paper venue at all.
//!
//! ## What it deliberately does not model
//!
//! Partial fills, queue position, order book depth, margin, and funding. Each
//! is a real effect and each would be guesswork here; the backtest's cost model
//! is where slippage is *estimated* and Phase 3's realised-vs-modelled
//! comparison is where it gets checked against a real venue (spec §6.1).
//!
//! One limitation is worth stating plainly rather than discovering later:
//! **reconciliation against this venue can never disagree.** A real venue keeps
//! its own record, and `get_positions` is genuinely independent evidence
//! against our fill log. Here both sides fold the same log, so the reconcile
//! path is *exercised* but never *falsified*. Phase 2 proves the plumbing runs;
//! only a real venue proves the comparison has teeth.

use async_trait::async_trait;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use venue::{
    derive_positions, AssetId, Balance, Clock, Fill, Market, OrderAck, OrderRequest, OrderState,
    OrderType, Position, PriceSource, Side, VenueAdapter, VenueError,
};

const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

/// How the fake broker behaves. Every field costs the account something.
#[derive(Debug, Clone)]
pub struct PaperConfig {
    pub quote_currency: String,
    pub initial_cash: Decimal,
    /// Charged on an order that crosses the spread.
    pub taker_fee_bps: Decimal,
    /// Charged on a resting order that is filled into.
    pub maker_fee_bps: Decimal,
    /// How far a market order's fill price is moved against us.
    pub slippage_bps: Decimal,
    /// Decimal places fees are rounded to — always away from zero, so
    /// rounding a fee never works in the account's favour.
    pub quote_decimals: u32,
}

impl Default for PaperConfig {
    fn default() -> Self {
        Self {
            quote_currency: "USD".into(),
            initial_cash: Decimal::ZERO,
            taker_fee_bps: Decimal::from(10), // 10bps, a plausible spot taker
            maker_fee_bps: Decimal::from(5),
            slippage_bps: Decimal::from(5),
            quote_decimals: 8,
        }
    }
}

/// An order accepted and still waiting for the mark to reach it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Resting {
    venue_order_id: String,
    client_order_id: String,
    asset: AssetId,
    side: Side,
    qty: Decimal,
    limit_price: Decimal,
    /// What placing it claimed from the account, and in what currency.
    /// Released when the order fills or is cancelled.
    reserved: Decimal,
    reserved_currency: String,
}

/// One order this venue has seen, kept so a replayed `client_order_id`
/// returns the original answer instead of placing a second order.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Placed {
    request: OrderRequest,
    ack: OrderAck,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Append-only. The only stored truth in this venue; everything else —
    /// positions, cash, balances — is folded out of it (spec §0.7).
    fills: Vec<Fill>,
    resting: Vec<Resting>,
    placed: HashMap<String, Placed>,
    seq: u64,
}

impl State {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }
}

/// The fake broker.
///
/// Generic over its price source and clock because both are seams a real
/// deployment fills differently from a test: Phase 2 supplies a live feed and
/// the system clock, a test supplies [`venue::ManualPrices`] and
/// [`venue::ManualClock`] and gets a deterministic venue.
pub struct PaperVenue<P, C> {
    config: PaperConfig,
    markets: Vec<Market>,
    prices: P,
    clock: C,
    state: Mutex<State>,
}

impl<P: PriceSource, C: Clock> PaperVenue<P, C> {
    pub fn new(config: PaperConfig, markets: Vec<Market>, prices: P, clock: C) -> Self {
        Self {
            config,
            markets,
            prices,
            clock,
            state: Mutex::new(State::default()),
        }
    }

    /// The venue's whole state as JSON.
    ///
    /// A real venue keeps its own books; this one keeps them in memory, so
    /// without this a restart silently resets the account to flat and cash to
    /// its initial value. That is not a small inconvenience — the fill log is
    /// the *only* stored truth here (spec §0.7), and losing it means the
    /// executor reconciles a real intention against a book that has forgotten
    /// everything.
    ///
    /// The whole state, not just the fills: resting orders and the
    /// idempotency map are what make a replayed `client_order_id` return its
    /// original answer, and a restart that dropped them would place the order
    /// again.
    pub async fn snapshot(&self) -> String {
        let state = self.state.lock().await;
        serde_json::to_string_pretty(&*state).expect("paper state serialises")
    }

    /// Rebuild from [`snapshot`](Self::snapshot).
    ///
    /// Config, markets, prices and clock come from the caller as usual: those
    /// are deployment choices, and restoring a stale copy of them alongside the
    /// book is how a venue ends up trading yesterday's fee schedule.
    pub fn restore(
        config: PaperConfig,
        markets: Vec<Market>,
        prices: P,
        clock: C,
        snapshot: &str,
    ) -> Result<Self, serde_json::Error> {
        let state: State = serde_json::from_str(snapshot)?;
        Ok(Self {
            config,
            markets,
            prices,
            clock,
            state: Mutex::new(state),
        })
    }

    fn market(&self, asset: &str) -> Result<&Market, VenueError> {
        self.markets
            .iter()
            .find(|m| m.asset == asset)
            .ok_or_else(|| VenueError::UnknownMarket(asset.to_string()))
    }

    /// Cash, folded out of the fill log.
    ///
    /// Not a running total kept alongside the fills: a stored balance and a
    /// fill log can disagree, and then there is a third opinion about the
    /// account with no way to tell which is right.
    fn cash(&self, fills: &[Fill]) -> Decimal {
        fills.iter().fold(self.config.initial_cash, |acc, f| {
            acc - f.signed_qty() * f.price - f.fee
        })
    }

    fn fee(&self, notional: Decimal, bps: Decimal) -> Decimal {
        (notional * bps / BPS)
            .round_dp_with_strategy(self.config.quote_decimals, RoundingStrategy::AwayFromZero)
    }

    /// The fee a resting order might cost, for reservation purposes: whichever
    /// of the two rates is worse. Reserving the optimistic one lets an account
    /// place an order it cannot actually afford to have filled.
    fn worst_fee(&self, notional: Decimal) -> Decimal {
        self.fee(
            notional,
            self.config.taker_fee_bps.max(self.config.maker_fee_bps),
        )
    }

    /// Match every resting order the mark has reached.
    ///
    /// Called at the top of every trait method, because a fake broker over a
    /// live feed should reflect the feed as of the moment it is asked — not as
    /// of the last time someone placed an order.
    ///
    /// An asset with no price is skipped rather than raising: a venue that
    /// cannot price a market cannot match in it either, and refusing to report
    /// balances because some unrelated symbol went dark would fail closed in
    /// the wrong place.
    async fn settle(&self, state: &mut State) -> Result<(), VenueError> {
        loop {
            let mut hit: Option<usize> = None;

            for (i, order) in state.resting.iter().enumerate() {
                let mark = match self.prices.mark_price(&order.asset).await {
                    Ok(p) => p,
                    Err(VenueError::NoPrice(_)) => continue,
                    Err(e) => return Err(e),
                };
                let marketable = match order.side {
                    Side::Buy => mark <= order.limit_price,
                    Side::Sell => mark >= order.limit_price,
                };
                if marketable {
                    hit = Some(i);
                    break;
                }
            }

            let Some(i) = hit else { return Ok(()) };
            let order = state.resting.remove(i);

            // A resting order fills at its own price. Never better — a paper
            // venue handing out price improvement is inventing edge.
            let notional = order.qty * order.limit_price;
            let fee = self.fee(notional, self.config.maker_fee_bps);
            let seq = state.next_seq();

            state.fills.push(Fill {
                venue_fill_id: format!("paper-f-{seq}"),
                venue_order_id: order.venue_order_id.clone(),
                client_order_id: order.client_order_id.clone(),
                asset: order.asset.clone(),
                side: order.side,
                qty: order.qty,
                price: order.limit_price,
                fee,
                fee_currency: self.config.quote_currency.clone(),
                ts: self.clock.now(),
            });

            if let Some(p) = state.placed.get_mut(&order.client_order_id) {
                p.ack.state = OrderState::Filled;
            }
        }
    }

    /// What each currency has left after resting orders take their claim.
    fn reserved(state: &State) -> HashMap<String, Decimal> {
        let mut out: HashMap<String, Decimal> = HashMap::new();
        for r in &state.resting {
            *out.entry(r.reserved_currency.clone()).or_default() += r.reserved;
        }
        out
    }

    fn balances_from(&self, state: &State) -> Vec<Balance> {
        let reserved = Self::reserved(state);
        let claim = |currency: &str| reserved.get(currency).copied().unwrap_or(Decimal::ZERO);

        let cash = self.cash(&state.fills);
        let mut out = vec![Balance {
            currency: self.config.quote_currency.clone(),
            total: cash,
            available: cash - claim(&self.config.quote_currency),
        }];

        for position in derive_positions(&state.fills) {
            let claimed = claim(&position.asset);
            out.push(Balance {
                currency: position.asset,
                total: position.qty,
                available: position.qty - claimed,
            });
        }
        out
    }

    /// Test and operations hook: run the matching pass without placing or
    /// reading anything. Callers driving the venue by hand — a Phase 2 poll
    /// loop, or a test that just moved the price — use this.
    pub async fn poll(&self) -> Result<(), VenueError> {
        let mut state = self.state.lock().await;
        self.settle(&mut state).await
    }
}

/// Round `value` onto the `increment` grid, in the direction given.
///
/// `up` means "away from the account's interest": ceiling for a buy price,
/// floor for a sell price.
fn round_to_grid(value: Decimal, increment: Decimal, up: bool) -> Decimal {
    if increment.is_zero() {
        return value;
    }
    let steps = value / increment;
    let rounded = if up { steps.ceil() } else { steps.floor() };
    rounded * increment
}

#[async_trait]
impl<P: PriceSource, C: Clock> VenueAdapter for PaperVenue<P, C> {
    async fn get_markets(&self) -> Result<Vec<Market>, VenueError> {
        Ok(self.markets.clone())
    }

    async fn get_balances(&self) -> Result<Vec<Balance>, VenueError> {
        let mut state = self.state.lock().await;
        self.settle(&mut state).await?;
        Ok(self.balances_from(&state))
    }

    async fn get_positions(&self) -> Result<Vec<Position>, VenueError> {
        let mut state = self.state.lock().await;
        self.settle(&mut state).await?;
        Ok(derive_positions(&state.fills))
    }

    async fn place_order(&self, order: &OrderRequest) -> Result<OrderAck, VenueError> {
        let mut state = self.state.lock().await;
        self.settle(&mut state).await?;

        // Idempotency first, before any validation. A retry of an order that
        // was accepted must not be re-judged against a book that has moved
        // since — the answer to "did you take this order" is fixed once given.
        if let Some(previous) = state.placed.get(&order.client_order_id) {
            if previous.request == *order {
                return Ok(previous.ack.clone());
            }
            return Err(VenueError::ClientOrderIdReused {
                id: order.client_order_id.clone(),
            });
        }

        let market = self.market(&order.asset)?;

        if order.qty <= Decimal::ZERO {
            return Err(VenueError::NonPositiveQty(order.asset.clone()));
        }
        if !market.qty_on_grid(order.qty) {
            return Err(VenueError::LotSize {
                asset: order.asset.clone(),
                qty: order.qty,
                lot: market.lot,
            });
        }

        // The price this order is judged against: its own limit, or the mark.
        let reference = match order.order_type {
            OrderType::Limit => {
                let price = order
                    .limit_price
                    .ok_or_else(|| VenueError::MissingLimitPrice(order.asset.clone()))?;
                if price <= Decimal::ZERO {
                    return Err(VenueError::TickSize {
                        asset: order.asset.clone(),
                        price,
                        tick: market.tick,
                    });
                }
                if !market.price_on_grid(price) {
                    return Err(VenueError::TickSize {
                        asset: order.asset.clone(),
                        price,
                        tick: market.tick,
                    });
                }
                price
            }
            OrderType::Market => self.prices.mark_price(&order.asset).await?,
        };

        let notional = order.qty * reference;
        if notional < market.min_notional {
            return Err(VenueError::BelowMinNotional {
                asset: order.asset.clone(),
                notional,
                min_notional: market.min_notional,
                quote: market.quote_currency.clone(),
            });
        }

        let balances = self.balances_from(&state);
        let available = |currency: &str| {
            balances
                .iter()
                .find(|b| b.currency == currency)
                .map(|b| b.available)
                .unwrap_or(Decimal::ZERO)
        };

        let signed = match order.side {
            Side::Buy => order.qty,
            Side::Sell => -order.qty,
        };
        let held = derive_positions(&state.fills)
            .into_iter()
            .find(|p| p.asset == order.asset)
            .map(|p| p.qty)
            .unwrap_or(Decimal::ZERO);
        let resulting = held + signed;

        if resulting < Decimal::ZERO && !market.capabilities.short {
            return Err(VenueError::ShortNotSupported {
                asset: order.asset.clone(),
                resulting,
            });
        }

        // Affordability, against *available* rather than total: two resting
        // buys must not both be affordable out of the same cash.
        match order.side {
            Side::Buy => {
                let need = notional + self.worst_fee(notional);
                let have = available(&self.config.quote_currency);
                if need > have {
                    return Err(VenueError::InsufficientBalance {
                        currency: self.config.quote_currency.clone(),
                        need,
                        available: have,
                    });
                }
            }
            Side::Sell => {
                // Only a long being sold needs inventory. A short sale is
                // permitted by capability above, and its margin is the venue's
                // problem — not modelled here, and said so in the crate docs.
                if !market.capabilities.short {
                    let have = available(&order.asset);
                    if order.qty > have {
                        return Err(VenueError::InsufficientBalance {
                            currency: order.asset.clone(),
                            need: order.qty,
                            available: have,
                        });
                    }
                }
            }
        }

        let now = self.clock.now();
        let seq = state.next_seq();
        let venue_order_id = format!("paper-o-{seq}");

        // Does it go now, or rest? A market order always goes. A limit order
        // goes if the mark is already through it.
        let immediate = match order.order_type {
            OrderType::Market => true,
            OrderType::Limit => match self.prices.mark_price(&order.asset).await {
                Ok(mark) => match order.side {
                    Side::Buy => mark <= reference,
                    Side::Sell => mark >= reference,
                },
                // Unpriceable: it rests. It cannot be matched, so it cannot be
                // filled, and refusing it outright would be a different answer
                // than the venue gives once the feed returns.
                Err(VenueError::NoPrice(_)) => false,
                Err(e) => return Err(e),
            },
        };

        let ack = if immediate {
            let price = match order.order_type {
                // Slippage against us, then onto the tick grid, also against us.
                OrderType::Market => {
                    let slip = self.config.slippage_bps / BPS;
                    let moved = match order.side {
                        Side::Buy => reference * (Decimal::ONE + slip),
                        Side::Sell => reference * (Decimal::ONE - slip),
                    };
                    round_to_grid(moved, market.tick, order.side == Side::Buy)
                }
                // A marketable limit fills at its limit, which is by definition
                // no better than the mark it crossed.
                OrderType::Limit => reference,
            };

            let fill_notional = order.qty * price;
            let fee = self.fee(fill_notional, self.config.taker_fee_bps);

            // Slippage can push a market buy past what the account can pay.
            // Checking the estimate and then filling anyway would let the
            // paper venue go overdrawn, which no venue does.
            if order.side == Side::Buy {
                let need = fill_notional + fee;
                let have = available(&self.config.quote_currency);
                if need > have {
                    return Err(VenueError::InsufficientBalance {
                        currency: self.config.quote_currency.clone(),
                        need,
                        available: have,
                    });
                }
            }

            let fill_seq = state.next_seq();
            state.fills.push(Fill {
                venue_fill_id: format!("paper-f-{fill_seq}"),
                venue_order_id: venue_order_id.clone(),
                client_order_id: order.client_order_id.clone(),
                asset: order.asset.clone(),
                side: order.side,
                qty: order.qty,
                price,
                fee,
                fee_currency: self.config.quote_currency.clone(),
                ts: now,
            });

            OrderAck {
                client_order_id: order.client_order_id.clone(),
                venue_order_id,
                state: OrderState::Filled,
                accepted_at: now,
            }
        } else {
            let (reserved, reserved_currency) = match order.side {
                Side::Buy => (
                    notional + self.worst_fee(notional),
                    self.config.quote_currency.clone(),
                ),
                Side::Sell => (order.qty, order.asset.clone()),
            };

            state.resting.push(Resting {
                venue_order_id: venue_order_id.clone(),
                client_order_id: order.client_order_id.clone(),
                asset: order.asset.clone(),
                side: order.side,
                qty: order.qty,
                limit_price: reference,
                reserved,
                reserved_currency,
            });

            OrderAck {
                client_order_id: order.client_order_id.clone(),
                venue_order_id,
                state: OrderState::Open,
                accepted_at: now,
            }
        };

        state.placed.insert(
            order.client_order_id.clone(),
            Placed {
                request: order.clone(),
                ack: ack.clone(),
            },
        );
        Ok(ack)
    }

    async fn cancel_order(&self, venue_order_id: &str) -> Result<(), VenueError> {
        let mut state = self.state.lock().await;
        self.settle(&mut state).await?;

        let Some(i) = state
            .resting
            .iter()
            .position(|r| r.venue_order_id == venue_order_id)
        else {
            // Including an order that filled a moment ago. Cancelling
            // something that is gone is not a no-op — it means our view of the
            // venue is wrong, and that is worth stopping for.
            return Err(VenueError::UnknownOrder(venue_order_id.to_string()));
        };

        let cancelled = state.resting.remove(i);
        if let Some(p) = state.placed.get_mut(&cancelled.client_order_id) {
            p.ack.state = OrderState::Cancelled;
        }
        Ok(())
    }

    async fn get_fills(&self, since: Option<OffsetDateTime>) -> Result<Vec<Fill>, VenueError> {
        let mut state = self.state.lock().await;
        self.settle(&mut state).await?;
        Ok(state
            .fills
            .iter()
            .filter(|f| since.is_none_or(|s| f.ts >= s))
            .cloned()
            .collect())
    }
}
