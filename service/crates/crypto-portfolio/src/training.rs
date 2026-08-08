//! Training matrix construction using the exact runtime feature code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDate, Utc};

use crate::{config::Config, store, universe};

#[derive(Debug, serde::Serialize)]
pub struct TrainingRow {
    pub date: NaiveDate,
    pub asset: String,
    /// Demeaned forward return - the reward the model has always been fit on.
    pub target: f64,
    /// The same return before demeaning, plus the asset's realised vol at the
    /// decision. Emitted so a trainer can build ALTERNATIVE rewards - e.g. the
    /// risk-adjusted target demean(ret/vol) - without a second matrix pass.
    /// The record only documents the demeaned-return reward; whether a
    /// risk-adjusted one came later lived in the uncommitted harness, so the
    /// comparison has to be re-run rather than remembered.
    pub raw_ret: f64,
    pub vol: Option<f64>,
    pub features: BTreeMap<String, f64>,
}

pub struct TrainingMatrix {
    pub features: Vec<String>,
    pub rows: Vec<TrainingRow>,
}

pub fn build(
    root: &Path,
    cfg: &Config,
    start: NaiveDate,
    end: NaiveDate,
    lag_hours: i64,
    hold_hours: i64,
    funding_window: features_crypto::FundingWindow,
) -> Result<TrainingMatrix, String> {
    if end < start {
        return Err("training end precedes start".into());
    }
    if lag_hours < 0 || hold_hours <= 0 {
        return Err("training lag must be non-negative and holding period positive".into());
    }
    let daily_bars: Vec<_> = store::read(root, cfg.interval_s as i32)?
        .into_iter()
        .filter(|b| canonical(&b.asset))
        .collect();
    let all_hourly: Vec<_> = store::read(root, 3_600)?
        .into_iter()
        .filter(|b| canonical(&b.asset))
        .collect();
    if all_hourly.is_empty() {
        return Err("no hourly bars; training inputs require them".into());
    }
    let first_decision = start.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let last_decision = end.and_hms_opt(0, 0, 0).unwrap().and_utc();
    // The largest hourly window is 168 observations. One earlier bar is
    // required to form the first return. Targets need bars through the final
    // delayed exit, but those later bars must never enter feature calculation.
    let feature_start = first_decision - Duration::hours(169);
    let feature_end = last_decision - Duration::hours(1);
    let target_end = last_decision + Duration::hours(lag_hours + hold_hours);
    let opens = marked_opens(&all_hourly, first_decision, target_end);
    let hourly_bars: Vec<_> = all_hourly
        .into_iter()
        .filter(|bar| bar.ts_utc >= feature_start && bar.ts_utc <= feature_end)
        .collect();
    let listings = store::funding_listings(root)?;
    let daily = features_crypto::daily(
        &daily_bars,
        cfg.benchmark.as_deref(),
        &listings,
        &crate::funding::load(root)?,
        funding_window,
    )?;
    let hourly =
        features_crypto::hourly_before_daily_decision(&hourly_bars, cfg.benchmark.as_deref())?;

    let daily_by_key: BTreeMap<_, _> = daily
        .iter()
        .map(|r| ((r.ts_utc, r.asset.as_str()), r))
        .collect();
    let mut hourly_by_ts: BTreeMap<DateTime<Utc>, Vec<_>> = BTreeMap::new();
    for row in &hourly {
        hourly_by_ts.entry(row.ts_utc).or_default().push(row);
    }
    let raw_names: Vec<String> = features_crypto::DAILY_FEATURE_NAMES
        .iter()
        .chain(features_crypto::HOURLY_FEATURE_NAMES.iter())
        .map(|v| (*v).to_owned())
        .collect();
    let feature_names: Vec<String> = raw_names.iter().map(|v| format!("x_{v}")).collect();
    let mut rows = Vec::new();
    let mut day = start;
    while day <= end {
        let stamp = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let hourly_stamp = stamp - Duration::hours(1);
        let Some(hour_rows) = hourly_by_ts.get(&hourly_stamp) else {
            day = day.succ_opt().unwrap();
            continue;
        };
        let members = match universe::load(root, stamp) {
            Ok(v) => v,
            Err(_) => {
                day = day.succ_opt().unwrap();
                continue;
            }
        };
        let eligible: BTreeSet<_> = members
            .into_iter()
            .filter(|m| m.eligible)
            .map(|m| m.asset)
            .collect();
        let daily_stamp = stamp - Duration::seconds(cfg.interval_s);
        let entry = stamp + Duration::hours(lag_hours);
        let exit = entry + Duration::hours(hold_hours);
        let mut block = Vec::new();
        for hour in hour_rows {
            let asset = hour.asset.as_str();
            if !eligible.contains(asset) {
                continue;
            }
            if listings.get(asset).is_none_or(|listed| *listed > day) {
                continue;
            }
            let (Some(p0), Some(p1)) = (
                opens.get(&(entry, asset.to_owned())),
                opens.get(&(exit, asset.to_owned())),
            ) else {
                continue;
            };
            let Some(daily) = daily_by_key.get(&(daily_stamp, asset)) else {
                continue;
            };
            let values = raw_names
                .iter()
                .map(|name| daily.value(name).or_else(|| hour.value(name)))
                .collect::<Vec<_>>();
            // Vol at the decision, same preference order as inference: the
            // 24h realised vol if the hourly store has it, else 30-day daily.
            let vol = hour.rv_24h.or(daily.vol_30).filter(|v| *v > 0.0);
            block.push((asset.to_owned(), p1 / p0 - 1.0, vol, values));
        }
        if block.len() >= 12 {
            let raw = block.iter().map(|(_, _, _, v)| v.clone()).collect::<Vec<_>>();
            let normalised = features_crypto::rank_normalise(&raw)?;
            let mean_target = block.iter().map(|(_, y, _, _)| y).sum::<f64>() / block.len() as f64;
            for ((asset, target, vol, _), values) in block.into_iter().zip(normalised) {
                rows.push(TrainingRow {
                    date: day,
                    asset,
                    target: target - mean_target,
                    raw_ret: target,
                    vol,
                    features: feature_names.iter().cloned().zip(values).collect(),
                });
            }
        }
        day = day.succ_opt().unwrap();
    }
    if rows.is_empty() {
        return Err(format!("no usable training rows between {start} and {end}"));
    }
    Ok(TrainingMatrix {
        features: feature_names,
        rows,
    })
}

/// Mark opens exactly like the production discontinuity rule while retaining
/// only target timestamps. Scanning the full sorted series is necessary: a
/// ticker identity break before the requested training window freezes that
/// asset's mark forever and cannot be forgotten by slicing the input.
fn marked_opens(
    bars: &[features_crypto::Bar],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> BTreeMap<(DateTime<Utc>, String), f64> {
    let mut out = BTreeMap::new();
    let mut current = "";
    let mut previous_close = None;
    let mut previous_open = None;
    let mut frozen_open = None;
    for bar in bars {
        if bar.asset != current {
            current = &bar.asset;
            previous_close = None;
            previous_open = None;
            frozen_open = None;
        }
        if frozen_open.is_none()
            && previous_close
                .is_some_and(|previous: f64| (bar.close / previous).ln() > std::f64::consts::LN_10)
        {
            // At the first break, freeze at the prior bar's open. The bars are
            // sorted by (asset, timestamp) by the store reader.
            frozen_open = previous_open;
        }
        if bar.ts_utc >= start && bar.ts_utc <= end {
            out.insert(
                (bar.ts_utc, bar.asset.clone()),
                frozen_open.unwrap_or(bar.open),
            );
        }
        previous_close = Some(bar.close);
        previous_open = Some(bar.open);
    }
    out
}

fn canonical(asset: &str) -> bool {
    !asset.is_empty()
        && asset.len() <= 20
        && asset
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_freeze_at_the_first_identity_break() {
        let start = "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let bars = [(10.0, 10.0), (11.0, 11.0), (120.0, 121.0), (130.0, 130.0)]
            .into_iter()
            .enumerate()
            .map(|(index, (open, close))| features_crypto::Bar {
                ts_utc: start + Duration::hours(index as i64),
                asset: "LUNA".into(),
                interval_s: 3_600,
                open,
                high: open.max(close),
                low: open.min(close),
                close,
                volume: 1.0,
                quote_volume: Some(1.0),
                trades: Some(1),
            })
            .collect::<Vec<_>>();
        let out = marked_opens(&bars, start, start + Duration::hours(3));
        assert_eq!(out[&(start + Duration::hours(1), "LUNA".into())], 11.0);
        assert_eq!(out[&(start + Duration::hours(2), "LUNA".into())], 11.0);
        assert_eq!(out[&(start + Duration::hours(3), "LUNA".into())], 11.0);
    }
}
