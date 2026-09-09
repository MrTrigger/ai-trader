//! Training matrix construction using the exact runtime feature code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use rust_decimal::prelude::ToPrimitive;

use crate::{config::Config, store, universe};

pub const TARGET_CONTRACT: &str = "rust-crypto-targets-1";

#[derive(Debug, serde::Serialize)]
pub struct TrainingRow {
    pub date: NaiveDate,
    /// Hour-of-day of the decision stamp. 0 for the daily grid; the per-date
    /// demeaning and ranking in the trainer must group by (date, slot) or a
    /// sub-daily grid folds two cross-sections into one.
    pub slot: u32,
    pub asset: String,
    /// Demeaned forward return - the reward the model has always been fit on.
    pub target: f64,
    /// Demeaned raw_ret/vol within this date and slot's vol-bearing rows.
    pub target_per_risk: Option<f64>,
    /// raw_ret/vol without cross-sectional demeaning.
    pub target_per_risk_abs: Option<f64>,
    /// The same return before demeaning, plus the asset's realised vol at the
    /// decision. These raw columns support target audits. Trainers consume
    /// the finalized target fields and do not transform these columns.
    pub raw_ret: f64,
    pub vol: Option<f64>,
    pub features: BTreeMap<String, f64>,
}

pub struct TrainingMatrix {
    pub market_context: Option<&'static str>,
    pub features: Vec<String>,
    pub rows: Vec<TrainingRow>,
}

#[derive(Clone, Copy, Debug)]
pub enum LabelPrices<'a> {
    ExternalHourly,
    NativeDaily(&'a [features_crypto::Bar]),
    NativeHourly(&'a crate::lighter_hourly::LabelMarks),
}

pub fn build(
    root: &Path,
    cfg: &Config,
    start: NaiveDate,
    end: NaiveDate,
    lag_hours: i64,
    hold_hours: i64,
    funding_window: features_crypto::FundingWindow,
    include_unlisted: bool,
    step_hours: i64,
) -> Result<TrainingMatrix, String> {
    build_for_universe(
        root,
        root,
        cfg,
        start,
        end,
        lag_hours,
        hold_hours,
        funding_window,
        include_unlisted,
        step_hours,
        LabelPrices::ExternalHourly,
        &BTreeSet::new(),
        12,
        false,
        false,
    )
}

/// Build with a separately generated venue-aware universe and an explicit
/// label-price source. `omitted_decisions` removes whole cross-sections before
/// feature normalisation; it is intended for recorded native-data quarantine.
pub fn build_for_universe(
    root: &Path,
    universe_root: &Path,
    cfg: &Config,
    start: NaiveDate,
    end: NaiveDate,
    lag_hours: i64,
    hold_hours: i64,
    funding_window: features_crypto::FundingWindow,
    include_unlisted: bool,
    step_hours: i64,
    label_prices: LabelPrices<'_>,
    omitted_decisions: &BTreeSet<NaiveDate>,
    min_cross_section: usize,
    apply_runtime_eligibility: bool,
    market_context: bool,
) -> Result<TrainingMatrix, String> {
    if min_cross_section < 2 {
        return Err("minimum cross-section must be at least 2".into());
    }
    if apply_runtime_eligibility && cfg.min_hourly_quote_volume != rust_decimal::Decimal::ZERO {
        return Err("venue-aware training cannot reproduce a nonzero hourly-volume profile".into());
    }
    if matches!(label_prices, LabelPrices::NativeDaily(_))
        && (lag_hours != 0 || hold_hours != 24 || step_hours != 24)
    {
        return Err("native daily labels require lag 0h, hold 24h and step 24h".into());
    }
    if step_hours <= 0 || 24 % step_hours != 0 && step_hours % 24 != 0 {
        return Err("step_hours must be positive and divide or multiply 24".into());
    }
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
    let external_opens = marked_opens(&all_hourly, first_decision, target_end);
    let native_opens: BTreeMap<_, _> = match label_prices {
        LabelPrices::ExternalHourly => BTreeMap::new(),
        LabelPrices::NativeDaily(bars) => bars
            .iter()
            .filter(|bar| bar.volume > 0.0 && bar.quote_volume.is_some_and(|v| v > 0.0))
            .map(|bar| ((bar.ts_utc, bar.asset.clone()), bar.open))
            .collect(),
        LabelPrices::NativeHourly(history) => history
            .opens
            .iter()
            .filter(|((at, _), _)| *at >= first_decision && *at <= target_end)
            .map(|((at, asset), open)| {
                let price = open
                    .to_f64()
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .ok_or_else(|| {
                        format!("invalid native hourly label price for {asset} at {at}")
                    })?;
                Ok(((*at, asset.clone()), price))
            })
            .collect::<Result<_, String>>()?,
    };
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
    let mut feature_names: Vec<String> = raw_names.iter().map(|v| format!("x_{v}")).collect();
    let mut rows = Vec::new();
    let mut stamp = start.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let end_stamp = end.and_hms_opt(0, 0, 0).unwrap().and_utc();
    while stamp <= end_stamp {
        let day = stamp.date_naive();
        if omitted_decisions.contains(&day) {
            stamp += Duration::hours(step_hours);
            continue;
        }
        let hourly_stamp = stamp - Duration::hours(1);
        let Some(hour_rows) = hourly_by_ts.get(&hourly_stamp) else {
            stamp += Duration::hours(step_hours);
            continue;
        };
        let members = match universe::load(universe_root, stamp) {
            Ok(v) => v,
            Err(_) => {
                stamp += Duration::hours(step_hours);
                continue;
            }
        };
        let eligible: BTreeSet<_> = members
            .into_iter()
            .filter(|m| m.eligible)
            .map(|m| m.asset)
            .collect();
        // The newest daily bar whose close precedes the stamp: for a 00:00
        // decision that is yesterday's bar exactly as before; for an intraday
        // slot it is the same bar, now up to step_hours stale - the hourly
        // block carries the freshness.
        let daily_stamp = (stamp - Duration::hours(24))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let entry = stamp + Duration::hours(lag_hours);
        let exit = entry + Duration::hours(hold_hours);
        let mut block = Vec::new();
        for hour in hour_rows {
            let asset = hour.asset.as_str();
            if !eligible.contains(asset) {
                continue;
            }
            // A perp listing gates TRADING, not learning: the ranker can be
            // taught by 2017-2018 spot cross-sections even though no perp
            // existed to trade them. Inference eligibility is untouched - this
            // flag only widens what the trees get to see.
            if !include_unlisted && listings.get(asset).is_none_or(|listed| *listed > day) {
                continue;
            }
            let Some(daily) = daily_by_key.get(&(daily_stamp, asset)) else {
                continue;
            };
            if apply_runtime_eligibility && !runtime_daily_eligible(daily, cfg)? {
                continue;
            }
            let values = raw_names
                .iter()
                .map(|name| daily.value(name).or_else(|| hour.value(name)))
                .collect::<Vec<_>>();
            // Vol at the decision, same preference order as inference: the
            // 24h realised vol if the hourly store has it, else 30-day daily.
            let vol = hour.rv_24h.or(daily.vol_30).filter(|v| *v > 0.0);
            let prices = match label_prices {
                LabelPrices::ExternalHourly => &external_opens,
                LabelPrices::NativeDaily(_) | LabelPrices::NativeHourly(_) => &native_opens,
            };
            let label = forward_return(
                prices,
                asset,
                entry,
                exit,
                matches!(label_prices, LabelPrices::NativeHourly(_)),
            )?;
            block.push(TrainingCandidate {
                asset: asset.to_owned(),
                label,
                vol,
                values,
            });
        }
        let mut completed = finalize_cross_section(
            day,
            stamp.time().hour(),
            &feature_names,
            block,
            min_cross_section,
        )?;
        if market_context && !completed.is_empty() {
            let context = crate::market_context::values(
                daily_by_key.get(&(daily_stamp, "BTC")).copied(),
                daily_stamp.date_naive(),
            )?;
            append_context(&mut completed, context);
        }
        rows.extend(completed);
        stamp += Duration::hours(step_hours);
    }
    if rows.is_empty() {
        return Err(format!("no usable training rows between {start} and {end}"));
    }
    finalize_per_risk_targets(&mut rows)?;
    if market_context {
        feature_names.extend(
            crate::market_context::FEATURES
                .into_iter()
                .map(str::to_owned),
        );
    }
    Ok(TrainingMatrix {
        market_context: market_context.then_some(crate::market_context::CONTRACT),
        features: feature_names,
        rows,
    })
}

fn forward_return(
    prices: &BTreeMap<(DateTime<Utc>, String), f64>,
    asset: &str,
    entry: DateTime<Utc>,
    exit: DateTime<Utc>,
    require_complete: bool,
) -> Result<Option<f64>, String> {
    let label = prices
        .get(&(entry, asset.to_owned()))
        .zip(prices.get(&(exit, asset.to_owned())))
        .map(|(p0, p1)| p1 / p0 - 1.0);
    if require_complete && label.is_none() {
        return Err(format!(
            "native hourly label lacks {asset} entry {entry} or exit {exit}"
        ));
    }
    Ok(label)
}

fn append_context(rows: &mut [TrainingRow], values: [f64; 4]) {
    for row in rows {
        row.features.extend(
            crate::market_context::FEATURES
                .into_iter()
                .zip(values)
                .map(|(name, value)| (name.to_owned(), value)),
        );
    }
}

fn runtime_daily_eligible(row: &features_crypto::DailyRow, cfg: &Config) -> Result<bool, String> {
    let min_dollar_volume = cfg
        .min_dollar_volume
        .to_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or("invalid minimum dollar volume")?;
    let min_volatility = cfg
        .min_volatility
        .to_f64()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or("invalid minimum volatility")?;
    Ok(row.bars_available >= cfg.min_history_bars
        && row.adv_quote.is_some_and(|v| v >= min_dollar_volume)
        && !row.vol_30.is_some_and(|v| v < min_volatility))
}

struct TrainingCandidate {
    asset: String,
    label: Option<f64>,
    vol: Option<f64>,
    values: Vec<Option<f64>>,
}

fn finalize_cross_section(
    date: NaiveDate,
    slot: u32,
    feature_names: &[String],
    candidates: Vec<TrainingCandidate>,
    min_cross_section: usize,
) -> Result<Vec<TrainingRow>, String> {
    if candidates.len() < min_cross_section {
        return Ok(Vec::new());
    }
    let raw: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.values.clone())
        .collect();
    let normalised = features_crypto::rank_normalise(&raw)?;
    let labels: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| candidate.label)
        .collect();
    if labels.len() < 12 {
        return Ok(Vec::new());
    }
    if labels.iter().any(|label| !label.is_finite()) {
        return Err("nonfinite forward training return".into());
    }
    let mean_target = labels.iter().sum::<f64>() / labels.len() as f64;
    Ok(candidates
        .into_iter()
        .zip(normalised)
        .filter_map(|(candidate, values)| {
            candidate.label.map(|label| TrainingRow {
                date,
                slot,
                asset: candidate.asset,
                target: label - mean_target,
                target_per_risk: None,
                target_per_risk_abs: None,
                raw_ret: label,
                vol: candidate.vol,
                features: feature_names.iter().cloned().zip(values).collect(),
            })
        })
        .collect())
}

fn finalize_per_risk_targets(rows: &mut [TrainingRow]) -> Result<(), String> {
    let mut groups = BTreeMap::<(NaiveDate, u32), (f64, usize)>::new();
    for row in rows.iter_mut() {
        row.target_per_risk = None;
        row.target_per_risk_abs = match row.vol {
            None => None,
            Some(vol) => {
                let target = row.raw_ret / vol;
                if !vol.is_finite() || vol <= 0.0 || !target.is_finite() {
                    return Err(format!(
                        "{} {} slot {} has invalid per-risk target inputs",
                        row.asset, row.date, row.slot
                    ));
                }
                let group = groups.entry((row.date, row.slot)).or_default();
                group.0 += target;
                group.1 += 1;
                Some(target)
            }
        };
    }
    for row in rows {
        if let Some(absolute) = row.target_per_risk_abs {
            let (sum, count) = groups[&(row.date, row.slot)];
            let relative = absolute - sum / count as f64;
            if !relative.is_finite() {
                return Err("nonfinite group mean in per-risk targets".into());
            }
            row.target_per_risk = Some(relative);
        }
    }
    Ok(())
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
    fn native_hourly_labels_require_exact_delayed_marks_without_fallback() {
        let decision: DateTime<Utc> = "2025-02-25T00:00:00Z".parse().unwrap();
        let entry = decision + Duration::hours(1);
        let exit = decision + Duration::hours(25);
        let mut prices = BTreeMap::from([
            ((decision, "BTC".into()), 500.0),
            ((entry, "BTC".into()), 100.0),
            ((exit, "BTC".into()), 110.0),
            ((exit + Duration::hours(1), "BTC".into()), 900.0),
        ]);
        assert!(
            (forward_return(&prices, "BTC", entry, exit, true)
                .unwrap()
                .unwrap()
                - 0.1)
                .abs()
                < 1e-12
        );
        prices.remove(&(exit, "BTC".into()));
        assert!(forward_return(&prices, "BTC", entry, exit, true)
            .unwrap_err()
            .contains("native hourly label lacks BTC"));
        assert_eq!(
            forward_return(&prices, "BTC", entry, exit, false).unwrap(),
            None
        );
    }

    fn target_row(date: &str, slot: u32, raw_ret: f64, vol: Option<f64>) -> TrainingRow {
        TrainingRow {
            date: date.parse().unwrap(),
            slot,
            asset: "fixture".into(),
            target: 123.0,
            target_per_risk: None,
            target_per_risk_abs: None,
            raw_ret,
            vol,
            features: BTreeMap::from([("x_ret_7".into(), 0.5)]),
        }
    }

    #[test]
    fn raw_context_addition_preserves_all_ranked_inputs_rows_and_targets() {
        let date = "2025-02-01".parse().unwrap();
        let block = (0..13)
            .map(|i| TrainingCandidate {
                asset: format!("A{i}"),
                label: (i != 12).then_some(i as f64 / 1000.0),
                vol: Some(0.5),
                values: vec![Some(i as f64)],
            })
            .collect();
        let mut rows = finalize_cross_section(date, 0, &["x_ret_7".into()], block, 12).unwrap();
        finalize_per_risk_targets(&mut rows).unwrap();
        let before = serde_json::to_value(&rows).unwrap();
        append_context(&mut rows, [0.14, -0.22, 0.31, 0.55]);
        assert_eq!(rows.len(), 12);
        for row in &rows {
            assert_eq!(row.features["mkt_btc_ret_7d"], 0.14);
            assert_eq!(row.features["mkt_btc_ret_30d"], -0.22);
            assert_eq!(row.features["mkt_btc_vol_30d"], 0.55);
        }
        let mut stripped = serde_json::to_value(&rows).unwrap();
        for row in stripped.as_array_mut().unwrap() {
            for name in crate::market_context::FEATURES {
                row["features"].as_object_mut().unwrap().remove(name);
            }
        }
        assert_eq!(stripped, before);
    }

    #[test]
    fn final_per_risk_targets_group_by_date_slot_and_exclude_missing_vol() {
        let mut rows = vec![
            target_row("2026-01-01", 0, 0.02, Some(0.1)),
            target_row("2026-01-01", 0, 0.06, Some(0.2)),
            target_row("2026-01-01", 0, 999.0, None),
            target_row("2026-01-01", 12, 0.04, Some(0.1)),
            target_row("2026-01-02", 0, 0.02, Some(0.2)),
            target_row("2026-01-03", 0, 0.1, None),
        ];
        finalize_per_risk_targets(&mut rows).unwrap();
        assert!((rows[0].target_per_risk_abs.unwrap() - 0.2).abs() < 1e-12);
        assert!((rows[0].target_per_risk.unwrap() + 0.05).abs() < 1e-12);
        assert!((rows[1].target_per_risk.unwrap() - 0.05).abs() < 1e-12);
        assert_eq!(rows[2].target_per_risk, None);
        assert_eq!(rows[2].target_per_risk_abs, None);
        assert_eq!(rows[3].target_per_risk, Some(0.0));
        assert_eq!(rows[4].target_per_risk, Some(0.0));
        assert_eq!(rows[5].target_per_risk, None);
        assert!(rows
            .iter()
            .all(|row| row.target == 123.0 && row.features["x_ret_7"] == 0.5));
        let serialized = serde_json::to_value(&rows[2]).unwrap();
        assert!(serialized["target_per_risk"].is_null());
    }

    #[test]
    fn per_risk_targets_reject_invalid_present_vol_or_return() {
        for vol in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            assert!(
                finalize_per_risk_targets(&mut [target_row("2026-01-01", 0, 0.02, Some(vol))])
                    .is_err()
            );
        }
        assert!(finalize_per_risk_targets(&mut [target_row(
            "2026-01-01",
            0,
            f64::INFINITY,
            Some(0.1)
        )])
        .is_err());
    }

    #[test]
    fn missing_future_exit_does_not_change_remaining_feature_vectors() {
        let candidates = |missing_exit: bool| {
            (0..13)
                .map(|i| TrainingCandidate {
                    asset: format!("A{i:02}"),
                    label: if missing_exit && i == 12 {
                        None
                    } else {
                        Some(i as f64 * 0.01)
                    },
                    vol: Some(0.1),
                    values: vec![Some(i as f64), Some((12 - i) as f64)],
                })
                .collect()
        };
        let date = "2026-01-01".parse().unwrap();
        let names = ["x_ret_7".into(), "x_ret_30".into()];
        let complete = finalize_cross_section(date, 0, &names, candidates(false), 12).unwrap();
        let missing = finalize_cross_section(date, 0, &names, candidates(true), 12).unwrap();
        assert_eq!(complete.len(), 13);
        assert_eq!(missing.len(), 12);
        for (full, partial) in complete.iter().zip(&missing) {
            assert_eq!(full.asset, partial.asset);
            assert_eq!(full.features, partial.features);
        }
        assert!((missing.iter().map(|row| row.target).sum::<f64>()).abs() < 1e-12);
        assert!((missing[0].target + 0.055).abs() < 1e-12);
        let removed_cohort: Vec<_> = candidates(false).into_iter().take(12).collect();
        let incorrectly_ranked =
            finalize_cross_section(date, 0, &names, removed_cohort, 12).unwrap();
        assert_ne!(missing[11].features, incorrectly_ranked[11].features);
    }

    #[test]
    fn venue_minimum_is_applied_to_the_decision_cohort() {
        let candidates = (0..14)
            .map(|i| TrainingCandidate {
                asset: format!("A{i:02}"),
                label: Some(i as f64 * 0.01),
                vol: Some(0.1),
                values: vec![Some(i as f64)],
            })
            .collect();
        let date = "2026-01-01".parse().unwrap();
        assert!(
            finalize_cross_section(date, 0, &["x_ret_7".into()], candidates, 15)
                .unwrap()
                .is_empty()
        );
    }

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
