//! Historical replay of the exact live Rust decision function.
//!
//! Feature rows are computed once with the shared causal feature crate, then
//! every rebalance calls [`crate::decide`]. Fills occur at the opening price of
//! the bar excluded from that decision, so this module cannot acquire a second
//! strategy implementation as it evolves.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use features_crypto::{DailyRow, HourlyRow};
use plan::{Mode, OrderReason, Side, Status};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::config::Config;
use crate::execution::{self, Settings};
use crate::{decide, store, universe, DecisionInput, Portfolio, Position};

pub const CRYPTO_YEAR: f64 = 365.0;
pub const INSUFFICIENT_SAMPLE_N: usize = 60;
const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

#[derive(Debug, Clone, serde::Serialize)]
pub struct Fill {
    pub ts: DateTime<Utc>,
    pub asset: String,
    pub side: Side,
    #[serde(with = "rust_decimal::serde::str")]
    pub qty: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub fee: Decimal,
    pub reason: OrderReason,
    #[serde(with = "rust_decimal::serde::str")]
    pub reference_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub spread_cost: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub impact_cost: Decimal,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Attribution {
    #[serde(with = "rust_decimal::serde::str")]
    pub nav_before: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub long_price_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub short_price_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub funding_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub commission_cost: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub spread_cost: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub impact_cost: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub delay_pnl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unfilled_notional: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub reconciliation_error: Decimal,
}

impl Attribution {
    fn pnl(&self) -> Decimal {
        self.long_price_pnl + self.short_price_pnl + self.funding_pnl + self.delay_pnl
            - self.commission_cost
            - self.spread_cost
            - self.impact_cost
    }
}

impl Fill {
    fn notional(&self) -> Decimal {
        self.qty * self.price
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Step {
    pub as_of: DateTime<Utc>,
    #[serde(with = "rust_decimal::serde::str")]
    pub nav: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cash: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub gross_exposure: Decimal,
    /// The book's net exposure (sum of signed position weights over NAV)
    /// AFTER this step's fills, marked at this step's prices - the number
    /// `max_net_exposure` is about, recorded so a replay can show the
    /// realized distribution and not just the plan's target. Older fold
    /// files lack it and read as zero.
    #[serde(default, with = "rust_decimal::serde::str")]
    pub net_exposure: Decimal,
    pub status: Status,
    pub fills: Vec<Fill>,
    pub plan_id: uuid::Uuid,
    pub warnings: Vec<String>,
    pub attribution: Option<Attribution>,
}

impl Step {
    fn traded_notional(&self) -> Decimal {
        self.fills.iter().map(Fill::notional).sum()
    }

    fn fees(&self) -> Decimal {
        self.fills
            .iter()
            .map(|fill| fill.fee + fill.spread_cost + fill.impact_cost)
            .sum()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Metrics {
    pub n: usize,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_return: Decimal,
    pub cagr: f64,
    pub volatility: f64,
    pub sharpe: f64,
    #[serde(with = "rust_decimal::serde::str")]
    pub max_drawdown: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub turnover_per_rebalance: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub cost_drag_bps: Decimal,
    pub rejected: usize,
    pub insufficient_sample: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct BacktestResult {
    pub schema_version: &'static str,
    #[serde(with = "rust_decimal::serde::str")]
    pub initial_cash: Decimal,
    pub execution: Settings,
    pub steps: Vec<Step>,
    pub metrics: Metrics,
    pub disclosures: Vec<String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub slippage_multiple: Decimal,
}

/// Immutable feature and execution-price history. Phase 1 prepares this once
/// and shares it across the candidate, stress, and baseline replays.
pub struct Prepared {
    daily: Vec<DailyRow>,
    hourly: Option<Vec<HourlyRow>>,
    opens: BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>>,
    /// Realised daily funding, kept so the replay can charge it rather than
    /// only feed it to the features.
    funding: features_crypto::FundingTable,
    execution_prices: execution::Prices,
    liquidity: BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
}

pub fn prepare(
    cfg: &Config,
    root: &Path,
    require_hourly: bool,
    funding_window: features_crypto::FundingWindow,
) -> Result<Prepared, String> {
    let daily_bars: Vec<_> = store::read(root, cfg.interval_s as i32)?
        .into_iter()
        .filter(|bar| canonical(&bar.asset))
        .collect();
    if daily_bars.is_empty() {
        return Err("daily store is empty".into());
    }
    let listings = store::funding_listings(root)?;
    let funding = crate::funding::load(root)?;
    let daily = features_crypto::daily(
        &daily_bars,
        cfg.benchmark.as_deref(),
        &listings,
        &funding,
        funding_window,
    )?;
    let mut execution_prices = BTreeMap::new();
    let hourly = if require_hourly {
        let bars: Vec<_> = store::read(root, 3_600)?
            .into_iter()
            .filter(|bar| canonical(&bar.asset))
            .collect();
        if bars.is_empty() {
            return Err("ml_ranker requires hourly bars".into());
        }
        for bar in &bars {
            execution_prices
                .entry(bar.ts_utc)
                .or_insert_with(BTreeMap::new)
                .insert(bar.asset.clone(), decimal(bar.open)?);
        }
        Some(features_crypto::hourly_before_daily_decision(
            &bars,
            cfg.benchmark.as_deref(),
        )?)
    } else {
        None
    };
    let mut opens: BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>> = BTreeMap::new();
    let mut liquidity = BTreeMap::new();
    for row in &daily {
        opens
            .entry(row.ts_utc)
            .or_default()
            .insert(row.asset.clone(), decimal(row.mark_open)?);
        if let (Some(adv), Some(vol)) = (row.adv_quote, row.vol_30) {
            liquidity
                .entry(row.ts_utc + Duration::seconds(cfg.interval_s))
                .or_insert_with(BTreeMap::new)
                .insert(
                    row.asset.clone(),
                    execution::Liquidity {
                        hourly_volume: adv / 24.0,
                        hourly_volatility: vol / (365.0_f64 * 24.0).sqrt(),
                    },
                );
        }
    }
    Ok(Prepared {
        daily,
        hourly,
        opens,
        funding,
        execution_prices,
        liquidity,
    })
}

pub fn replay(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    slippage_multiple: Decimal,
    funding_window: features_crypto::FundingWindow,
    step_hours: Option<i64>,
) -> Result<BacktestResult, String> {
    let prepared = prepare(cfg, root, cfg.signal == "ml_ranker", funding_window)?;
    replay_prepared_stepped(
        cfg,
        start,
        end,
        root,
        initial_cash,
        slippage_multiple,
        &prepared,
        step_hours,
    )
}

pub fn replay_prepared(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    slippage_multiple: Decimal,
    prepared: &Prepared,
) -> Result<BacktestResult, String> {
    replay_prepared_stepped(
        cfg,
        start,
        end,
        root,
        initial_cash,
        slippage_multiple,
        prepared,
        None,
    )
}

pub fn replay_prepared_stepped(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    slippage_multiple: Decimal,
    prepared: &Prepared,
    step_hours_override: Option<i64>,
) -> Result<BacktestResult, String> {
    replay_with_execution(
        cfg,
        start,
        end,
        root,
        initial_cash,
        slippage_multiple,
        prepared,
        step_hours_override,
        &Settings::default(),
    )
}

pub fn replay_with_execution(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    slippage_multiple: Decimal,
    prepared: &Prepared,
    step_hours_override: Option<i64>,
    execution: &Settings,
) -> Result<BacktestResult, String> {
    execution.validate()?;
    if initial_cash <= Decimal::ZERO {
        return Err("initial cash must be positive".into());
    }
    if start > end {
        return Err(format!(
            "start {} is after end {}",
            start.date_naive(),
            end.date_naive()
        ));
    }
    if slippage_multiple <= Decimal::ZERO {
        return Err("slippage multiple must be positive".into());
    }

    if cfg.signal == "ml_ranker" && prepared.hourly.is_none() {
        return Err("prepared replay data has no hourly features for ml_ranker".into());
    }

    let cadence = match step_hours_override {
        // The evaluation grid, decoupled from the bar interval: a 12h grid
        // decides twice a day on the same daily features plus fresher hourly
        // ones. None keeps the config cadence exactly as before.
        Some(h) => Duration::hours(h),
        None => Duration::seconds(cfg.interval_s * cfg.rebalance_every.max(1) as i64),
    };
    if cadence <= Duration::zero() || Duration::hours(execution.delay_hours as i64) >= cadence {
        return Err("execution delay must be shorter than a positive decision cadence".into());
    }
    if execution.delay_hours > 0 && prepared.execution_prices.is_empty() {
        return Err("delayed execution requires historical hourly prices".into());
    }
    if cfg.liquidity_profile.is_some() {
        return Err("historical replay refuses an undated per-decision liquidity profile; use explicit execution spread scenarios".into());
    }
    let mut portfolio = Portfolio {
        cash: initial_cash,
        positions: Vec::new(),
    };
    let mut steps = Vec::new();
    let mut disclosures = Vec::new();
    // The last date whose funding has been charged. None until the first step,
    // because a book that does not exist yet cannot have been funded.
    let mut last_priced: Option<DateTime<Utc>> = None;
    let mut funding_total = Decimal::ZERO;
    // Last price seen for every asset the book has touched, for settling a
    // position whose asset stops trading (below).
    let mut last_price: BTreeMap<String, Decimal> = BTreeMap::new();
    for (_, prices) in prepared.opens.range(..start) {
        last_price.extend(prices.iter().map(|(asset, price)| (asset.clone(), *price)));
    }
    let mut settled_total = 0usize;
    let mut as_of = start;
    let mut previous_nav = initial_cash;
    while as_of <= end {
        // Venue identity breaks. The daily store has no transient holes (every
        // gap of 2+ days in 659 assets since 2020-09 is a real venue event,
        // and most are ticker redenominations - LUNA 2022-05-13 -> 05-31 is
        // 20,000x, COCOS 1000x, QUICK 1/1000). A venue settles a delisted
        // contract at its last mark; this loop used to keep the position as a
        // ghost marked at zero and then re-mark it at whatever the ticker
        // meant next (a -4.5% LUNA short read -45% of NAV on the reborn
        // series - docs/research/net-exposure-guard.md). So: a held position
        // whose asset has no bar today is settled at its last known price,
        // disclosed, and gone. The reborn series is a new identity as far as
        // the features are concerned already (their windows poison across
        // the gap), so nothing re-enters it until it has history again.
        let mut attribution = Attribution {
            nav_before: previous_nav,
            ..Default::default()
        };
        if let Some(opens) = prepared.opens.get(&as_of) {
            for position in &portfolio.positions {
                if let (Some(previous), Some(current)) =
                    (last_price.get(&position.asset), opens.get(&position.asset))
                {
                    let pnl = position.qty * (*current - *previous);
                    if position.qty > Decimal::ZERO {
                        attribution.long_price_pnl += pnl;
                    } else {
                        attribution.short_price_pnl += pnl;
                    }
                }
            }
            let settled = settle_missing(&mut portfolio, opens, &last_price);
            settled_total += settled.len();
            for (asset, qty, px) in settled {
                disclosures.push(format!(
                    "{}: {asset} has no bar; position {qty} closed at last-price proxy {px}; historical venue settlement is unverified",
                    as_of.date_naive()
                ));
            }
            for (asset, px) in opens {
                last_price.insert(asset.clone(), *px);
            }
        }
        let members = universe::load(root, as_of)
            .map_err(|e| format!("{as_of}: incomplete replay universe: {e}"))?;
        let eligible: BTreeSet<_> = members
            .into_iter()
            .filter(|member| member.eligible)
            .map(|member| member.asset)
            .collect();
        let input_id = format!("rust-backtest:{}", as_of.to_rfc3339());
        let decision = decide(DecisionInput {
            as_of,
            created_at: as_of,
            mode: Mode::Dry,
            config: cfg,
            daily_features: &prepared.daily,
            hourly_features: prepared.hourly.as_deref(),
            eligible_universe: &eligible,
            portfolio: &portfolio,
            inputs_hash: &input_id,
        });
        let decision = decision.map_err(|e| format!("{as_of}: incomplete replay decision: {e}"))?;
        let prices = prepared
            .opens
            .get(&as_of)
            .ok_or_else(|| format!("{as_of}: no valuation prices"))?;
        // Funding for the period that just ENDED, on the book that was held
        // through it, before this step's orders change anything. The direction
        // matters more than it looks: summing the days ahead of the position
        // instead of behind it is the one-character error that turned a
        // +134.9% backtest into +760.7%, so the window is closed on both ends
        // and excludes today.
        let carry = funding_carry(&portfolio, &prices, &prepared.funding, last_priced, as_of);
        portfolio.cash += carry;
        attribution.funding_pnl = carry;
        funding_total += carry;
        last_priced = Some(as_of);
        let fill_time = as_of + Duration::hours(execution.delay_hours as i64);
        let fill_prices = if execution.delay_hours == 0 {
            Some(prices)
        } else {
            prepared.execution_prices.get(&fill_time)
        };
        let (next, filled) = apply(
            &decision.plan.orders,
            &portfolio,
            prices,
            &last_price,
            fill_prices,
            cfg,
            execution,
            prepared.liquidity.get(&as_of),
            slippage_multiple,
            fill_time,
            &mut attribution,
        )?;
        portfolio = next;
        let nav = mark(&portfolio, &prices);
        attribution.reconciliation_error = nav - previous_nav - attribution.pnl();
        if attribution.reconciliation_error.abs() > Decimal::new(1, 8) {
            return Err(format!(
                "{as_of}: P&L attribution fails reconciliation: {}",
                attribution.reconciliation_error
            ));
        }
        if nav <= Decimal::ZERO {
            return Err(format!("{as_of}: portfolio insolvent, NAV {nav}"));
        }
        previous_nav = nav;
        let net_exposure = if nav > Decimal::ZERO {
            portfolio
                .positions
                .iter()
                .fold(Decimal::ZERO, |acc, position| {
                    acc + prices
                        .get(&position.asset)
                        .map(|price| position.qty * *price)
                        .unwrap_or_default()
                })
                / nav
        } else {
            Decimal::ZERO
        };
        steps.push(Step {
            as_of,
            nav,
            cash: portfolio.cash,
            gross_exposure: portfolio
                .positions
                .iter()
                .map(|p| p.qty.abs() * prices[&p.asset])
                .sum::<Decimal>()
                / nav,
            net_exposure,
            status: decision.plan.status,
            fills: filled,
            plan_id: decision.plan.plan_id,
            warnings: decision
                .plan
                .warnings
                .iter()
                .map(|warning| warning.message.clone())
                .collect(),
            attribution: Some(attribution),
        });
        as_of += cadence;
    }

    if settled_total > 0 {
        disclosures.insert(
            0,
            format!(
                "{settled_total} position(s) settled at last price because the asset stopped trading (venue identity break); see per-date lines"
            ),
        );
    }
    if prepared.funding.is_empty() {
        disclosures
            .push("no funding data in the store, so this replay carries no funding P&L".into());
    } else {
        disclosures.push(format!(
            "funding carried on held positions: {} over the window (Binance USD-M rates \
             standing in for Hyperliquid's, as the plan discloses)",
            format!("{:.2}", funding_total)
        ));
    }
    disclosures.extend(standing_disclosures(cfg, &steps, slippage_multiple));
    disclosures.push("execution liquidity uses trailing daily ADV / 24 and annual volatility / sqrt(365*24); these are hourly proxies, not observed venue depth".into());
    disclosures.push("spread overrides are explicit counterfactual scenarios, not historical observations; impact coefficient is assumed, not calibrated".into());
    disclosures.push("NAV is marked at the decision reference with execution shortfall debited; delayed fills use later hourly opens, never later prices in the signal; unfilled remainder is cancelled".into());
    Ok(BacktestResult {
        schema_version: "rust-backtest-2",
        initial_cash,
        execution: execution.clone(),
        metrics: metrics(&steps),
        steps,
        disclosures,
        slippage_multiple,
    })
}

/// What the book paid or earned in funding over `(since, until]`.
///
/// A perpetual charges longs and pays shorts when the rate is positive, so the
/// P&L on a position is `-qty * price * rate`: the sign falls out of the
/// quantity and needs no special case for shorts.
///
/// The window excludes `until` itself. Funding for a day is only knowable once
/// that day has happened, and charging it to a book being priced at that day's
/// open is how a backtest earns money it could not have earned. The recovered
/// harness summed the window forward by one day and turned +134.9% into
/// +760.7%; this is the same arithmetic pointed the other way, and the test
/// beside it exists to keep it pointed there.
fn funding_carry(
    book: &Portfolio,
    prices: &BTreeMap<String, Decimal>,
    table: &features_crypto::FundingTable,
    since: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
) -> Decimal {
    let Some(since) = since else {
        return Decimal::ZERO;
    };
    let mut total = Decimal::ZERO;
    for position in &book.positions {
        if position.qty.is_zero() {
            continue;
        }
        let Some(price) = prices.get(&position.asset) else {
            continue;
        };
        let Some(days) = table.get(&position.asset) else {
            continue;
        };
        for (day, rate) in days.range(since.date_naive()..until.date_naive()) {
            // range() is half-open, so `since` is included and `until` is not.
            // The day of `since` is the first full day the book was held.
            let _ = day;
            let Some(rate) = decimal(*rate).ok() else {
                continue;
            };
            total -= position.qty * price * rate;
        }
    }
    total
}

fn apply(
    orders: &[plan::Order],
    book: &Portfolio,
    prices: &BTreeMap<String, Decimal>,
    last_prices: &BTreeMap<String, Decimal>,
    fill_prices: Option<&BTreeMap<String, Decimal>>,
    cfg: &Config,
    execution: &Settings,
    liquidity: Option<&BTreeMap<String, execution::Liquidity>>,
    stress: Decimal,
    as_of: DateTime<Utc>,
    attribution: &mut Attribution,
) -> Result<(Portfolio, Vec<Fill>), String> {
    let mut quantities: BTreeMap<_, _> = book
        .positions
        .iter()
        .map(|position| (position.asset.clone(), position.qty))
        .collect();
    let mut cash = book.cash;
    let mut fills = Vec::new();
    for order in orders {
        let Some(open) = prices.get(&order.asset) else {
            let last = last_prices.get(&order.asset).ok_or_else(|| {
                format!("{}: unfilled order has no reference history", order.asset)
            })?;
            attribution.unfilled_notional += order.qty * *last;
            continue;
        };
        let Some(reference) = fill_prices.and_then(|p| p.get(&order.asset)) else {
            attribution.unfilled_notional += order.qty * *open;
            continue;
        };
        let Some(quote) = execution::quote(
            cfg,
            execution,
            &order.asset,
            order.qty,
            *reference,
            liquidity.and_then(|l| l.get(&order.asset)).copied(),
            stress,
        )?
        else {
            attribution.unfilled_notional += order.qty * *open;
            continue;
        };
        attribution.unfilled_notional += (order.qty - quote.qty) * *open;
        let spread_cost = quote.qty * *reference * quote.spread_bps / BPS;
        let impact_cost = quote.qty * *reference * quote.impact_bps / BPS;
        let edge = *reference * (quote.spread_bps + quote.impact_bps) / BPS;
        let price = match order.side {
            Side::Buy => *reference + edge,
            Side::Sell => *reference - edge,
        };
        let notional = quote.qty * price;
        let fee = notional * cfg.costs.commission_bps / BPS;
        let signed = match order.side {
            Side::Buy => quote.qty,
            Side::Sell => -quote.qty,
        };
        attribution.delay_pnl += signed * (*open - *reference);
        attribution.commission_cost += fee;
        attribution.spread_cost += spread_cost;
        attribution.impact_cost += impact_cost;
        cash += match order.side {
            Side::Buy => -notional - fee,
            Side::Sell => notional - fee,
        };
        *quantities.entry(order.asset.clone()).or_default() += signed;
        fills.push(Fill {
            ts: as_of,
            asset: order.asset.clone(),
            side: order.side,
            qty: quote.qty,
            price,
            fee,
            reason: order.reason,
            reference_price: *reference,
            spread_cost,
            impact_cost,
        });
    }
    let positions = quantities
        .into_iter()
        .filter(|(_, qty)| !qty.is_zero())
        .map(|(asset, qty)| Position { asset, qty })
        .collect();
    Ok((Portfolio { cash, positions }, fills))
}

/// Settle every held position whose asset has no price today at its last
/// known price - what a venue does when a contract stops trading - and
/// return what was settled as `(asset, qty, price)`. A position never priced
/// at all is kept: there is nothing to settle it at.
fn settle_missing(
    book: &mut Portfolio,
    today: &BTreeMap<String, Decimal>,
    last_price: &BTreeMap<String, Decimal>,
) -> Vec<(String, Decimal, Decimal)> {
    let mut settled = Vec::new();
    let mut kept = Vec::with_capacity(book.positions.len());
    for position in book.positions.drain(..) {
        if position.qty.is_zero() || today.contains_key(&position.asset) {
            kept.push(position);
            continue;
        }
        match last_price.get(&position.asset) {
            Some(px) => {
                book.cash += position.qty * *px;
                settled.push((position.asset, position.qty, *px));
            }
            None => kept.push(position),
        }
    }
    book.positions = kept;
    settled
}

fn mark(book: &Portfolio, prices: &BTreeMap<String, Decimal>) -> Decimal {
    book.positions.iter().fold(book.cash, |nav, position| {
        nav + prices
            .get(&position.asset)
            .map(|price| position.qty * *price)
            .unwrap_or_default()
    })
}

pub fn metrics(steps: &[Step]) -> Metrics {
    let rejected = steps
        .iter()
        .filter(|step| step.status == Status::Rejected)
        .count();
    let empty = || Metrics {
        n: steps.len(),
        total_return: Decimal::ZERO,
        cagr: 0.0,
        volatility: 0.0,
        sharpe: 0.0,
        max_drawdown: Decimal::ZERO,
        turnover_per_rebalance: Decimal::ZERO,
        cost_drag_bps: Decimal::ZERO,
        rejected,
        insufficient_sample: steps.len() < INSUFFICIENT_SAMPLE_N,
    };
    if steps.is_empty() {
        return empty();
    }
    let navs: Vec<_> = steps.iter().map(|step| step.nav).collect();
    let average_nav = navs.iter().sum::<Decimal>() / Decimal::from(navs.len() as u64);
    let traded = steps.iter().map(Step::traded_notional).sum::<Decimal>();
    let fees = steps.iter().map(Step::fees).sum::<Decimal>();
    let turnover = if average_nav.is_zero() {
        Decimal::ZERO
    } else {
        traded / Decimal::from(steps.len() as u64) / average_nav
    };
    let cost_drag = if average_nav.is_zero() {
        Decimal::ZERO
    } else {
        fees / average_nav * BPS
    };
    if steps.len() < 2 && steps[0].attribution.is_none() {
        return Metrics {
            turnover_per_rebalance: turnover,
            cost_drag_bps: cost_drag,
            ..empty()
        };
    }

    let initial = steps[0].attribution.as_ref().map(|a| a.nav_before);
    let first = initial.unwrap_or(navs[0]);
    let last = *navs.last().unwrap();
    let total_return = if first.is_zero() {
        Decimal::ZERO
    } else {
        (last - first) / first
    };
    let initial_period = if initial.is_some() {
        steps
            .get(1)
            .map(|s| s.as_of - steps[0].as_of)
            .unwrap_or(Duration::days(1))
    } else {
        Duration::zero()
    };
    let span_days = (steps.last().unwrap().as_of - steps[0].as_of + initial_period).num_seconds()
        as f64
        / 86_400.0;
    let years = span_days / CRYPTO_YEAR;
    let cagr = if years > 0.0 && first > Decimal::ZERO {
        (last / first)
            .to_f64()
            .unwrap_or_default()
            .powf(1.0 / years)
            - 1.0
    } else {
        0.0
    };
    let mut return_navs = Vec::with_capacity(navs.len() + 1);
    if let Some(initial) = initial {
        return_navs.push(initial);
    }
    return_navs.extend_from_slice(&navs);
    let returns: Vec<f64> = return_navs
        .windows(2)
        .filter(|pair| !pair[0].is_zero())
        .filter_map(|pair| ((pair[1] - pair[0]) / pair[0]).to_f64())
        .collect();
    let periods_per_year = if years > 0.0 {
        returns.len() as f64 / years
    } else {
        0.0
    };
    let mean = if returns.is_empty() {
        0.0
    } else {
        returns.iter().sum::<f64>() / returns.len() as f64
    };
    let variance = if returns.len() > 1 {
        returns
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (returns.len() - 1) as f64
    } else {
        0.0
    };
    let volatility = (variance * periods_per_year).sqrt();
    let sharpe = if volatility > 0.0 {
        mean * periods_per_year / volatility
    } else {
        0.0
    };
    let mut peak = first;
    let mut max_drawdown = Decimal::ZERO;
    for nav in navs {
        peak = peak.max(nav);
        if peak > Decimal::ZERO {
            max_drawdown = max_drawdown.min((nav - peak) / peak);
        }
    }
    Metrics {
        n: steps.len(),
        total_return,
        cagr,
        volatility,
        sharpe,
        max_drawdown,
        turnover_per_rebalance: turnover,
        cost_drag_bps: cost_drag,
        rejected,
        insufficient_sample: steps.len() < INSUFFICIENT_SAMPLE_N,
    }
}

fn standing_disclosures(cfg: &Config, steps: &[Step], slippage_multiple: Decimal) -> Vec<String> {
    let mut out = Vec::new();
    if steps.len() < INSUFFICIENT_SAMPLE_N {
        out.push(format!(
            "insufficient sample: {} rebalances, fewer than {INSUFFICIENT_SAMPLE_N}; no conclusion about edge is available",
            steps.len()
        ));
    }
    if !cfg.costs.calibrated {
        out.push("the cost model is uncalibrated: its impact coefficient is assumed rather than fitted to realised fills".into());
    }
    if slippage_multiple != Decimal::ONE {
        out.push(format!(
            "slippage is scaled {slippage_multiple}x for this run; it is an error bar, not a parameter"
        ));
    }
    let rejected = steps
        .iter()
        .filter(|step| step.status == Status::Rejected)
        .count();
    if rejected > 0 {
        out.push(format!(
            "{rejected} of {} plans were rejected by the risk gate and traded nothing",
            steps.len()
        ));
    }
    out.push("execution settings specify commission, spread, impact, delay and partial-fill assumptions; queue position and order-book depth are not simulated".into());
    out
}

fn decimal(value: f64) -> Result<Decimal, String> {
    if !value.is_finite() {
        return Err(format!("non-finite price {value}"));
    }
    value
        .to_string()
        .parse::<Decimal>()
        .map_err(|error| error.to_string())
}

fn canonical(asset: &str) -> bool {
    !asset.is_empty()
        && asset.len() <= 20
        && asset
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::NaiveDate;

    fn book(asset: &str, qty: i64) -> Portfolio {
        Portfolio {
            cash: Decimal::from(100_000),
            positions: vec![crate::Position {
                asset: asset.into(),
                qty: Decimal::from(qty),
            }],
        }
    }

    fn rates(days: &[(&str, f64)]) -> features_crypto::FundingTable {
        let mut inner = BTreeMap::new();
        for (d, r) in days {
            inner.insert(NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap(), *r);
        }
        BTreeMap::from([("BTC".to_string(), inner)])
    }

    fn at(d: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("{d}T00:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A long pays funding when the rate is positive; a short is paid.
    #[test]
    fn funding_charges_the_long_and_pays_the_short() {
        let prices = BTreeMap::from([("BTC".to_string(), Decimal::from(100))]);
        let table = rates(&[("2026-01-01", 0.001)]);
        let long = funding_carry(
            &book("BTC", 10),
            &prices,
            &table,
            Some(at("2026-01-01")),
            at("2026-01-02"),
        );
        let short = funding_carry(
            &book("BTC", -10),
            &prices,
            &table,
            Some(at("2026-01-01")),
            at("2026-01-02"),
        );
        // 10 units at 100 is 1000 of notional; 0.1% of it is 1.
        assert_eq!(long, Decimal::from(-1));
        assert_eq!(short, Decimal::from(1));
    }

    /// The +875% bug, in one assertion.
    ///
    /// The recovered harness summed the funding window FORWARD - `day +
    /// timedelta` where it needed `day -` - so a position was charged rates
    /// from days it had not lived through. That single character took a
    /// +134.9% backtest to +760.7%. The window here is half-open and must
    /// never reach the day being priced.
    #[test]
    fn funding_never_reaches_a_day_the_book_has_not_lived_through() {
        let prices = BTreeMap::from([("BTC".to_string(), Decimal::from(100))]);
        // A colossal rate on the day we are pricing at, and nothing before it.
        let table = rates(&[("2026-01-02", 1.0)]);
        let carry = funding_carry(
            &book("BTC", 10),
            &prices,
            &table,
            Some(at("2026-01-01")),
            at("2026-01-02"),
        );
        assert_eq!(
            carry,
            Decimal::ZERO,
            "the 2nd's rate belongs to the period ENDING on the 3rd, not the one \
             ending on the 2nd"
        );
    }

    #[test]
    fn a_book_with_no_history_yet_is_charged_nothing() {
        let prices = BTreeMap::from([("BTC".to_string(), Decimal::from(100))]);
        let table = rates(&[("2026-01-01", 0.001)]);
        // First step of a replay: nothing was held before it.
        assert_eq!(
            funding_carry(&book("BTC", 10), &prices, &table, None, at("2026-01-02")),
            Decimal::ZERO
        );
    }

    #[test]
    fn every_day_the_book_was_held_is_charged_exactly_once() {
        let prices = BTreeMap::from([("BTC".to_string(), Decimal::from(100))]);
        let table = rates(&[
            ("2026-01-01", 0.001),
            ("2026-01-02", 0.001),
            ("2026-01-03", 0.001),
        ]);
        // Held from the 1st to the 4th: three days, the 4th not yet knowable.
        let carry = funding_carry(
            &book("BTC", 10),
            &prices,
            &table,
            Some(at("2026-01-01")),
            at("2026-01-04"),
        );
        assert_eq!(carry, Decimal::from(-3));
    }

    /// LUNA, May 2022: a short of 1,000,000 units at $0.00005 (worth -$50)
    /// whose ticker disappears and comes back 18 days later at $8.87. Before:
    /// a ghost marked at zero, then -$8.87M. Now: settled at $0.00005 the day
    /// the bar is missing, cash -$50, position gone; a long in a live asset
    /// and a never-priced position are untouched.
    #[test]
    fn a_position_whose_asset_stops_trading_is_settled_at_its_last_price() {
        let mut book = Portfolio {
            cash: Decimal::from(100_000),
            positions: vec![
                Position {
                    asset: "LUNA".into(),
                    qty: Decimal::from(-1_000_000),
                },
                Position {
                    asset: "BTC".into(),
                    qty: Decimal::new(5, 1),
                },
                Position {
                    asset: "NEW".into(),
                    qty: Decimal::from(10),
                },
            ],
        };
        let today = BTreeMap::from([("BTC".to_string(), Decimal::from(30_000))]);
        let last = BTreeMap::from([
            ("LUNA".to_string(), Decimal::new(5, 5)),
            ("BTC".to_string(), Decimal::from(29_000)),
        ]);
        let settled = settle_missing(&mut book, &today, &last);
        assert_eq!(
            settled,
            vec![(
                "LUNA".to_string(),
                Decimal::from(-1_000_000),
                Decimal::new(5, 5)
            )]
        );
        assert_eq!(book.cash, Decimal::from(100_000) - Decimal::from(50));
        let names: Vec<&str> = book.positions.iter().map(|p| p.asset.as_str()).collect();
        assert_eq!(names, vec!["BTC", "NEW"]);
        // Reborn LUNA at $8.87 the next day is not our problem any more.
        let reborn = BTreeMap::from([
            ("BTC".to_string(), Decimal::from(30_000)),
            ("LUNA".to_string(), Decimal::new(887, 2)),
        ]);
        assert!(settle_missing(&mut book, &reborn, &last).is_empty());
        assert_eq!(book.cash, Decimal::from(99_950));
    }

    fn step(day: i64, nav: i64) -> Step {
        Step {
            as_of: "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(day),
            nav: Decimal::from(nav),
            cash: Decimal::from(nav),
            gross_exposure: Decimal::ZERO,
            net_exposure: Decimal::ZERO,
            status: Status::Accepted,
            fills: Vec::new(),
            plan_id: uuid::Uuid::nil(),
            warnings: Vec::new(),
            attribution: None,
        }
    }

    #[test]
    fn metrics_use_calendar_span_and_report_drawdown() {
        let out = metrics(&[step(0, 100), step(365, 120), step(730, 90)]);
        assert_eq!(out.n, 3);
        assert_eq!(out.total_return, Decimal::new(-1, 1));
        assert_eq!(out.max_drawdown, Decimal::new(-25, 2));
        assert!((out.cagr - (0.9_f64.sqrt() - 1.0)).abs() < 1e-12);
    }

    #[test]
    fn execution_costs_and_delays_reconcile_to_cash_and_positions() {
        let cfg_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let cfg = Config::load(&cfg_path).unwrap();
        let book = Portfolio {
            cash: Decimal::from(100_000),
            positions: Vec::new(),
        };
        let prices = BTreeMap::from([("BTC".into(), Decimal::from(100))]);
        let later = BTreeMap::from([("BTC".into(), Decimal::from(102))]);
        let liq = BTreeMap::from([(
            "BTC".into(),
            execution::Liquidity {
                hourly_volume: 100_000.0,
                hourly_volatility: 0.02,
            },
        )]);
        for side in [Side::Buy, Side::Sell] {
            let order = plan::Order {
                asset: "BTC".into(),
                side,
                qty: Decimal::from(100),
                order_type: plan::OrderType::Market,
                limit_price: None,
                reason: OrderReason::Entry,
                est_cost_bps: None,
            };
            let mut attr = Attribution {
                nav_before: book.cash,
                ..Default::default()
            };
            let (next, fills) = apply(
                &[order],
                &book,
                &prices,
                &prices,
                Some(&later),
                &cfg,
                &Settings::default(),
                Some(&liq),
                Decimal::ONE,
                Utc::now(),
                &mut attr,
            )
            .unwrap();
            assert_eq!(fills.len(), 1);
            assert!(fills[0].qty < Decimal::from(100));
            assert!(
                fills[0].qty.scale() <= 8,
                "partial fills must use the planner's quantity precision"
            );
            assert!(attr.impact_cost > Decimal::ZERO);
            assert!(attr.unfilled_notional > Decimal::ZERO);
            assert!((mark(&next, &prices) - book.cash - attr.pnl()).abs() < Decimal::new(1, 10));
            match side {
                Side::Buy => {
                    assert!(fills[0].price > Decimal::from(102));
                    assert!(attr.delay_pnl < Decimal::ZERO);
                }
                Side::Sell => {
                    assert!(fills[0].price < Decimal::from(102));
                    assert!(attr.delay_pnl > Decimal::ZERO);
                }
            }
        }
    }

    #[test]
    fn stale_daily_price_cannot_authorize_a_later_hourly_fill() {
        let cfg = Config::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml"),
        )
        .unwrap();
        let book = Portfolio {
            cash: Decimal::from(1000),
            positions: vec![],
        };
        let stale = BTreeMap::from([("ERD".into(), Decimal::from(10))]);
        let later = BTreeMap::from([("ERD".into(), Decimal::from(100))]);
        let order = plan::Order {
            asset: "ERD".into(),
            side: Side::Buy,
            qty: Decimal::from(2),
            order_type: plan::OrderType::Market,
            limit_price: None,
            reason: OrderReason::Entry,
            est_cost_bps: None,
        };
        let mut attr = Attribution::default();
        let (next, fills) = apply(
            &[order],
            &book,
            &BTreeMap::new(),
            &stale,
            Some(&later),
            &cfg,
            &Settings {
                model: execution::Model::Flat,
                delay_hours: 1,
                ..Default::default()
            },
            None,
            Decimal::ONE,
            Utc::now(),
            &mut attr,
        )
        .unwrap();
        assert!(fills.is_empty());
        assert!(next.positions.is_empty());
        assert_eq!(next.cash, book.cash);
        assert_eq!(attr.unfilled_notional, Decimal::from(20));
    }

    #[test]
    fn first_day_cost_is_a_return_and_a_drawdown() {
        let mut first = step(0, 99);
        first.attribution = Some(Attribution {
            nav_before: Decimal::from(100),
            commission_cost: Decimal::ONE,
            ..Default::default()
        });
        let out = metrics(&[first, step(1, 99)]);
        assert_eq!(out.total_return, Decimal::new(-1, 2));
        assert_eq!(out.max_drawdown, Decimal::new(-1, 2));
        assert!(out.sharpe < 0.0);
    }
}
