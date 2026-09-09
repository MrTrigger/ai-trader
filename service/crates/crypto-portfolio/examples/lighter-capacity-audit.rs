//! Compare matched zero-delay native/proxy reports without replaying or fitting.
//! cargo run -p crypto-portfolio --example lighter-capacity-audit -- NATIVE PROXY OUT
//! Requested notional is filled reference notional plus saved unfilled notional.
//! It excludes orders the planner never submitted. Reports do not save the side
//! of wholly unfilled orders, so this audit does not invent side-level demand.
use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn decimal(value: &Value, key: &str) -> Result<Decimal, String> {
    Decimal::from_str(
        value[key]
            .as_str()
            .ok_or_else(|| format!("missing decimal string {key}"))?,
    )
    .map_err(|e| format!("{key}: {e}"))
}
fn number(value: &Value, key: &str) -> Result<f64, String> {
    decimal(value, key)?
        .to_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("invalid {key}"))
}
fn finite(value: Decimal) -> f64 {
    value
        .to_f64()
        .expect("Decimal is representable as finite f64")
}
fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}
fn sd(values: &[f64]) -> f64 {
    let avg = mean(values);
    (values.iter().map(|v| (v - avg).powi(2)).sum::<f64>() / (values.len() - 1) as f64).sqrt()
}
fn correlation(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 2 {
        return None;
    }
    let am = mean(a);
    let bm = mean(b);
    let av = a.iter().map(|v| (v - am).powi(2)).sum::<f64>();
    let bv = b.iter().map(|v| (v - bm).powi(2)).sum::<f64>();
    (av > 0.0 && bv > 0.0).then(|| {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - am) * (y - bm))
            .sum::<f64>()
            / (av * bv).sqrt()
    })
}
#[derive(Serialize)]
struct Distribution {
    mean: f64,
    min: f64,
    p05: f64,
    p25: f64,
    p50: f64,
    p75: f64,
    p95: f64,
    max: f64,
}
fn distribution(values: &[f64]) -> Distribution {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let q = |p: f64| {
        let i = p * (sorted.len() - 1) as f64;
        let lo = i.floor() as usize;
        let hi = i.ceil() as usize;
        sorted[lo] + (sorted[hi] - sorted[lo]) * (i - lo as f64)
    };
    Distribution {
        mean: mean(values),
        min: sorted[0],
        p05: q(0.05),
        p25: q(0.25),
        p50: q(0.5),
        p75: q(0.75),
        p95: q(0.95),
        max: *sorted.last().unwrap(),
    }
}

#[derive(Default, Serialize)]
struct Flow {
    buy: f64,
    sell: f64,
    add_long: f64,
    reduce_long: f64,
    add_short: f64,
    reduce_short: f64,
    by_reason: BTreeMap<String, f64>,
}
fn fill_flow(old: Decimal, signed: Decimal, reference: Decimal) -> (f64, f64, f64, f64) {
    if signed > Decimal::ZERO {
        let close = signed.min((-old).max(Decimal::ZERO));
        (
            finite((signed - close) * reference),
            0.0,
            0.0,
            finite(close * reference),
        )
    } else {
        let close = (-signed).min(old.max(Decimal::ZERO));
        (
            0.0,
            finite(close * reference),
            finite((-signed - close) * reference),
            0.0,
        )
    }
}

#[derive(Serialize)]
struct Day {
    as_of: DateTime<Utc>,
    nav: f64,
    nav_before: f64,
    return_net: f64,
    gross_weight: f64,
    net_weight: f64,
    long_weight: f64,
    short_weight: f64,
    long_held_notional: f64,
    short_held_notional: f64,
    filled_reference_notional: f64,
    unfilled_reference_notional: f64,
    requested_reference_notional: f64,
    unfilled_fraction: Option<f64>,
    long_price_pnl: f64,
    short_price_pnl: f64,
    funding_pnl: f64,
    cost: f64,
    fills: usize,
    held_long_names: usize,
    held_short_names: usize,
    flow: Flow,
}
#[derive(Serialize)]
struct Summary {
    path: String,
    sha256: String,
    initial_cash: f64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    days: usize,
    recalculated_sharpe: f64,
    total_return: f64,
    gross_weight: Distribution,
    net_weight: Distribution,
    long_weight: Distribution,
    short_weight: Distribution,
    long_held_notional: Distribution,
    short_held_notional: Distribution,
    days_net_over_010: usize,
    days_net_below_minus010: usize,
    days_abs_net_over_010: usize,
    days_abs_net_over_010_fraction: f64,
    days_gross_below_040: usize,
    days_unfilled: usize,
    unfilled_fraction_on_requested_days: Distribution,
    total_filled_reference_notional: f64,
    total_unfilled_reference_notional: f64,
    total_requested_reference_notional: f64,
    aggregate_unfilled_fraction: f64,
    total_long_price_pnl: f64,
    total_short_price_pnl: f64,
    total_funding_pnl: f64,
    total_cost: f64,
    gross_component_return_correlation: Option<f64>,
    gross_component_pnl_correlation: Option<f64>,
    total_add_long: f64,
    total_reduce_long: f64,
    total_add_short: f64,
    total_reduce_short: f64,
    max_nav_reconciliation_error: f64,
    metadata: Value,
    execution: Value,
}
#[derive(Serialize)]
struct Audit {
    summary: Summary,
    daily: Vec<Day>,
}

fn audit(path: &Path) -> Result<Audit, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if doc["execution"]["delay_hours"] != 0 || doc["execution"]["model"] != "impact" {
        return Err("audit requires zero-delay impact reports for matching notional bases".into());
    }
    let initial = number(&doc, "initial_cash")?;
    if initial <= 0.0 {
        return Err("initial cash must be positive".into());
    }
    let rows = doc["steps"].as_array().ok_or("missing steps")?;
    if rows.len() < 2 {
        return Err("at least two daily steps required".into());
    }
    let mut positions = BTreeMap::<String, Decimal>::new();
    let mut daily = Vec::new();
    let mut previous_nav = initial;
    let mut previous_day = None;
    let mut max_error: f64 = 0.0;
    for step in rows {
        let at = step["as_of"]
            .as_str()
            .ok_or("missing as_of")?
            .parse::<DateTime<Utc>>()
            .map_err(|e| e.to_string())?;
        if at.timestamp() % 86400 != 0
            || at.timestamp_subsec_nanos() != 0
            || previous_day.is_some_and(|p| at != p + Duration::days(1))
        {
            return Err("steps must be unique consecutive UTC midnights".into());
        }
        previous_day = Some(at);
        let nav = number(step, "nav")?;
        let attr = &step["attribution"];
        let before = number(attr, "nav_before")?;
        if nav <= 0.0 || before <= 0.0 || (before - previous_nav).abs() > 1e-7 {
            return Err(format!("invalid NAV chain at {at}"));
        }
        let gross = number(step, "gross_exposure")?;
        let net = number(step, "net_exposure")?;
        if gross < 0.0 || net.abs() > gross + 1e-10 {
            return Err(format!("inconsistent gross/net at {at}"));
        }
        let long = (gross + net) / 2.0;
        let short = (gross - net) / 2.0;
        let long_pnl = number(attr, "long_price_pnl")?;
        let short_pnl = number(attr, "short_price_pnl")?;
        let funding = number(attr, "funding_pnl")?;
        let cost = number(attr, "commission_cost")?
            + number(attr, "spread_cost")?
            + number(attr, "impact_cost")?;
        let delay = number(attr, "delay_pnl")?;
        let error = nav - before - (long_pnl + short_pnl + funding + delay - cost);
        max_error = max_error.max(error.abs());
        if error.abs() > 1e-7 || delay.abs() > 1e-10 || cost < 0.0 {
            return Err(format!("attribution reconciliation failed at {at}"));
        }
        let fills = step["fills"].as_array().ok_or("missing fills")?;
        let mut filled = Decimal::ZERO;
        let mut fill_cost = Decimal::ZERO;
        let mut flow = Flow::default();
        for fill in fills {
            if fill["ts"]
                .as_str()
                .ok_or("missing fill timestamp")?
                .parse::<DateTime<Utc>>()
                .map_err(|e| e.to_string())?
                != at
            {
                return Err(format!("fill timestamp differs from decision at {at}"));
            }
            fill_cost += decimal(fill, "fee")?
                + decimal(fill, "spread_cost")?
                + decimal(fill, "impact_cost")?;
            let qty = decimal(fill, "qty")?;
            let reference = decimal(fill, "reference_price")?;
            if qty <= Decimal::ZERO || reference <= Decimal::ZERO {
                return Err("nonpositive fill quantity/reference".into());
            }
            let signed = match fill["side"].as_str() {
                Some("buy") => qty,
                Some("sell") => -qty,
                _ => return Err("unknown fill side".into()),
            };
            let asset = fill["asset"]
                .as_str()
                .ok_or("missing fill asset")?
                .to_owned();
            let old = *positions.get(&asset).unwrap_or(&Decimal::ZERO);
            let notional = finite(qty * reference);
            let (al, rl, ash, rs) = fill_flow(old, signed, reference);
            flow.add_long += al;
            flow.reduce_long += rl;
            flow.add_short += ash;
            flow.reduce_short += rs;
            if signed > Decimal::ZERO {
                flow.buy += notional;
            } else {
                flow.sell += notional;
            }
            *flow
                .by_reason
                .entry(fill["reason"].as_str().ok_or("missing fill reason")?.into())
                .or_default() += notional;
            positions.insert(asset, old + signed);
            filled += qty * reference;
        }
        if (finite(fill_cost) - cost).abs() > 1e-7 {
            return Err(format!("saved fill costs do not match attribution at {at}"));
        }
        let unfilled = number(attr, "unfilled_notional")?;
        if unfilled < 0.0 {
            return Err("negative unfilled notional".into());
        }
        let filled = finite(filled);
        let requested = filled + unfilled;
        daily.push(Day {
            as_of: at,
            nav,
            nav_before: before,
            return_net: nav / before - 1.0,
            gross_weight: gross,
            net_weight: net,
            long_weight: long,
            short_weight: short,
            long_held_notional: nav * long,
            short_held_notional: nav * short,
            filled_reference_notional: filled,
            unfilled_reference_notional: unfilled,
            requested_reference_notional: requested,
            unfilled_fraction: (requested > 0.0).then_some(unfilled / requested),
            long_price_pnl: long_pnl,
            short_price_pnl: short_pnl,
            funding_pnl: funding,
            cost,
            fills: fills.len(),
            held_long_names: positions.values().filter(|q| **q > Decimal::ZERO).count(),
            held_short_names: positions.values().filter(|q| **q < Decimal::ZERO).count(),
            flow,
        });
        previous_nav = nav;
    }
    let vector = |f: fn(&Day) -> f64| daily.iter().map(f).collect::<Vec<_>>();
    let sum = |f: fn(&Day) -> f64| daily.iter().map(f).sum::<f64>();
    let returns = vector(|d| d.return_net);
    let unfilled: Vec<_> = daily.iter().filter_map(|d| d.unfilled_fraction).collect();
    if unfilled.is_empty() || sd(&returns) == 0.0 {
        return Err("sample has no requested trades or return variance".into());
    }
    let filled_total = sum(|d| d.filled_reference_notional);
    let unfilled_total = sum(|d| d.unfilled_reference_notional);
    let abs_days = daily.iter().filter(|d| d.net_weight.abs() > 0.10).count();
    let summary = Summary {
        path: path.display().to_string(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        initial_cash: initial,
        start: daily[0].as_of,
        end: daily.last().unwrap().as_of,
        days: daily.len(),
        recalculated_sharpe: mean(&returns) / sd(&returns) * 365.0_f64.sqrt(),
        total_return: previous_nav / initial - 1.0,
        gross_weight: distribution(&vector(|d| d.gross_weight)),
        net_weight: distribution(&vector(|d| d.net_weight)),
        long_weight: distribution(&vector(|d| d.long_weight)),
        short_weight: distribution(&vector(|d| d.short_weight)),
        long_held_notional: distribution(&vector(|d| d.long_held_notional)),
        short_held_notional: distribution(&vector(|d| d.short_held_notional)),
        days_net_over_010: daily.iter().filter(|d| d.net_weight > 0.10).count(),
        days_net_below_minus010: daily.iter().filter(|d| d.net_weight < -0.10).count(),
        days_abs_net_over_010: abs_days,
        days_abs_net_over_010_fraction: abs_days as f64 / daily.len() as f64,
        days_gross_below_040: daily.iter().filter(|d| d.gross_weight < 0.4).count(),
        days_unfilled: daily
            .iter()
            .filter(|d| d.unfilled_reference_notional > 0.01)
            .count(),
        unfilled_fraction_on_requested_days: distribution(&unfilled),
        total_filled_reference_notional: filled_total,
        total_unfilled_reference_notional: unfilled_total,
        total_requested_reference_notional: filled_total + unfilled_total,
        aggregate_unfilled_fraction: unfilled_total / (filled_total + unfilled_total),
        total_long_price_pnl: sum(|d| d.long_price_pnl),
        total_short_price_pnl: sum(|d| d.short_price_pnl),
        total_funding_pnl: sum(|d| d.funding_pnl),
        total_cost: sum(|d| d.cost),
        gross_component_return_correlation: correlation(
            &vector(|d| d.long_price_pnl / d.nav_before),
            &vector(|d| d.short_price_pnl / d.nav_before),
        ),
        gross_component_pnl_correlation: correlation(
            &vector(|d| d.long_price_pnl),
            &vector(|d| d.short_price_pnl),
        ),
        total_add_long: sum(|d| d.flow.add_long),
        total_reduce_long: sum(|d| d.flow.reduce_long),
        total_add_short: sum(|d| d.flow.add_short),
        total_reduce_short: sum(|d| d.flow.reduce_short),
        max_nav_reconciliation_error: max_error,
        metadata: doc["metadata"].clone(),
        execution: doc["execution"].clone(),
    };
    Ok(Audit { summary, daily })
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: lighter-capacity-audit NATIVE_JSON PROXY_JSON OUTPUT_JSON".into());
    }
    let native = audit(Path::new(&args[0]))?;
    let proxy = audit(Path::new(&args[1]))?;
    if native.summary.start != proxy.summary.start
        || native.summary.end != proxy.summary.end
        || native.summary.initial_cash != proxy.summary.initial_cash
    {
        return Err("reports must have matching dates and initial capital".into());
    }
    let output = serde_json::json!({"schema_version":"lighter-capacity-audit-1","limitations":["Gross component correlations are not standalone strategy returns.","Held long/short notional is reconstructed from saved post-fill gross/net and NAV; named-position counts assume no unreported settlements.","Requested notional includes submitted orders only. Wholly unfilled order sides and planner-deferred orders are absent from saved reports.","Native/proxy reports differ in prices, funding and liquidity; the comparison does not isolate capacity alone.","Native execution caps each order at participation times trailing daily ADV / 24. This remains an hourly liquidity proxy, not measured opening depth."],"native":native,"proxy":proxy});
    let out = Path::new(&args[2]);
    if out.exists() {
        return Err("output already exists; refusing overwrite".into());
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        out,
        serde_json::to_vec_pretty(&output).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("wrote {}", out.display());
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("lighter-capacity-audit: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_statistics_and_component_correlation() {
        let d = distribution(&[4.0, 1.0, 3.0, 2.0]);
        assert_eq!(d.mean, 2.5);
        assert_eq!(d.p50, 2.5);
        assert_eq!(d.p25, 1.75);
        assert_eq!(correlation(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]), Some(-1.0));
        assert_eq!(correlation(&[1.0, 1.0], &[2.0, 3.0]), None);
    }
    #[test]
    fn crossing_fill_splits_hedge_reduction_from_new_risk() {
        assert_eq!(
            fill_flow(Decimal::from(-3), Decimal::from(5), Decimal::from(10)),
            (20.0, 0.0, 0.0, 30.0)
        );
        assert_eq!(
            fill_flow(Decimal::from(3), Decimal::from(-5), Decimal::from(10)),
            (0.0, 30.0, 20.0, 0.0)
        );
    }
}
