//! `scalper-data binance-costs`: turn Binance UM's book/flow microstructure
//! (Task 1's `binance_micro` module output, joined here through the
//! universe file exactly as `training-matrix` does) into a per-asset,
//! per-day cost estimate the gate can charge each trade against instead of
//! one flat plan-2 number.
//!
//! Two numbers per (asset, day): `spread_bps_p75`, the day's p75 minute
//! spread estimate (flow files - conservative over the median, since a
//! backtest charging the typical spread when the tail is what actually
//! fills is optimistic), and `impact_bps`, a pre-registered walk-cost model
//! over the day's median ±0.2% band depth (book files) - `None` when that
//! depth couldn't have absorbed `--notional` at all (a thin-book day).
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
/// well-sampled one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayCost {
    pub spread_bps_p75: f64,
    pub impact_bps: Option<f64>,
    pub samples: u32,
}

/// Pre-registered impact model: a uniform-density walk within the ±0.2%
/// (20bps total width one side) band, halved for average depth, times a
/// 1.5x safety multiplier - `1.5 * (notional / min(bid_02, ask_02)) *
/// 10.0`. `None` (book too thin to absorb the clip at all) when `notional`
/// exceeds the shallower side's depth, or when either side has no
/// (positive) depth to walk.
pub(crate) fn impact_bps(notional: f64, bid_02: f64, ask_02: f64) -> Option<f64> {
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
/// `impact_bps` is priced off the day's MEDIAN ±0.2% band notional on each
/// side, not the mean - a single unusually deep or thin snapshot shouldn't
/// dominate a whole day's depth estimate. Two different situations both
/// collapse into that same `impact_bps: None`, deliberately and
/// conservatively: no book coverage AT ALL that day (the book file is
/// missing or empty, `bid_02`/`ask_02` end up with no samples), and book
/// coverage that WAS present but too thin to absorb `--notional` (see
/// `impact_bps` above). Both mean the same thing to a trader - "there is no
/// evidence this book could have absorbed the clip today" - so neither
/// should be distinguishable from the other, and neither should be able to
/// borrow a neighboring day's number.
pub(crate) fn day_cost(book: &[BookMinute], flow: &[FlowMinute], notional: f64) -> Option<DayCost> {
    let spreads: Vec<f64> = flow.iter().filter_map(|m| m.spread_bps_med).collect();
    if spreads.is_empty() {
        return None;
    }
    let spread_bps_p75 = percentile(&spreads, 0.75);
    let samples = spreads.len() as u32;

    let bid_02: Vec<f64> = book
        .iter()
        .filter_map(|m| m.bands.get("-0.2").copied())
        .collect();
    let ask_02: Vec<f64> = book
        .iter()
        .filter_map(|m| m.bands.get("0.2").copied())
        .collect();
    let impact = if bid_02.is_empty() || ask_02.is_empty() {
        None
    } else {
        let median_bid = percentile(&bid_02, 0.5);
        let median_ask = percentile(&ask_02, 0.5);
        impact_bps(notional, median_bid, median_ask)
    };

    Some(DayCost {
        spread_bps_p75,
        impact_bps: impact,
        samples,
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

    fn book_minute(ts_s: i64, bid02: f64, ask02: f64) -> BookMinute {
        let mut bands = std::collections::BTreeMap::new();
        bands.insert("-0.2".to_string(), bid02);
        bands.insert("0.2".to_string(), ask02);
        BookMinute { ts_s, bands }
    }

    // -- impact_bps -------------------------------------------------------

    #[test]
    fn impact_bps_is_a_uniform_density_walk_with_the_1_5_safety_multiplier() {
        // depth = min(2000, 3000) = 2000; ratio = 1000/2000 = 0.5;
        // impact = 1.5 * 0.5 * 10.0 = 7.5.
        let got = impact_bps(1_000.0, 2_000.0, 3_000.0).unwrap();
        assert!((got - 7.5).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn impact_bps_prices_off_the_shallower_side() {
        // bid deeper than ask: depth = min(5000, 1000) = 1000; ratio = 0.5.
        let got = impact_bps(500.0, 5_000.0, 1_000.0).unwrap();
        assert!((got - 7.5).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn impact_bps_is_none_when_notional_exceeds_the_shallow_side() {
        assert!(impact_bps(5_000.0, 2_000.0, 3_000.0).is_none());
    }

    #[test]
    fn impact_bps_at_exactly_the_depth_is_not_thin() {
        // notional == depth is not "> depth", so this still prices a walk
        // (the full 20bps band, halved, times the safety multiplier).
        let got = impact_bps(2_000.0, 2_000.0, 3_000.0).unwrap();
        assert!((got - 15.0).abs() < 1e-9, "got {got}"); // 1.5 * 1.0 * 10.0
    }

    #[test]
    fn impact_bps_is_none_when_depth_is_zero_or_negative() {
        assert!(impact_bps(100.0, 0.0, 3_000.0).is_none());
        assert!(impact_bps(100.0, -5.0, 3_000.0).is_none());
    }

    // -- day_cost -----------------------------------------------------------

    #[test]
    fn day_cost_p75_spreads_and_medians_the_band_depth() {
        let flow = vec![
            flow_minute(0, Some(5.0)),
            flow_minute(60, Some(10.0)),
            flow_minute(120, Some(15.0)),
            flow_minute(180, Some(20.0)),
        ];
        let book = vec![
            book_minute(0, 2_000.0, 3_000.0),
            book_minute(60, 2_000.0, 3_000.0),
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
        // median bid=2000, ask=3000 -> impact = 1.5*(1000/2000)*10 = 7.5.
        assert!((dc.impact_bps.unwrap() - 7.5).abs() < 1e-9);
    }

    #[test]
    fn day_cost_is_none_with_no_spread_samples_that_day() {
        let flow = vec![flow_minute(0, None), flow_minute(60, None)];
        let book = vec![book_minute(0, 2_000.0, 3_000.0)];
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
    fn day_cost_carries_the_thin_book_none_through_from_impact_bps() {
        let flow = vec![flow_minute(0, Some(5.0))];
        let book = vec![book_minute(0, 100.0, 100.0)]; // depth 100, way under notional
        let dc = day_cost(&book, &flow, 10_000.0).unwrap();
        assert!(
            dc.impact_bps.is_none(),
            "thin book must surface as None, not a bogus number"
        );
    }
}
