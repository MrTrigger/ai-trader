//! Scalper feature set for 1-minute crypto bars.
//!
//! Same discipline as `features_crypto`: a single left-to-right pass over
//! ascending bars with trailing state only. Appending future bars cannot
//! change a previously emitted row (`appending_bars_never_changes_emitted_rows`
//! below asserts this directly). A timestamp gap greater than 120s from the
//! previous bar resets every accumulator — the same identity-break latch
//! `features_crypto` uses, just on a per-minute clock instead of per-day.
//!
//! BTC context (`btc_ret_5`, `rel_ret_5`) is computed by running the same
//! causal r_5 fold over `btc_bars` and indexing the result by exact
//! timestamp; for BTC itself the caller passes the same slice for both
//! arguments, so `btc_ret_5` degenerates to `ret_5` and `rel_ret_5` to 0.0
//! with no special-casing.

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Datelike, Timelike, Utc};
use features_crypto::Bar;

pub const FEATURE_SET_VERSION: &str = "fs-rust-scalper-1";

pub const FEATURE_NAMES: [&str; 26] = [
    "ret_1",
    "ret_3",
    "ret_5",
    "ret_15",
    "ret_30",
    "ret_60",
    "mom_5",
    "mom_15",
    "mom_30",
    "mom_60",
    "vol_15",
    "vol_60",
    "vol_ratio_15_60",
    "vwap_dist_60",
    "volume_z_60",
    "volume_ratio_5_60",
    "trades_z_60",
    "hl_range",
    "body_frac",
    "upper_wick_frac",
    "lower_wick_frac",
    "tod_sin",
    "tod_cos",
    "dow",
    "btc_ret_5",
    "rel_ret_5",
];

const EPS: f64 = 1e-12;
const GAP_LIMIT_S: i64 = 120;
const VOL_WINDOW: usize = 60;
const CLOSE_WINDOW: usize = VOL_WINDOW + 1;

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureRow {
    pub ts_utc: DateTime<Utc>,
    pub asset: String,
    pub values: Vec<Option<f64>>,
}

/// Compute the 26-feature row set for `bars` (one asset's ascending 1m
/// bars), using `btc_bars` (BTC's ascending 1m bars, ascending) as context
/// for `btc_ret_5` / `rel_ret_5`. Pass the same slice for both to compute
/// BTC's own rows.
pub fn compute(bars: &[Bar], btc_bars: &[Bar]) -> Result<Vec<FeatureRow>, String> {
    validate_ascending(bars)?;
    validate_ascending(btc_bars)?;

    let ret5 = r5_series(bars);
    let btc_ret5 = r5_series(btc_bars);
    let btc_ret5_by_ts: BTreeMap<DateTime<Utc>, f64> = btc_bars
        .iter()
        .zip(btc_ret5.iter())
        .filter_map(|(b, r)| r.map(|v| (b.ts_utc, v)))
        .collect();

    let mut rows = Vec::with_capacity(bars.len());

    // Trailing state, cleared whenever a >120s gap is seen.
    let mut closes: VecDeque<f64> = VecDeque::new(); // up to 61, for ret_1..ret_60
    let mut rets: VecDeque<f64> = VecDeque::new(); // up to 60 one-minute log returns
    let mut vwap_terms: VecDeque<(f64, f64)> = VecDeque::new(); // (typical*volume, volume), up to 60
    let mut volumes: VecDeque<f64> = VecDeque::new(); // up to 60
    let mut trades: VecDeque<Option<i64>> = VecDeque::new(); // up to 60
    let mut prev_ts: Option<DateTime<Utc>> = None;

    for (idx, b) in bars.iter().enumerate() {
        let gapped = match prev_ts {
            Some(pts) => (b.ts_utc - pts).num_seconds() > GAP_LIMIT_S,
            None => false,
        };
        if gapped {
            closes.clear();
            rets.clear();
            vwap_terms.clear();
            volumes.clear();
            trades.clear();
        }
        prev_ts = Some(b.ts_utc);

        // One-minute log return, only if a previous close exists in this
        // window (i.e. we did not just reset).
        if let Some(&last_close) = closes.back() {
            let r1 = (b.close / last_close).ln();
            rets.push_back(r1);
            if rets.len() > VOL_WINDOW {
                rets.pop_front();
            }
        }
        closes.push_back(b.close);
        if closes.len() > CLOSE_WINDOW {
            closes.pop_front();
        }

        let typical = (b.high + b.low + b.close) / 3.0;
        vwap_terms.push_back((typical * b.volume, b.volume));
        if vwap_terms.len() > VOL_WINDOW {
            vwap_terms.pop_front();
        }

        volumes.push_back(b.volume);
        if volumes.len() > VOL_WINDOW {
            volumes.pop_front();
        }

        trades.push_back(b.trades);
        if trades.len() > VOL_WINDOW {
            trades.pop_front();
        }

        let mut values: Vec<Option<f64>> = vec![None; FEATURE_NAMES.len()];

        let ret_1 = ret_k(&closes, 1);
        let ret_3 = ret_k(&closes, 3);
        let ret_15 = ret_k(&closes, 15);
        let ret_30 = ret_k(&closes, 30);
        let ret_60 = ret_k(&closes, 60);
        let ret_5 = ret5[idx];

        values[0] = ret_1;
        values[1] = ret_3;
        values[2] = ret_5;
        values[3] = ret_15;
        values[4] = ret_30;
        values[5] = ret_60;

        // vol60: population std of the last 60 one-minute log returns, only
        // once that window is fully populated post-reset.
        let vol60 = if rets.len() == VOL_WINDOW {
            Some(population_std(rets.iter().copied()))
        } else {
            None
        };
        let vol_15 = if rets.len() >= 15 {
            Some(population_std(rets.iter().rev().take(15).copied()))
        } else {
            None
        };

        values[6] = mom(ret_5, vol60, 5.0);
        values[7] = mom(ret_15, vol60, 15.0);
        values[8] = mom(ret_30, vol60, 30.0);
        values[9] = mom(ret_60, vol60, 60.0);

        values[10] = vol_15;
        values[11] = vol60;
        values[12] = vol60.map(|v60| vol_15.unwrap_or(0.0) / (v60 + EPS));

        values[13] = if vwap_terms.len() == VOL_WINDOW {
            let sum_vol: f64 = vwap_terms.iter().map(|(_, v)| v).sum();
            if sum_vol < EPS {
                None
            } else {
                let sum_tv: f64 = vwap_terms.iter().map(|(tv, _)| tv).sum();
                let vwap = sum_tv / sum_vol;
                Some((b.close / vwap).ln())
            }
        } else {
            None
        };

        values[14] = if volumes.len() == VOL_WINDOW {
            let mean = mean(volumes.iter().copied());
            let std = population_std(volumes.iter().copied());
            Some((b.volume - mean) / (std + EPS))
        } else {
            None
        };

        values[15] = if volumes.len() == VOL_WINDOW {
            let mean5 = mean(volumes.iter().rev().take(5).copied());
            let mean60 = mean(volumes.iter().copied());
            Some(mean5 / (mean60 + EPS))
        } else {
            None
        };

        values[16] = if trades.len() == VOL_WINDOW && trades.iter().all(|t| t.is_some()) {
            let as_f64: Vec<f64> = trades.iter().map(|t| t.unwrap() as f64).collect();
            let mean = mean(as_f64.iter().copied());
            let std = population_std(as_f64.iter().copied());
            let current = b.trades.unwrap() as f64;
            Some((current - mean) / (std + EPS))
        } else {
            None
        };

        values[17] = match vol60 {
            Some(v) if v >= EPS => Some(((b.high - b.low) / b.close) / (v + EPS)),
            _ => None,
        };

        let hl = b.high - b.low;
        values[18] = Some((b.close - b.open) / (hl + EPS));
        values[19] = Some((b.high - b.open.max(b.close)) / (hl + EPS));
        values[20] = Some((b.open.min(b.close) - b.low) / (hl + EPS));

        let minute_of_day = (b.ts_utc.hour() * 60 + b.ts_utc.minute()) as f64;
        let angle = 2.0 * std::f64::consts::PI * minute_of_day / 1440.0;
        values[21] = Some(angle.sin());
        values[22] = Some(angle.cos());
        values[23] = Some(b.ts_utc.weekday().num_days_from_monday() as f64);

        let btc_ret_5 = btc_ret5_by_ts.get(&b.ts_utc).copied();
        values[24] = btc_ret_5;
        values[25] = match (ret_5, btc_ret_5) {
            (Some(a), Some(bv)) => Some(a - bv),
            _ => None,
        };

        rows.push(FeatureRow {
            ts_utc: b.ts_utc,
            asset: b.asset.clone(),
            values,
        });
    }

    Ok(rows)
}

/// `mom_k = r_k / (vol60 * sqrt(k) + eps)`, `None` while `r_k` or `vol60`
/// are cold, or while `vol60` is degenerate (< eps) — the vol-scaled
/// features are deliberately `None` rather than blown up by the `+eps`
/// denominator guard alone.
fn mom(ret_k: Option<f64>, vol60: Option<f64>, k: f64) -> Option<f64> {
    let r = ret_k?;
    let v = vol60?;
    if v < EPS {
        return None;
    }
    Some(r / (v * k.sqrt() + EPS))
}

/// `r_k(t) = ln(close_t / close_{t-k})`. `closes` holds the trailing
/// window (already includes the current bar as the last element); `None`
/// until `k+1` closes exist since the last reset.
fn ret_k(closes: &VecDeque<f64>, k: usize) -> Option<f64> {
    if closes.len() < k + 1 {
        return None;
    }
    let cur = *closes.back().unwrap();
    let back = closes[closes.len() - 1 - k];
    Some((cur / back).ln())
}

/// r_5 series aligned 1:1 with `bars`, using the same 120s gap-reset rule
/// as the main pass. Shared by the main feature pass (for `ret_5`) and by
/// `compute` to build the BTC ts -> r_5 lookup.
fn r5_series(bars: &[Bar]) -> Vec<Option<f64>> {
    let mut closes: VecDeque<f64> = VecDeque::new();
    let mut prev_ts: Option<DateTime<Utc>> = None;
    let mut out = Vec::with_capacity(bars.len());

    for b in bars {
        let gapped = match prev_ts {
            Some(pts) => (b.ts_utc - pts).num_seconds() > GAP_LIMIT_S,
            None => false,
        };
        if gapped {
            closes.clear();
        }
        prev_ts = Some(b.ts_utc);

        closes.push_back(b.close);
        if closes.len() > 6 {
            closes.pop_front();
        }

        let r5 = if closes.len() == 6 {
            Some((closes[5] / closes[0]).ln())
        } else {
            None
        };
        out.push(r5);
    }

    out
}

fn mean(values: impl Iterator<Item = f64> + Clone) -> f64 {
    let n = values.clone().count() as f64;
    values.sum::<f64>() / n
}

fn population_std(values: impl Iterator<Item = f64> + Clone) -> f64 {
    let m = mean(values.clone());
    let n = values.clone().count() as f64;
    let var = values.map(|v| (v - m).powi(2)).sum::<f64>() / n;
    var.sqrt()
}

fn validate_ascending(bars: &[Bar]) -> Result<(), String> {
    for w in bars.windows(2) {
        if w[1].ts_utc <= w[0].ts_utc {
            return Err(format!(
                "bars must be strictly ascending by ts_utc: {} did not come after {}",
                w[1].ts_utc, w[0].ts_utc
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use features_crypto::Bar;

    fn bar(ts_min: i64, close: f64, volume: f64) -> Bar {
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::minutes(ts_min);
        Bar {
            ts_utc: ts,
            asset: "TEST".into(),
            interval_s: 60,
            open: close * 0.999,
            high: close * 1.001,
            low: close * 0.998,
            close,
            volume,
            quote_volume: Some(close * volume),
            trades: Some(10),
        }
    }

    fn ramp(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| bar(i as i64, 100.0 + i as f64 * 0.01, 5.0 + (i % 7) as f64))
            .collect()
    }

    #[test]
    fn one_row_per_bar_and_cold_windows_are_none_not_zero() {
        let bars = ramp(70);
        let rows = compute(&bars, &bars).unwrap();
        assert_eq!(rows.len(), 70);
        let i = |name: &str| FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
        assert!(rows[0].values[i("ret_1")].is_none(), "no history yet");
        assert!(rows[30].values[i("ret_60")].is_none(), "60m window not warm at t=30");
        assert!(rows[65].values[i("ret_60")].is_some());
        assert!(rows[65].values[i("mom_15")].is_some());
        assert!(rows[65].values[i("vwap_dist_60")].is_some());
        assert!(rows[65].values[i("tod_sin")].is_some());
    }

    #[test]
    fn appending_bars_never_changes_emitted_rows() {
        let long = ramp(200);
        let short: Vec<Bar> = long[..150].to_vec();
        let a = compute(&short, &short).unwrap();
        let b = compute(&long, &long).unwrap();
        for (x, y) in a.iter().zip(b.iter().take(150)) {
            assert_eq!(x.ts_utc, y.ts_utc);
            assert_eq!(x.values, y.values, "append changed history at {}", x.ts_utc);
        }
    }

    #[test]
    fn a_gap_resets_the_windows() {
        let mut bars = ramp(70);
        // 10-minute hole after bar 64
        for b in bars.iter_mut().skip(65) {
            b.ts_utc += Duration::minutes(10);
        }
        let rows = compute(&bars, &bars).unwrap();
        let i = |name: &str| FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
        assert!(rows[64].values[i("ret_1")].is_some(), "warm before the gap");
        assert!(rows[65].values[i("ret_1")].is_none(), "the gap resets state");
        assert!(
            rows[69].values[i("ret_1")].is_none() || rows[69].values[i("ret_60")].is_none()
        );
    }

    #[test]
    fn btc_context_aligns_by_timestamp_and_rel_ret_is_zero_for_btc_itself() {
        let bars = ramp(70);
        let rows = compute(&bars, &bars).unwrap();
        let i = |name: &str| FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
        let last = &rows[69];
        assert_eq!(last.values[i("btc_ret_5")], last.values[i("ret_5")]);
        assert_eq!(last.values[i("rel_ret_5")], Some(0.0));
        // Missing BTC ts -> None
        let mut btc = ramp(70);
        btc.retain(|b| b.ts_utc != bars[69].ts_utc);
        let rows2 = compute(&bars, &btc).unwrap();
        assert!(rows2[69].values[i("btc_ret_5")].is_none());
        assert!(rows2[69].values[i("rel_ret_5")].is_none());
    }

    #[test]
    fn the_catalog_is_the_contract() {
        assert_eq!(FEATURE_NAMES.len(), 26);
        let rows = compute(&ramp(70), &ramp(70)).unwrap();
        assert_eq!(rows[0].values.len(), FEATURE_NAMES.len());
    }
}
