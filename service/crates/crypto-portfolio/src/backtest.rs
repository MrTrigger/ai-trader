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
    pub status: Status,
    pub fills: Vec<Fill>,
    pub plan_id: uuid::Uuid,
    pub warnings: Vec<String>,
}

impl Step {
    fn traded_notional(&self) -> Decimal {
        self.fills.iter().map(Fill::notional).sum()
    }

    fn fees(&self) -> Decimal {
        self.fills.iter().map(|fill| fill.fee).sum()
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
    pub steps: Vec<Step>,
    pub metrics: Metrics,
    pub disclosures: Vec<String>,
    #[serde(with = "rust_decimal::serde::str")]
    pub slippage_multiple: Decimal,
}

struct SimFill {
    commission_bps: Decimal,
    slippage_bps: Decimal,
}

impl SimFill {
    fn new(cfg: &Config, slippage_multiple: Decimal) -> Self {
        Self {
            commission_bps: cfg.costs.commission_bps,
            slippage_bps: cfg.costs.spread_bps * slippage_multiple,
        }
    }

    fn price(&self, side: Side, open: Decimal) -> Decimal {
        let edge = open * self.slippage_bps / BPS;
        match side {
            Side::Buy => open + edge,
            Side::Sell => open - edge,
        }
    }

    fn fee(&self, notional: Decimal) -> Decimal {
        notional * self.commission_bps / BPS
    }
}

/// Immutable feature and execution-price history. Phase 1 prepares this once
/// and shares it across the candidate, stress, and baseline replays.
pub struct Prepared {
    daily: Vec<DailyRow>,
    hourly: Option<Vec<HourlyRow>>,
    opens: BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>>,
}

pub fn prepare(cfg: &Config, root: &Path, require_hourly: bool) -> Result<Prepared, String> {
    let daily_bars: Vec<_> = store::read(root, cfg.interval_s as i32)?
        .into_iter()
        .filter(|bar| canonical(&bar.asset))
        .collect();
    if daily_bars.is_empty() {
        return Err("daily store is empty".into());
    }
    let listings = store::funding_listings(root)?;
    let daily = features_crypto::daily(&daily_bars, cfg.benchmark.as_deref(), &listings)?;
    let hourly = if require_hourly {
        let bars: Vec<_> = store::read(root, 3_600)?
            .into_iter()
            .filter(|bar| canonical(&bar.asset))
            .collect();
        if bars.is_empty() {
            return Err("ml_ranker requires hourly bars".into());
        }
        Some(features_crypto::hourly_before_daily_decision(
            &bars,
            cfg.benchmark.as_deref(),
        )?)
    } else {
        None
    };
    let mut opens: BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>> = BTreeMap::new();
    for row in &daily {
        opens
            .entry(row.ts_utc)
            .or_default()
            .insert(row.asset.clone(), decimal(row.mark_open)?);
    }
    Ok(Prepared {
        daily,
        hourly,
        opens,
    })
}

pub fn replay(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    slippage_multiple: Decimal,
) -> Result<BacktestResult, String> {
    let prepared = prepare(cfg, root, cfg.signal == "ml_ranker")?;
    replay_prepared(
        cfg,
        start,
        end,
        root,
        initial_cash,
        slippage_multiple,
        &prepared,
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

    let cadence = Duration::seconds(cfg.interval_s * cfg.rebalance_every.max(1) as i64);
    let fills = SimFill::new(cfg, slippage_multiple);
    let mut portfolio = Portfolio {
        cash: initial_cash,
        positions: Vec::new(),
    };
    let mut steps = Vec::new();
    let mut disclosures = Vec::new();
    let mut gate_failures = 0usize;
    let mut as_of = start;
    while as_of <= end {
        let members = match universe::load(root, as_of) {
            Ok(members) => members,
            Err(error) => {
                gate_failures += 1;
                disclosures.push(format!(
                    "{}: gate failed, no plan ({error})",
                    as_of.date_naive()
                ));
                as_of += cadence;
                continue;
            }
        };
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
        let decision = match decision {
            Ok(value) => value,
            Err(error) => {
                gate_failures += 1;
                disclosures.push(format!(
                    "{}: gate failed, no plan ({error})",
                    as_of.date_naive()
                ));
                as_of += cadence;
                continue;
            }
        };
        let prices = prepared.opens.get(&as_of).cloned().unwrap_or_default();
        let (next, filled) = apply(&decision.plan.orders, &portfolio, &prices, &fills, as_of);
        portfolio = next;
        let nav = mark(&portfolio, &prices);
        steps.push(Step {
            as_of,
            nav,
            cash: portfolio.cash,
            gross_exposure: decision.plan.nav.gross_exposure,
            status: decision.plan.status,
            fills: filled,
            plan_id: decision.plan.plan_id,
            warnings: decision
                .plan
                .warnings
                .iter()
                .map(|warning| warning.message.clone())
                .collect(),
        });
        as_of += cadence;
    }

    if gate_failures > 0 {
        disclosures.insert(
            0,
            format!(
                "{gate_failures} of {} dates produced no plan (gates failed); those dates are absent from the metrics",
                gate_failures + steps.len()
            ),
        );
    }
    disclosures.extend(standing_disclosures(cfg, &steps, slippage_multiple));
    Ok(BacktestResult {
        metrics: metrics(&steps),
        steps,
        disclosures,
        slippage_multiple,
    })
}

fn apply(
    orders: &[plan::Order],
    book: &Portfolio,
    prices: &BTreeMap<String, Decimal>,
    model: &SimFill,
    as_of: DateTime<Utc>,
) -> (Portfolio, Vec<Fill>) {
    let mut quantities: BTreeMap<_, _> = book
        .positions
        .iter()
        .map(|position| (position.asset.clone(), position.qty))
        .collect();
    let mut cash = book.cash;
    let mut fills = Vec::new();
    for order in orders {
        let Some(open) = prices.get(&order.asset) else {
            continue;
        };
        let price = model.price(order.side, *open);
        let notional = order.qty * price;
        let fee = model.fee(notional);
        let signed = match order.side {
            Side::Buy => order.qty,
            Side::Sell => -order.qty,
        };
        cash += match order.side {
            Side::Buy => -notional - fee,
            Side::Sell => notional - fee,
        };
        *quantities.entry(order.asset.clone()).or_default() += signed;
        fills.push(Fill {
            ts: as_of,
            asset: order.asset.clone(),
            side: order.side,
            qty: order.qty,
            price,
            fee,
            reason: order.reason,
        });
    }
    let positions = quantities
        .into_iter()
        .filter(|(_, qty)| !qty.is_zero())
        .map(|(asset, qty)| Position { asset, qty })
        .collect();
    (Portfolio { cash, positions }, fills)
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
    if steps.len() < 2 {
        return Metrics {
            turnover_per_rebalance: turnover,
            cost_drag_bps: cost_drag,
            ..empty()
        };
    }

    let first = navs[0];
    let last = *navs.last().unwrap();
    let total_return = if first.is_zero() {
        Decimal::ZERO
    } else {
        (last - first) / first
    };
    let span_days = (steps.last().unwrap().as_of - steps[0].as_of).num_seconds() as f64 / 86_400.0;
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
    let returns: Vec<f64> = navs
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
    let mut peak = navs[0];
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
    out.push("the fill model crosses the spread and charges commission; it models no partial fills, queue position, or depth".into());
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

    fn step(day: i64, nav: i64) -> Step {
        Step {
            as_of: "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::days(day),
            nav: Decimal::from(nav),
            cash: Decimal::from(nav),
            gross_exposure: Decimal::ZERO,
            status: Status::Accepted,
            fills: Vec::new(),
            plan_id: uuid::Uuid::nil(),
            warnings: Vec::new(),
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
    fn fill_model_never_improves_the_open() {
        let cfg_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml");
        let cfg = Config::load(&cfg_path).unwrap();
        let model = SimFill::new(&cfg, Decimal::ONE);
        assert!(model.price(Side::Buy, Decimal::from(100)) > Decimal::from(100));
        assert!(model.price(Side::Sell, Decimal::from(100)) < Decimal::from(100));
    }
}
