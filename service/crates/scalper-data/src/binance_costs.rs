//! `scalper-data binance-costs`: turn Binance UM's book/flow microstructure
//! (Task 1's `binance_micro` module output, joined here through the
//! universe file exactly as `training-matrix` does) into a per-asset,
//! per-day cost estimate the gate can charge each trade against instead of
//! one flat plan-2 number.
//!
//! Two numbers per (asset, day): `spread_bps_p75`, the day's p75 minute
//! spread estimate (flow files - conservative over the median, since a
//! backtest charging the typical spread when the tail is what actually
//! fills is optimistic), and `impact_bps`, a pre-registered walk-cost model.
//!
//! Since Amendment 3b, the primary impact model is priced off the day's
//! median ±1.0% band depth (book files) - the band the archive has on every
//! day, not just post-2026-01-15 - `None` when that depth couldn't have
//! absorbed `--notional` at all (a thin-book day, checked at ±1.0% only:
//! `impact_bps_10`). On days where the ±0.2% band also exists, the older
//! Amendment-1 model (`impact_bps_02`) is computed too and the day's
//! `impact_bps` is the max of the two - post-2026-01-15 costs are never
//! lower than the pre-Amendment-3b run charged. Which model produced the
//! final number is recorded in `DayCost::impact_model`.
//!
//! Identity: these files are keyed by the Binance SYMBOL
//! (`data/binance-micro/{book,flow}/{SYMBOL}/{day}.jsonl}`), but the output
//! this command writes is keyed by the HL coin (the universe file's
//! `coin`), matching `matrix::MatrixRow::asset` and the gate's `Pred::asset`
//! - so the gate never has to know Binance symbols exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::binance_micro::{self, BookMinute, FlowMinute};
use crate::costs::percentile;
use crate::universe::Candidate;
use crate::{get, need, tmp_sibling};

/// Same default dollar clip size as the plan-2 cost summary's `--notional`.
pub const DEFAULT_NOTIONAL: f64 = 5000.0;

/// One asset's one day of Binance cost inputs. `samples` is the number of
/// minute spread estimates `spread_bps_p75` was computed over - purely
/// informational (the gate does not gate on it), kept so a suspiciously
/// thin day is visible in the output rather than indistinguishable from a
/// well-sampled one. `impact_model` records which model produced
/// `impact_bps`: `"b10"` (the ±1.0%-band model stood on its own) or
/// `"b10_floored_b02"` (the ±0.2%-band model was strictly higher and won
/// the floor). `#[serde(default)]` so cost files written before Amendment
/// 3b (no `impact_model` key at all) still deserialize.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayCost {
    pub spread_bps_p75: f64,
    pub impact_bps: Option<f64>,
    pub samples: u32,
    #[serde(default = "default_impact_model")]
    pub impact_model: String,
}

fn default_impact_model() -> String {
    "b02".to_string()
}

/// Amendment 3b's impact multiplier for the ±1.0%-band model. Measured, not
/// tuned: on the 5,010 asset-days from 2026-01-15 where both the ±1.0% and
/// ±0.2% bands exist, the ratio of the ±1%-band model at m = 1 to the
/// pre-registered ±0.2%-band model has p10/p25/p50/p75/p90 = 0.36 / 0.44 /
/// 0.56 / 0.68 / 0.76 (per-asset medians 0.37 AAVE, KAITO ... 0.74 BTC).
/// m = 2.3 is the smallest value at which the new model is at least as
/// conservative as the old on >= 75% of asset-days; the max-floor against
/// `impact_bps_02` covers the remainder where the ±0.2% band exists and the
/// old model was higher. One m is run; no scan.
pub(crate) const IMPACT_M_10: f64 = 2.3;

/// Amendment 3b's primary impact model: a uniform-density walk within the
/// ±1.0% (100bps total width one side) band, halved for average depth,
/// times `IMPACT_M_10` - `IMPACT_M_10 * (notional / min(bid_10, ask_10)) *
/// 50.0`. `None` (book too thin to absorb the clip at all, or the band
/// absent) when `notional` exceeds the shallower side's depth, or when
/// either side has no (positive) depth to walk.
pub(crate) fn impact_bps_10(notional: f64, bid_10: f64, ask_10: f64) -> Option<f64> {
    let depth = bid_10.min(ask_10);
    if depth <= 0.0 || notional > depth {
        return None;
    }
    Some(IMPACT_M_10 * (notional / depth) * 50.0)
}

/// Amendment 1's original impact model, kept as the floor per Amendment 3b:
/// a uniform-density walk within the ±0.2% (20bps total width one side)
/// band, halved for average depth, times a 1.5x safety multiplier - `1.5 *
/// (notional / min(bid_02, ask_02)) * 10.0`. `None` (book too thin to
/// absorb the clip at all) when `notional` exceeds the shallower side's
/// depth, or when either side has no (positive) depth to walk.
pub(crate) fn impact_bps_02(notional: f64, bid_02: f64, ask_02: f64) -> Option<f64> {
    let depth = bid_02.min(ask_02);
    if depth <= 0.0 || notional > depth {
        return None;
    }
    Some(1.5 * (notional / depth) * 10.0)
}

/// One day's `DayCost` from that day's already-loaded book/flow minute
/// rows, or `None` when the day has no spread samples at all - nothing to
/// report, so the day is simply absent from the output. That absence
/// matters downstream: `gate::resolve_round_trip` treats an ABSENT day
/// (this function returned `None`, so no entry ever reaches the output
/// JSON) as the only case eligible for its prior-day fallback. A day that
/// DOES appear below with `impact_bps: None` is not eligible for that
/// fallback at all - the entry exists, so the gate takes it as the final
/// word on that day, untradeable, no substitution from a calmer day.
///
/// `impact_bps` is priced off the day's MEDIAN band notional on each side,
/// not the mean - a single unusually deep or thin snapshot shouldn't
/// dominate a whole day's depth estimate. Since Amendment 3b, the ±1.0%
/// band is the model of record (`impact_bps_10`): thin-at-±1% collapses to
/// `impact_bps: None` regardless of what the ±0.2% band says, deliberately
/// and conservatively - no book coverage AT ALL that day (the book file is
/// missing or empty, `bid_10`/`ask_10` end up with no samples), and book
/// coverage that WAS present but too thin to absorb `--notional` (see
/// `impact_bps_10` above) both mean the same thing to a trader - "there is
/// no evidence this book could have absorbed the clip today" - so neither
/// should be distinguishable from the other, and neither should be able to
/// borrow a neighboring day's number. When the ±1.0% model prices a walk
/// AND the ±0.2% band is also present that day, `impact_bps_02` is computed
/// too and the higher of the two wins (`impact_model` records which).
pub(crate) fn day_cost(book: &[BookMinute], flow: &[FlowMinute], notional: f64) -> Option<DayCost> {
    let spreads: Vec<f64> = flow.iter().filter_map(|m| m.spread_bps_med).collect();
    if spreads.is_empty() {
        return None;
    }
    let spread_bps_p75 = percentile(&spreads, 0.75);
    let samples = spreads.len() as u32;

    let bid_10: Vec<f64> = book
        .iter()
        .filter_map(|m| m.bands.get("-1.0").copied())
        .collect();
    let ask_10: Vec<f64> = book
        .iter()
        .filter_map(|m| m.bands.get("1.0").copied())
        .collect();
    let impact_10 = if bid_10.is_empty() || ask_10.is_empty() {
        None
    } else {
        let median_bid = percentile(&bid_10, 0.5);
        let median_ask = percentile(&ask_10, 0.5);
        impact_bps_10(notional, median_bid, median_ask)
    };

    let bid_02: Vec<f64> = book
        .iter()
        .filter_map(|m| m.bands.get("-0.2").copied())
        .collect();
    let ask_02: Vec<f64> = book
        .iter()
        .filter_map(|m| m.bands.get("0.2").copied())
        .collect();
    let impact_02 = if bid_02.is_empty() || ask_02.is_empty() {
        None
    } else {
        let median_bid = percentile(&bid_02, 0.5);
        let median_ask = percentile(&ask_02, 0.5);
        impact_bps_02(notional, median_bid, median_ask)
    };

    let (impact, impact_model) = match impact_10 {
        None => (None, "b10".to_string()),
        Some(v10) => match impact_02 {
            Some(v02) if v02 > v10 => (Some(v02), "b10_floored_b02".to_string()),
            _ => (Some(v10), "b10".to_string()),
        },
    };

    Some(DayCost {
        spread_bps_p75,
        impact_bps: impact,
        samples,
        impact_model,
    })
}

/// One JSONL file's rows, or an empty `Vec` if the file does not exist - a
/// missing day/symbol file is "no coverage that day", not an error, same
/// discipline as the rest of the micro pipeline (`micro_join::read_jsonl`).
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", path.display())))
        .collect()
}

pub fn cmd_binance_costs(args: &[String]) -> Result<(), String> {
    let data_root = PathBuf::from(need(args, "--data-root")?);
    let universe_path = PathBuf::from(need(args, "--universe")?);
    let start = crate::parse_date(&need(args, "--start")?)?;
    let end = crate::parse_date(&need(args, "--end")?)?;
    if start >= end {
        return Err("--start must precede --end".into());
    }
    let notional: f64 = match get(args, "--notional") {
        Some(v) => v.parse().map_err(|e| format!("bad --notional: {e}"))?,
        None => DEFAULT_NOTIONAL,
    };
    let out_path = PathBuf::from(need(args, "--out")?);

    let text = std::fs::read_to_string(&universe_path)
        .map_err(|e| format!("{}: {e}", universe_path.display()))?;
    let candidates: Vec<Candidate> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", universe_path.display()))?;

    let micro_root = data_root.join("binance-micro");
    let days = binance_micro::days(start, end);

    let mut out: BTreeMap<String, BTreeMap<String, DayCost>> = BTreeMap::new();
    for candidate in &candidates {
        let Some(symbol) = &candidate.binance_um else {
            println!("{}: skipped (not listed on Binance UM)", candidate.coin);
            continue;
        };

        let mut per_day: BTreeMap<String, DayCost> = BTreeMap::new();
        for day in &days {
            let book: Vec<BookMinute> = read_jsonl(
                &micro_root
                    .join("book")
                    .join(symbol)
                    .join(format!("{day}.jsonl")),
            )?;
            let flow: Vec<FlowMinute> = read_jsonl(
                &micro_root
                    .join("flow")
                    .join(symbol)
                    .join(format!("{day}.jsonl")),
            )?;
            if let Some(dc) = day_cost(&book, &flow, notional) {
                per_day.insert(day.to_string(), dc);
            }
        }
        println!(
            "{} ({symbol}): {} day(s) of cost data",
            candidate.coin,
            per_day.len()
        );
        if !per_day.is_empty() {
            out.insert(candidate.coin.clone(), per_day);
        }
    }

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let json = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    let tmp_path = tmp_sibling(&out_path);
    std::fs::write(&tmp_path, json).map_err(|e| format!("{}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;
    println!(
        "wrote costs for {} asset(s) to {}",
        out.len(),
        out_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow_minute(ts_s: i64, spread: Option<f64>) -> FlowMinute {
        FlowMinute {
            ts_s,
            spread_bps_med: spread,
            n_spread_samples: 10,
            distinct_bids: 5,
            taker_buy_ratio: 0.5,
            n_trades: 100,
            notional: 10_000.0,
        }
    }

    /// A book minute with only the ±1.0% band - the band the archive has
    /// on every day, including pre-2026-01-15.
    fn book_minute_10(ts_s: i64, bid10: f64, ask10: f64) -> BookMinute {
        let mut bands = std::collections::BTreeMap::new();
        bands.insert("-1.0".to_string(), bid10);
        bands.insert("1.0".to_string(), ask10);
        BookMinute { ts_s, bands }
    }

    /// A book minute with both bands present.
    fn book_minute_both(ts_s: i64, bid10: f64, ask10: f64, bid02: f64, ask02: f64) -> BookMinute {
        let mut bands = std::collections::BTreeMap::new();
        bands.insert("-1.0".to_string(), bid10);
        bands.insert("1.0".to_string(), ask10);
        bands.insert("-0.2".to_string(), bid02);
        bands.insert("0.2".to_string(), ask02);
        BookMinute { ts_s, bands }
    }

    // -- impact_bps_02 (Amendment 1, unchanged logic, renamed) --------------

    #[test]
    fn impact_bps_02_is_a_uniform_density_walk_with_the_1_5_safety_multiplier() {
        // depth = min(2000, 3000) = 2000; ratio = 1000/2000 = 0.5;
        // impact = 1.5 * 0.5 * 10.0 = 7.5.
        let got = impact_bps_02(1_000.0, 2_000.0, 3_000.0).unwrap();
        assert!((got - 7.5).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn impact_bps_02_prices_off_the_shallower_side() {
        // bid deeper than ask: depth = min(5000, 1000) = 1000; ratio = 0.5.
        let got = impact_bps_02(500.0, 5_000.0, 1_000.0).unwrap();
        assert!((got - 7.5).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn impact_bps_02_is_none_when_notional_exceeds_the_shallow_side() {
        assert!(impact_bps_02(5_000.0, 2_000.0, 3_000.0).is_none());
    }

    #[test]
    fn impact_bps_02_at_exactly_the_depth_is_not_thin() {
        // notional == depth is not "> depth", so this still prices a walk
        // (the full 20bps band, halved, times the safety multiplier).
        let got = impact_bps_02(2_000.0, 2_000.0, 3_000.0).unwrap();
        assert!((got - 15.0).abs() < 1e-9, "got {got}"); // 1.5 * 1.0 * 10.0
    }

    #[test]
    fn impact_bps_02_is_none_when_depth_is_zero_or_negative() {
        assert!(impact_bps_02(100.0, 0.0, 3_000.0).is_none());
        assert!(impact_bps_02(100.0, -5.0, 3_000.0).is_none());
    }

    // -- impact_bps_10 (Amendment 3b) ----------------------------------------

    #[test]
    fn impact_bps_10_is_the_2_3x_multiplier_walk() {
        // depth = min(1_000_000, 1_000_000) = 1_000_000;
        // ratio = 5000/1_000_000 = 0.005; impact = 2.3 * 0.005 * 50 = 0.575.
        let got = impact_bps_10(5_000.0, 1_000_000.0, 1_000_000.0).unwrap();
        assert!((got - 0.575).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn impact_bps_10_prices_off_the_shallower_side() {
        // depth = min(2_000_000, 500_000) = 500_000; ratio = 5000/500_000 = 0.01;
        // impact = 2.3 * 0.01 * 50 = 1.15.
        let got = impact_bps_10(5_000.0, 2_000_000.0, 500_000.0).unwrap();
        assert!((got - 1.15).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn impact_bps_10_is_none_when_notional_exceeds_the_shallow_side() {
        assert!(impact_bps_10(5_000.0, 2_000.0, 3_000.0).is_none());
    }

    #[test]
    fn impact_bps_10_at_exactly_the_depth_is_not_thin() {
        let got = impact_bps_10(2_000.0, 2_000.0, 3_000.0).unwrap();
        assert!((got - 115.0).abs() < 1e-9, "got {got}"); // 2.3 * 1.0 * 50
    }

    #[test]
    fn impact_bps_10_is_none_when_depth_is_zero_or_negative() {
        assert!(impact_bps_10(100.0, 0.0, 3_000.0).is_none());
        assert!(impact_bps_10(100.0, -5.0, 3_000.0).is_none());
    }

    // -- day_cost -------------------------------------------------------------

    #[test]
    fn day_cost_p75_spreads_and_medians_the_1pct_band_depth() {
        let flow = vec![
            flow_minute(0, Some(5.0)),
            flow_minute(60, Some(10.0)),
            flow_minute(120, Some(15.0)),
            flow_minute(180, Some(20.0)),
        ];
        let book = vec![
            book_minute_10(0, 2_000_000.0, 3_000_000.0),
            book_minute_10(60, 2_000_000.0, 3_000_000.0),
        ];
        let dc = day_cost(&book, &flow, 1_000.0).unwrap();
        // p75 of [5,10,15,20] (linear interpolation): rank = 0.75*3 = 2.25
        // -> between sorted[2]=15 and sorted[3]=20 -> 15 + 0.25*5 = 16.25.
        assert!(
            (dc.spread_bps_p75 - 16.25).abs() < 1e-9,
            "got {}",
            dc.spread_bps_p75
        );
        assert_eq!(dc.samples, 4);
        // median bid=2_000_000, ask=3_000_000 -> depth=2_000_000;
        // impact = 2.3*(1000/2_000_000)*50 = 0.0575.
        assert!((dc.impact_bps.unwrap() - 0.0575).abs() < 1e-9);
        assert_eq!(dc.impact_model, "b10");
    }

    #[test]
    fn day_cost_is_none_with_no_spread_samples_that_day() {
        let flow = vec![flow_minute(0, None), flow_minute(60, None)];
        let book = vec![book_minute_10(0, 2_000_000.0, 3_000_000.0)];
        assert!(day_cost(&book, &flow, 1_000.0).is_none());
    }

    #[test]
    fn day_cost_reports_a_missing_impact_separately_from_a_missing_spread() {
        // Flow data exists (spread is computable) but no book coverage that
        // day - impact_bps must be None (no depth to price), not a
        // fabricated number, while spread_bps_p75 is still reported.
        let flow = vec![flow_minute(0, Some(5.0))];
        let dc = day_cost(&[], &flow, 1_000.0).unwrap();
        assert!(dc.impact_bps.is_none());
        assert_eq!(dc.samples, 1);
        assert!((dc.spread_bps_p75 - 5.0).abs() < 1e-9);
    }

    #[test]
    fn day_cost_carries_the_thin_1pct_book_none_through_from_impact_bps_10() {
        let flow = vec![flow_minute(0, Some(5.0))];
        let book = vec![book_minute_10(0, 100.0, 100.0)]; // depth 100, way under notional
        let dc = day_cost(&book, &flow, 10_000.0).unwrap();
        assert!(
            dc.impact_bps.is_none(),
            "thin ±1% book must surface as None, not a bogus number"
        );
    }

    #[test]
    fn day_cost_is_none_regardless_when_1pct_band_is_thin_even_if_02pct_band_is_healthy() {
        // ±1% band: depth 100, notional 10_000 -> thin, None.
        // ±0.2% band: depth 1_000_000, notional 10_000 -> would price fine
        // under the old model alone (ratio 0.01, impact = 1.5*0.01*10=0.15).
        // Per Amendment 3b, thin-at-±1% means None regardless of ±0.2%.
        let flow = vec![flow_minute(0, Some(5.0))];
        let book = vec![book_minute_both(0, 100.0, 100.0, 1_000_000.0, 1_000_000.0)];
        let dc = day_cost(&book, &flow, 10_000.0).unwrap();
        assert!(
            dc.impact_bps.is_none(),
            "±1% thinness must not be rescued by a healthy ±0.2% band"
        );
    }

    #[test]
    fn day_cost_floors_the_new_impact_with_the_old_when_the_old_is_higher() {
        // notional = 9.
        // ±1.0% band: depth 1150 -> impact_10 = 2.3*(9/1150)*50 = 0.9.
        // ±0.2% band: depth 112.5 -> impact_02 = 1.5*(9/112.5)*10 = 1.2.
        // 1.2 > 0.9, so the floor wins: impact_bps = 1.2, model floored.
        let flow = vec![flow_minute(0, Some(5.0))];
        let book = vec![book_minute_both(0, 1_150.0, 1_150.0, 112.5, 112.5)];
        let dc = day_cost(&book, &flow, 9.0).unwrap();
        assert!(
            (dc.impact_bps.unwrap() - 1.2).abs() < 1e-9,
            "got {:?}",
            dc.impact_bps
        );
        assert_eq!(dc.impact_model, "b10_floored_b02");
    }

    #[test]
    fn day_cost_uses_the_new_impact_when_no_02pct_band_is_present() {
        // Healthy ±1% band, no ±0.2% band at all -> new model wins outright.
        let flow = vec![flow_minute(0, Some(5.0))];
        let book = vec![book_minute_10(0, 1_000_000.0, 1_000_000.0)];
        let dc = day_cost(&book, &flow, 5_000.0).unwrap();
        assert!(
            (dc.impact_bps.unwrap() - 0.575).abs() < 1e-9,
            "got {:?}",
            dc.impact_bps
        );
        assert_eq!(dc.impact_model, "b10");
    }

    #[test]
    fn day_cost_pre_2026_only_1pct_band_is_now_costable_where_it_used_to_be_none() {
        // Simulates a pre-2026-01-15 archive day: only the ±1.0% band ever
        // exists (Amendment 2's finding), never the ±0.2% band. Under
        // Amendment 1's ±0.2%-band-only model this day would have had
        // impact_bps: None (bid_02/ask_02 empty, unconditionally) -
        // untradeable by rule no matter how deep the book actually was.
        // Amendment 3b's whole point: such a day is now COSTABLE.
        let flow = vec![flow_minute(0, Some(8.0))];
        let book = vec![book_minute_10(0, 1_000_000.0, 1_000_000.0)];

        // Confirm the test setup actually has no ±0.2% band (old model's
        // input would have been empty).
        assert!(book[0].bands.get("-0.2").is_none());
        assert!(book[0].bands.get("0.2").is_none());

        let dc = day_cost(&book, &flow, 5_000.0).unwrap();
        assert!(
            dc.impact_bps.is_some(),
            "±1% band alone must make the day costable under Amendment 3b"
        );
        assert_eq!(dc.impact_model, "b10");
    }
}
