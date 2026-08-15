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
//!
//! fs-2 (`fs-rust-scalper-2`) appended 12 microstructure features (spread,
//! depth imbalance/slope, taker flow, open interest, funding) built from
//! `MicroMinute` - a per-bar-aligned view the CALLER assembles from Binance
//! book/flow/metrics/funding files (see `scalper-data::micro_join`) and
//! passes in as `micro: &[Option<MicroMinute>]`, one slot per bar. This
//! module never touches those files; it only reads whatever `MicroMinute`
//! it is handed. The original 26 fs-1 features are computed by the exact
//! same code path as before (untouched), so a `micro` slice that is all
//! `None` reproduces fs-1's outputs bit-for-bit - the regression test below
//! asserts this. `MicroMinute`'s own fields are each independently
//! `Option<f64>` (a source can be absent even when the caller found SOME
//! micro data for that minute, e.g. funding without book coverage), so the
//! per-field discipline is: any input to a feature being `None` makes that
//! feature `None`, never a fabricated value.
//!
//! fs-3 (`fs-rust-scalper-3`) substitutes three of those 12 features -
//! indices 27/29/37 (`depth_imb_02`, `depth_02_z_60`, `depth_slope`) become
//! `depth_10_z_60`, `depth_10_log`, `depth_imb_10_m15` - because the ±0.2%
//! book-depth band (`bid_02`/`ask_02`) is absent from the ingested Binance
//! archive before 2026-01-15 while the ±1.0% band (`bid_10`/`ask_10`)
//! covers the full history (see `docs/scalper-research.md` Amendment 2).
//! fs-3 never reads `bid_02`/`ask_02`; those `MicroMinute` fields remain
//! for parity with fs-2 but are unused. The other 9 fs-2 features, and all
//! 26 fs-1 features, are unchanged.
//!
//! The four rolling micro features (`depth_10_z_60`, `taker_buy_ratio_m15`,
//! `oi_change_60`, `spread_z_60`) use trailing windows reset by the same
//! 120s bar-gap rule as fs-1's windows, and only ever contribute a value
//! once the window is both full AND has no `None` entries in it - the same
//! `trades_z_60` discipline fs-1 already established for a source that can
//! itself be missing. `depth_imb_10_m15` is a fifth rolling feature with
//! the same full-and-clean discipline, mirroring `taker_buy_ratio_m15`'s
//! 15-minute window instead of the 60-minute one.

use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Datelike, Timelike, Utc};
use features_crypto::Bar;

pub const FEATURE_SET_VERSION: &str = "fs-rust-scalper-3";

pub const FEATURE_NAMES: [&str; 38] = [
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
    "spread_bps",
    "depth_10_z_60",
    "depth_imb_10",
    "depth_10_log",
    "taker_buy_ratio",
    "taker_buy_ratio_m15",
    "oi_change_60",
    "taker_ls_ratio",
    "funding_rate_bps",
    "funding_x_mom",
    "spread_z_60",
    "depth_imb_10_m15",
];

const EPS: f64 = 1e-12;
const GAP_LIMIT_S: i64 = 120;
const VOL_WINDOW: usize = 60;
const CLOSE_WINDOW: usize = VOL_WINDOW + 1;
const TBR_WINDOW: usize = 15;

/// One bar's worth of Binance microstructure, aligned by the caller so that
/// every field reflects only data with timestamp <= this bar's close (the
/// causal snap-downward rule). Assembled from four independent sources
/// (book depth, aggTrades-derived flow, 5m metrics, funding), so each field
/// is independently `Option<f64>` - a source can be stale/absent for this
/// minute even when others are present.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MicroMinute {
    pub ts_s: i64,
    pub spread_bps: Option<f64>,
    pub taker_buy_ratio: Option<f64>,
    pub bid_02: Option<f64>,
    pub ask_02: Option<f64>,
    pub bid_10: Option<f64>,
    pub ask_10: Option<f64>,
    pub oi_value: Option<f64>,
    pub taker_ls_ratio: Option<f64>,
    pub funding_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureRow {
    pub ts_utc: DateTime<Utc>,
    pub asset: String,
    pub values: Vec<Option<f64>>,
}

/// Compute the 38-feature row set for `bars` (one asset's ascending 1m
/// bars), using `btc_bars` (BTC's ascending 1m bars, ascending) as context
/// for `btc_ret_5` / `rel_ret_5`, and `micro` (one slot per bar, same
/// length as `bars`) for the 12 fs-3 microstructure features. Pass the
/// same slice for `bars`/`btc_bars` to compute BTC's own rows. `micro[i]
/// == None` means no micro data was available that minute, so all 12 new
/// features are `None` at that row; the 26 fs-1 features are unaffected.
pub fn compute(
    bars: &[Bar],
    btc_bars: &[Bar],
    micro: &[Option<MicroMinute>],
) -> Result<Vec<FeatureRow>, String> {
    validate_ascending(bars)?;
    validate_ascending(btc_bars)?;
    if micro.len() != bars.len() {
        return Err(format!(
            "micro.len() ({}) must equal bars.len() ({})",
            micro.len(),
            bars.len()
        ));
    }

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
                                                             // fs-2/fs-3 rolling micro state, reset by the same gap rule.
    let mut depth_10_window: VecDeque<Option<f64>> = VecDeque::new(); // up to 60
    let mut spread_window: VecDeque<Option<f64>> = VecDeque::new(); // up to 60
    let mut tbr_window: VecDeque<Option<f64>> = VecDeque::new(); // up to 15
    let mut oi_window: VecDeque<Option<f64>> = VecDeque::new(); // up to 61
    let mut depth_imb_10_window: VecDeque<Option<f64>> = VecDeque::new(); // up to 15
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
            depth_10_window.clear();
            spread_window.clear();
            tbr_window.clear();
            oi_window.clear();
            depth_imb_10_window.clear();
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

        // fs-3 microstructure features (26..37). Every field of
        // `MicroMinute` is read independently: a missing field makes only
        // the feature(s) that need it `None`, never the whole row. fs-3
        // never reads `bid_02`/`ask_02` (Amendment 2) - only the ±1.0% band
        // (`bid_10`/`ask_10`) is used.
        let mi = micro[idx].as_ref();
        let spread_bps = mi.and_then(|m| m.spread_bps);
        let taker_buy_ratio = mi.and_then(|m| m.taker_buy_ratio);
        let bid_10 = mi.and_then(|m| m.bid_10);
        let ask_10 = mi.and_then(|m| m.ask_10);
        let oi_value = mi.and_then(|m| m.oi_value);
        let taker_ls_ratio = mi.and_then(|m| m.taker_ls_ratio);
        let funding_rate = mi.and_then(|m| m.funding_rate);

        let depth_10_sum = match (bid_10, ask_10) {
            (Some(bid), Some(ask)) => Some(bid + ask),
            _ => None,
        };
        depth_10_window.push_back(depth_10_sum);
        if depth_10_window.len() > VOL_WINDOW {
            depth_10_window.pop_front();
        }
        spread_window.push_back(spread_bps);
        if spread_window.len() > VOL_WINDOW {
            spread_window.pop_front();
        }
        tbr_window.push_back(taker_buy_ratio);
        if tbr_window.len() > TBR_WINDOW {
            tbr_window.pop_front();
        }
        oi_window.push_back(oi_value);
        if oi_window.len() > CLOSE_WINDOW {
            oi_window.pop_front();
        }

        let depth_imb_10 = match (bid_10, ask_10) {
            (Some(bid), Some(ask)) => Some((bid - ask) / (bid + ask + EPS)),
            _ => None,
        };
        depth_imb_10_window.push_back(depth_imb_10);
        if depth_imb_10_window.len() > TBR_WINDOW {
            depth_imb_10_window.pop_front();
        }

        values[26] = spread_bps;
        values[27] = rolling_zscore(&depth_10_window, VOL_WINDOW);
        values[28] = depth_imb_10;
        values[29] = depth_10_sum.map(|s| (s + EPS).ln());
        values[30] = taker_buy_ratio;
        values[31] = if tbr_window.len() == TBR_WINDOW && tbr_window.iter().all(Option::is_some) {
            let vals: Vec<f64> = tbr_window.iter().map(|v| v.unwrap()).collect();
            Some(mean(vals.iter().copied()))
        } else {
            None
        };
        values[32] = if oi_window.len() == CLOSE_WINDOW {
            match (oi_window.front().copied().flatten(), oi_value) {
                (Some(old), Some(cur)) => Some((cur / old).ln()),
                _ => None,
            }
        } else {
            None
        };
        values[33] = taker_ls_ratio;
        let funding_rate_bps = funding_rate.map(|r| r * 1e4);
        values[34] = funding_rate_bps;
        values[35] = match (funding_rate_bps, ret_15) {
            (Some(fbps), Some(r15)) => Some(fbps * sign(r15)),
            _ => None,
        };
        values[36] = rolling_zscore(&spread_window, VOL_WINDOW);
        values[37] = if depth_imb_10_window.len() == TBR_WINDOW
            && depth_imb_10_window.iter().all(Option::is_some)
        {
            let vals: Vec<f64> = depth_imb_10_window.iter().map(|v| v.unwrap()).collect();
            Some(mean(vals.iter().copied()))
        } else {
            None
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

/// `sign(x)`: 1.0 / -1.0 / 0.0. Used by `funding_x_mom`'s crowding
/// interaction; an exact-zero `ret_15` is deliberately neutral rather than
/// arbitrarily signed positive.
fn sign(x: f64) -> f64 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// z-score of the LAST entry in `window` against the window's own mean/std,
/// only once `window` holds exactly `required_len` entries AND every one of
/// them is `Some` (a `None` anywhere in the window - including the current
/// minute - means the window is not clean, so the z-score is `None` rather
/// than computed over a gappy series). Mirrors fs-1's `trades_z_60`
/// discipline for a rolling source that can itself be missing.
fn rolling_zscore(window: &VecDeque<Option<f64>>, required_len: usize) -> Option<f64> {
    if window.len() != required_len || window.iter().any(Option::is_none) {
        return None;
    }
    let vals: Vec<f64> = window.iter().map(|v| v.unwrap()).collect();
    let m = mean(vals.iter().copied());
    let std = population_std(vals.iter().copied());
    let cur = *vals.last().unwrap();
    Some((cur - m) / (std + EPS))
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

    fn no_micro(n: usize) -> Vec<Option<MicroMinute>> {
        vec![None; n]
    }

    fn i(name: &str) -> usize {
        FEATURE_NAMES.iter().position(|n| *n == name).unwrap()
    }

    #[test]
    fn one_row_per_bar_and_cold_windows_are_none_not_zero() {
        let bars = ramp(70);
        let rows = compute(&bars, &bars, &no_micro(70)).unwrap();
        assert_eq!(rows.len(), 70);
        assert!(rows[0].values[i("ret_1")].is_none(), "no history yet");
        assert!(
            rows[30].values[i("ret_60")].is_none(),
            "60m window not warm at t=30"
        );
        assert!(rows[65].values[i("ret_60")].is_some());
        assert!(rows[65].values[i("mom_15")].is_some());
        assert!(rows[65].values[i("vwap_dist_60")].is_some());
        assert!(rows[65].values[i("tod_sin")].is_some());
    }

    #[test]
    fn appending_bars_never_changes_emitted_rows() {
        let long = ramp(200);
        let short: Vec<Bar> = long[..150].to_vec();
        let a = compute(&short, &short, &no_micro(150)).unwrap();
        let b = compute(&long, &long, &no_micro(200)).unwrap();
        for (x, y) in a.iter().zip(b.iter().take(150)) {
            assert_eq!(x.ts_utc, y.ts_utc);
            assert_eq!(x.values, y.values, "append changed history at {}", x.ts_utc);
        }
    }

    /// Same append-safety property, but with every fs-2 field populated -
    /// the new rolling windows must not peek ahead either.
    #[test]
    fn appending_bars_never_changes_emitted_rows_with_micro_data() {
        let long = ramp(200);
        let long_micro: Vec<Option<MicroMinute>> = long
            .iter()
            .enumerate()
            .map(|(idx, b)| {
                Some(MicroMinute {
                    ts_s: b.ts_utc.timestamp(),
                    spread_bps: Some(3.0 + (idx % 5) as f64),
                    taker_buy_ratio: Some(0.5 + (idx % 3) as f64 * 0.01),
                    bid_02: Some(100.0 + idx as f64),
                    ask_02: Some(90.0),
                    bid_10: Some(400.0 + idx as f64),
                    ask_10: Some(390.0),
                    oi_value: Some(1000.0 + idx as f64),
                    taker_ls_ratio: Some(1.1),
                    funding_rate: Some(0.00003),
                })
            })
            .collect();
        let short: Vec<Bar> = long[..150].to_vec();
        let short_micro: Vec<Option<MicroMinute>> = long_micro[..150].to_vec();
        let a = compute(&short, &short, &short_micro).unwrap();
        let b = compute(&long, &long, &long_micro).unwrap();
        for (x, y) in a.iter().zip(b.iter().take(150)) {
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
        let rows = compute(&bars, &bars, &no_micro(70)).unwrap();
        assert!(rows[64].values[i("ret_1")].is_some(), "warm before the gap");
        assert!(
            rows[65].values[i("ret_1")].is_none(),
            "the gap resets state"
        );
        assert!(rows[69].values[i("ret_1")].is_none() || rows[69].values[i("ret_60")].is_none());
    }

    #[test]
    fn btc_context_aligns_by_timestamp_and_rel_ret_is_zero_for_btc_itself() {
        let bars = ramp(70);
        let rows = compute(&bars, &bars, &no_micro(70)).unwrap();
        let last = &rows[69];
        assert_eq!(last.values[i("btc_ret_5")], last.values[i("ret_5")]);
        assert_eq!(last.values[i("rel_ret_5")], Some(0.0));
        // Missing BTC ts -> None
        let mut btc = ramp(70);
        btc.retain(|b| b.ts_utc != bars[69].ts_utc);
        let rows2 = compute(&bars, &btc, &no_micro(70)).unwrap();
        assert!(rows2[69].values[i("btc_ret_5")].is_none());
        assert!(rows2[69].values[i("rel_ret_5")].is_none());
    }

    #[test]
    fn the_catalog_is_the_contract() {
        assert_eq!(FEATURE_NAMES.len(), 38);
        assert_eq!(FEATURE_SET_VERSION, "fs-rust-scalper-3");
        assert_eq!(FEATURE_NAMES[27], "depth_10_z_60");
        assert_eq!(FEATURE_NAMES[28], "depth_imb_10");
        assert_eq!(FEATURE_NAMES[29], "depth_10_log");
        assert_eq!(FEATURE_NAMES[37], "depth_imb_10_m15");
        let rows = compute(&ramp(70), &ramp(70), &no_micro(70)).unwrap();
        assert_eq!(rows[0].values.len(), FEATURE_NAMES.len());
    }

    #[test]
    fn micro_length_mismatch_is_rejected() {
        let bars = ramp(10);
        let err = compute(&bars, &bars, &no_micro(9)).unwrap_err();
        assert!(err.contains("micro.len()"), "got: {err}");
    }

    /// The regression fs-2 exists to protect: an all-`None` micro slice
    /// must leave every one of the 26 fs-1 features exactly as fs-1 would
    /// have computed them (proven here by reusing fs-1's own assertions),
    /// and every one of the 12 new features must be `None`.
    #[test]
    fn micro_all_none_reproduces_fs1_and_nones_the_rest() {
        let bars = ramp(70);
        let rows = compute(&bars, &bars, &no_micro(70)).unwrap();

        // Golden values for row 65 of `ramp(70)`, pinned bit-for-bit. These
        // literals ARE fs-1's outputs on this input: `is_some()` alone lets
        // a future refactor silently perturb the shared 26 features (e.g.
        // in the 15th decimal) and still pass. Every deployed fs-1/fs-2
        // model was trained against exactly these numbers for this input -
        // changing them without an intentional feature-set version bump is
        // train/live drift. Recompute deliberately (never "fix the test to
        // match") if the formula genuinely changes.
        assert_eq!(rows[65].values[i("ret_1")], Some(9.935913367004817e-5));
        assert_eq!(rows[65].values[i("ret_60")], Some(0.005979091056058231));
        assert_eq!(rows[65].values[i("mom_15")], Some(2239.1677202124374));
        assert_eq!(
            rows[65].values[i("vwap_dist_60")],
            Some(0.003275639617194944)
        );
        assert_eq!(rows[65].values[i("volume_z_60")], Some(-0.4683630922972444));
        assert_eq!(rows[65].values[i("body_frac")], Some(0.33333333333224513));

        for (idx, row) in rows.iter().enumerate() {
            for name in FEATURE_NAMES.iter().skip(26) {
                assert!(
                    row.values[i(name)].is_none(),
                    "row {idx} feature {name} should be None with no micro data"
                );
            }
        }
    }

    /// fs-3 never reads `bid_02`/`ask_02` (Amendment 2) - `depth_imb_10` is
    /// hand-computed from the ±1.0% band alone.
    #[test]
    fn depth_imb_10_is_hand_computed() {
        let bars = ramp(5);
        let mut micro = no_micro(5);
        micro[4] = Some(MicroMinute {
            ts_s: bars[4].ts_utc.timestamp(),
            bid_10: Some(500.0),
            ask_10: Some(300.0),
            ..Default::default()
        });
        let rows = compute(&bars, &bars, &micro).unwrap();
        let expect_imb10 = (500.0 - 300.0) / (500.0 + 300.0 + EPS);
        assert!((rows[4].values[i("depth_imb_10")].unwrap() - expect_imb10).abs() < 1e-12);
        // No micro data at all that minute -> None, not a fabricated ratio.
        assert!(rows[0].values[i("depth_imb_10")].is_none());
    }

    /// `depth_10_log` = ln(bid_10 + ask_10 + eps), hand-computed; `None` if
    /// either endpoint is missing (never a fabricated log of a partial sum).
    #[test]
    fn depth_10_log_is_the_log_depth_level_and_none_if_either_side_missing() {
        let bars = ramp(5);
        let mut micro = no_micro(5);
        micro[4] = Some(MicroMinute {
            ts_s: bars[4].ts_utc.timestamp(),
            bid_10: Some(500.0),
            ask_10: Some(300.0),
            ..Default::default()
        });
        let rows = compute(&bars, &bars, &micro).unwrap();
        let expect = (500.0_f64 + 300.0 + EPS).ln();
        assert!((rows[4].values[i("depth_10_log")].unwrap() - expect).abs() < 1e-12);

        micro[4] = Some(MicroMinute {
            ts_s: bars[4].ts_utc.timestamp(),
            bid_10: Some(500.0),
            ask_10: None,
            ..Default::default()
        });
        let rows2 = compute(&bars, &bars, &micro).unwrap();
        assert!(rows2[4].values[i("depth_10_log")].is_none());
    }

    /// `depth_imb_10_m15` mirrors `taker_buy_ratio_m15`'s window mechanics
    /// exactly: a trailing 15-minute mean of `depth_imb_10`, `None` until
    /// all 15 are `Some`, and reset by the same 120s gap rule as every
    /// other rolling window.
    #[test]
    fn depth_imb_10_m15_is_the_trailing_15_minute_mean_and_resets_on_gap() {
        let mut bars = ramp(40);
        for b in bars.iter_mut().skip(21) {
            b.ts_utc += Duration::minutes(10);
        }
        let micro: Vec<Option<MicroMinute>> = bars
            .iter()
            .enumerate()
            .map(|(idx, b)| {
                Some(MicroMinute {
                    ts_s: b.ts_utc.timestamp(),
                    bid_10: Some(500.0 + idx as f64),
                    ask_10: Some(300.0),
                    ..Default::default()
                })
            })
            .collect();
        let rows = compute(&bars, &bars, &micro).unwrap();
        assert!(
            rows[13].values[i("depth_imb_10_m15")].is_none(),
            "only 14 minutes of history"
        );
        let expect: f64 = (5..20)
            .map(|idx| {
                let bid = 500.0 + idx as f64;
                let ask = 300.0;
                (bid - ask) / (bid + ask + EPS)
            })
            .sum::<f64>()
            / 15.0;
        let got = rows[19].values[i("depth_imb_10_m15")].unwrap();
        assert!((got - expect).abs() < 1e-9);

        assert!(
            rows[21].values[i("depth_imb_10_m15")].is_none(),
            "the gap resets the window"
        );

        // A missing entry inside the trailing 15 breaks the all-Some
        // requirement rather than averaging over a gap.
        let mut micro2 = micro.clone();
        micro2[10] = None;
        let rows2 = compute(&bars, &bars, &micro2).unwrap();
        assert!(rows2[19].values[i("depth_imb_10_m15")].is_none());
    }

    #[test]
    fn funding_and_taker_fields_pass_through_and_funding_x_mom_uses_the_sign_of_ret_15() {
        let bars = ramp(20); // monotonically increasing -> ret_15 > 0 by t=19
        let mut micro = no_micro(20);
        micro[19] = Some(MicroMinute {
            ts_s: bars[19].ts_utc.timestamp(),
            funding_rate: Some(0.0001),
            taker_buy_ratio: Some(0.62),
            taker_ls_ratio: Some(1.4),
            ..Default::default()
        });
        let rows = compute(&bars, &bars, &micro).unwrap();
        assert_eq!(rows[19].values[i("funding_rate_bps")], Some(0.0001 * 1e4));
        assert_eq!(rows[19].values[i("taker_buy_ratio")], Some(0.62));
        assert_eq!(rows[19].values[i("taker_ls_ratio")], Some(1.4));
        let ret15 = rows[19].values[i("ret_15")].unwrap();
        assert!(ret15 > 0.0, "ramp is monotonic increasing");
        let expect = 0.0001 * 1e4; // funding_rate_bps * sign(+1)
        assert!((rows[19].values[i("funding_x_mom")].unwrap() - expect).abs() < 1e-9);
    }

    #[test]
    fn oi_change_60_compares_against_the_bar_exactly_60_minutes_earlier() {
        let bars = ramp(65);
        let micro: Vec<Option<MicroMinute>> = bars
            .iter()
            .enumerate()
            .map(|(idx, b)| {
                Some(MicroMinute {
                    ts_s: b.ts_utc.timestamp(),
                    oi_value: Some(1000.0 + idx as f64 * 10.0),
                    ..Default::default()
                })
            })
            .collect();
        let rows = compute(&bars, &bars, &micro).unwrap();
        assert!(
            rows[58].values[i("oi_change_60")].is_none(),
            "not yet 61 minutes of history since the last reset"
        );
        let expect = ((1000.0 + 60.0 * 10.0) / 1000.0_f64).ln();
        let got = rows[60].values[i("oi_change_60")].unwrap();
        assert!((got - expect).abs() < 1e-12);

        // A missing endpoint breaks the feature rather than fabricating a
        // ratio against a stale substitute.
        let mut micro2 = micro.clone();
        micro2[0] = None;
        let rows2 = compute(&bars, &bars, &micro2).unwrap();
        assert!(rows2[60].values[i("oi_change_60")].is_none());
    }

    #[test]
    fn taker_buy_ratio_m15_is_the_trailing_15_minute_mean() {
        let bars = ramp(20);
        let micro: Vec<Option<MicroMinute>> = bars
            .iter()
            .enumerate()
            .map(|(idx, b)| {
                Some(MicroMinute {
                    ts_s: b.ts_utc.timestamp(),
                    taker_buy_ratio: Some(0.5 + idx as f64 * 0.01),
                    ..Default::default()
                })
            })
            .collect();
        let rows = compute(&bars, &bars, &micro).unwrap();
        assert!(
            rows[13].values[i("taker_buy_ratio_m15")].is_none(),
            "only 14 minutes of history"
        );
        let expect: f64 = (5..20).map(|idx| 0.5 + idx as f64 * 0.01).sum::<f64>() / 15.0;
        let got = rows[19].values[i("taker_buy_ratio_m15")].unwrap();
        assert!((got - expect).abs() < 1e-12);
    }

    /// `depth_10_z_60` needs a full, gap-free 60-minute window: warm right
    /// before the gap, `None` immediately after (the window was cleared),
    /// still cold at 59 clean minutes post-reset, and warm again only once
    /// 60 fresh clean minutes have accumulated. `spread_z_60` is checked at
    /// the same points since it follows the identical rolling-window
    /// discipline.
    #[test]
    fn depth_and_spread_z_scores_require_a_clean_full_window_and_reset_on_a_gap() {
        let mut bars = ramp(130);
        for b in bars.iter_mut().skip(65) {
            b.ts_utc += Duration::minutes(10);
        }
        let micro: Vec<Option<MicroMinute>> = bars
            .iter()
            .enumerate()
            .map(|(idx, b)| {
                Some(MicroMinute {
                    ts_s: b.ts_utc.timestamp(),
                    bid_10: Some(100.0 + idx as f64),
                    ask_10: Some(90.0),
                    spread_bps: Some(5.0 + (idx % 3) as f64),
                    ..Default::default()
                })
            })
            .collect();
        let rows = compute(&bars, &bars, &micro).unwrap();
        assert!(
            rows[64].values[i("depth_10_z_60")].is_some(),
            "60-wide clean window before the gap"
        );
        assert!(rows[64].values[i("spread_z_60")].is_some());
        assert!(
            rows[65].values[i("depth_10_z_60")].is_none(),
            "the gap resets the window"
        );
        assert!(rows[65].values[i("spread_z_60")].is_none());
        assert!(
            rows[123].values[i("depth_10_z_60")].is_none(),
            "only 59 clean minutes since the reset"
        );
        assert!(
            rows[124].values[i("depth_10_z_60")].is_some(),
            "60 clean minutes accumulated post-reset"
        );
        assert!(rows[124].values[i("spread_z_60")].is_some());
    }
}
