//! Cross-sectional information coefficient measured from Rust-owned features.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::ToPrimitive;

use crate::config::Config;
use crate::{store, universe};

pub const MIN_CROSS_SECTION: usize = 10;

#[derive(Debug, serde::Serialize)]
pub struct PeriodIc {
    pub as_of: DateTime<Utc>,
    pub n_assets: usize,
    pub ic: f64,
}

#[derive(Debug, serde::Serialize)]
pub struct IcResult {
    pub horizon_days: i64,
    pub step_days: i64,
    pub periods: Vec<PeriodIc>,
    pub disclosures: Vec<String>,
    pub n_periods: usize,
    pub n_observations: usize,
    pub mean_ic: f64,
    pub std_ic: f64,
    pub effective_n: f64,
    pub t_stat: f64,
    pub hit_rate: f64,
    pub distinguishable_from_zero: bool,
}

pub fn measure(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    score: &str,
    horizons: &[i64],
) -> Result<Vec<IcResult>, String> {
    if !features_crypto::DAILY_FEATURE_NAMES.contains(&score) {
        return Err(format!(
            "IC score {score:?} is not a Rust daily feature; available: {:?}",
            features_crypto::DAILY_FEATURE_NAMES
        ));
    }
    let bars: Vec<_> = store::read(root, cfg.interval_s as i32)?
        .into_iter()
        .filter(|bar| canonical(&bar.asset))
        .collect();
    if bars.is_empty() {
        return Err("no daily bars in the store".into());
    }
    let listings = store::funding_listings(root)?;
    let features = features_crypto::daily(&bars, cfg.benchmark.as_deref(), &listings)?;
    let by_stamp: BTreeMap<_, Vec<_>> = {
        let mut grouped: BTreeMap<DateTime<Utc>, Vec<_>> = BTreeMap::new();
        for row in &features {
            grouped.entry(row.ts_utc).or_default().push(row);
        }
        grouped
    };
    let mut prices: BTreeMap<String, Vec<(DateTime<Utc>, f64)>> = BTreeMap::new();
    for row in &features {
        prices
            .entry(row.asset.clone())
            .or_default()
            .push((row.ts_utc, row.mark_open));
    }
    for rows in prices.values_mut() {
        rows.sort_by_key(|row| row.0);
    }

    let cadence = Duration::seconds(cfg.interval_s * cfg.rebalance_every.max(1) as i64);
    let step_days = cadence.num_days().max(1);
    let mut results = Vec::new();
    for horizon_days in horizons {
        if *horizon_days <= 0 {
            return Err(format!("IC horizon must be positive, got {horizon_days}"));
        }
        let mut periods = Vec::new();
        let mut thin = 0usize;
        let mut missing_snapshot = 0usize;
        let mut as_of = start;
        while as_of <= end {
            let members = match universe::load(root, as_of) {
                Ok(members) => members,
                Err(_) => {
                    missing_snapshot += 1;
                    as_of += cadence;
                    continue;
                }
            };
            let eligible: BTreeSet<_> = members
                .into_iter()
                .filter(|member| member.eligible)
                .map(|member| member.asset)
                .collect();
            let feature_stamp = as_of - Duration::seconds(cfg.interval_s);
            let mut cross = Vec::new();
            for row in by_stamp.get(&feature_stamp).into_iter().flatten() {
                if !eligible.contains(&row.asset)
                    || row.bars_available < cfg.min_history_bars
                    || row.adv_quote.is_none_or(|value| {
                        value < cfg.min_dollar_volume.to_f64().unwrap_or(f64::INFINITY)
                    })
                    || row.vol_30.is_none_or(|value| {
                        value < cfg.min_volatility.to_f64().unwrap_or(f64::INFINITY)
                    })
                {
                    continue;
                }
                if let Some(value) = row.value(score).filter(|value| value.is_finite()) {
                    cross.push((row.asset.as_str(), value));
                }
            }
            if cross.len() < MIN_CROSS_SECTION {
                thin += 1;
                as_of += cadence;
                continue;
            }
            let exit = as_of + Duration::days(*horizon_days);
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for (asset, value) in cross {
                let Some(series) = prices.get(asset) else {
                    continue;
                };
                let entry = series.iter().find(|(stamp, _)| *stamp == as_of);
                let final_price = series.iter().rev().find(|(stamp, _)| *stamp <= exit);
                if let (Some((_, p0)), Some((_, p1))) = (entry, final_price) {
                    if *p0 > 0.0 {
                        xs.push(value);
                        ys.push(p1 / p0 - 1.0);
                    }
                }
            }
            if xs.len() >= MIN_CROSS_SECTION {
                if let Some(value) = spearman(&xs, &ys) {
                    periods.push(PeriodIc {
                        as_of,
                        n_assets: xs.len(),
                        ic: value,
                    });
                }
            } else {
                thin += 1;
            }
            as_of += cadence;
        }
        let mut disclosures = Vec::new();
        if missing_snapshot > 0 {
            disclosures.push(format!(
                "{missing_snapshot} date(s) had no universe snapshot and were skipped"
            ));
        }
        if thin > 0 {
            disclosures.push(format!(
                "{thin} date(s) had fewer than {MIN_CROSS_SECTION} rankable assets; a correlation over a handful is not a measurement"
            ));
        }
        if *horizon_days > step_days {
            disclosures.push(format!(
                "{horizon_days}d forward returns sampled every {step_days}d overlap {:.1}x; effective sample size is deflated",
                *horizon_days as f64 / step_days as f64
            ));
        }
        results.push(summarise(*horizon_days, step_days, periods, disclosures));
    }
    Ok(results)
}

fn summarise(
    horizon_days: i64,
    step_days: i64,
    periods: Vec<PeriodIc>,
    disclosures: Vec<String>,
) -> IcResult {
    let n_periods = periods.len();
    let n_observations = periods.iter().map(|period| period.n_assets).sum();
    let mean_ic = if periods.is_empty() {
        0.0
    } else {
        periods.iter().map(|period| period.ic).sum::<f64>() / n_periods as f64
    };
    let std_ic = if n_periods < 2 {
        0.0
    } else {
        (periods
            .iter()
            .map(|period| (period.ic - mean_ic).powi(2))
            .sum::<f64>()
            / (n_periods - 1) as f64)
            .sqrt()
    };
    let overlap = (horizon_days as f64 / step_days as f64).max(1.0);
    let effective_n = n_periods as f64 / overlap;
    let t_stat = if std_ic == 0.0 || n_periods < 2 {
        0.0
    } else {
        mean_ic / (std_ic / effective_n.sqrt())
    };
    let hit_rate = if periods.is_empty() {
        0.0
    } else {
        periods.iter().filter(|period| period.ic > 0.0).count() as f64 / n_periods as f64
    };
    IcResult {
        horizon_days,
        step_days,
        periods,
        disclosures,
        n_periods,
        n_observations,
        mean_ic,
        std_ic,
        effective_n,
        t_stat,
        hit_rate,
        distinguishable_from_zero: t_stat.abs() > 2.0,
    }
}

pub fn spearman(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return None;
    }
    let rx = ranks(xs);
    let ry = ranks(ys);
    let n = xs.len() as f64;
    let mx = rx.iter().sum::<f64>() / n;
    let my = ry.iter().sum::<f64>() / n;
    let covariance = rx
        .iter()
        .zip(&ry)
        .map(|(a, b)| (a - mx) * (b - my))
        .sum::<f64>();
    let vx = rx.iter().map(|value| (value - mx).powi(2)).sum::<f64>();
    let vy = ry.iter().map(|value| (value - my).powi(2)).sum::<f64>();
    (vx > 0.0 && vy > 0.0).then(|| covariance / (vx * vy).sqrt())
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<_> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
    let mut ranks = vec![0.0; values.len()];
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len() && values[order[end]] == values[order[start]] {
            end += 1;
        }
        let shared = ((start + 1) as f64 + end as f64) / 2.0;
        for index in &order[start..end] {
            ranks[*index] = shared;
        }
        start = end;
    }
    ranks
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

    #[test]
    fn spearman_averages_ties_and_detects_order() {
        assert_eq!(spearman(&[1.0, 2.0, 3.0], &[3.0, 2.0, 1.0]), Some(-1.0));
        let tied = spearman(&[1.0, 2.0, 2.0, 4.0], &[1.0, 3.0, 3.0, 4.0]).unwrap();
        assert!((tied - 1.0).abs() < 1e-12);
        assert_eq!(spearman(&[1.0, 1.0], &[1.0, 2.0]), None);
    }
}
