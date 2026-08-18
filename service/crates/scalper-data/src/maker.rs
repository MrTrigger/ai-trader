//! Maker entry - `docs/scalper-research.md` Amendment 5, §5.3/§5.4 (with
//! the 2026-08-18 fill-bar clarification). `gate --entry maker`.
//!
//! The signal and threshold rule are `gate.rs`'s unchanged (`|pred| >
//! threshold_mult × round_trip`); only what an accepted signal turns into
//! differs from `simulate_with_state_atr`:
//!
//! - A post-only limit at `P = close` of the signal bar rests from `C + 1 s`
//!   to `C + 60 s` (`C` = the signal bar's close), half-open in ms:
//!   `[C + 1000, C + 60 000)`.
//! - It is filled iff a tape trade in that window prints STRICTLY through
//!   `P` (long: `price < P`; short: `price > P`). A print AT `P` never fills
//!   us. Fill price = `P`, fill time = that trade's `ts_ms`. No such print =
//!   a counted miss, no trade.
//! - The fill minute (bar with open `C`) is resolved on the tape from the
//!   fill trade onward, in archive order: the first later print at or
//!   through the stop exits at the stop, at or through the target exits at
//!   the target. From the next bar on, `exits::resolve_exit` walks OHLC
//!   exactly as Amendment 3 (stop-wins-ties, gapped stop at the open, time
//!   exit at the bar at `C + H·60`). Stop/target distances come from the
//!   fill price `P` and ATR(14) at the SIGNAL bar (causal at placement).
//! - Round trip (§5.4): `fee_maker + fee_taker + spread_p75/2 + impact` -
//!   resolved by `gate::resolve_round_trip` under `CostMode::MakerEntry`,
//!   not here.
//! - One order or position per asset at a time: a resting order blocks
//!   until `C + 60 s`; a position until its exit bar.
//!
//! Every fill-model decision is a pure function over the tape slice it is
//! handed (`resolve_fill`, `resolve_fill_minute_exit`), tested below with
//! the exact edges §5.6 names.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use features_scalper::TapeTrade;

use crate::exits::{self, ExitKind};
use crate::gate::{AssetPath, Pred, Trade};
use crate::tape::TapeCursor;

/// Placement latency: the order cannot rest before `C + 1000 ms`.
pub const PLACEMENT_LATENCY_MS: i64 = 1000;
/// Rest window end (exclusive): `C + 60 000 ms`.
pub const REST_WINDOW_MS: i64 = 60_000;

/// Outcome tallies for one `simulate_maker` call, summed across folds by
/// `cmd_gate`.
#[derive(Debug, Clone, Default)]
pub struct MakerStatsAccum {
    /// Signals accepted by the threshold rule with a resolvable cost and a
    /// free slot - i.e. orders actually placed.
    pub signals: usize,
    pub fills: usize,
    pub misses: usize,
    pub fill_delay_ms_sum: u64,
    /// Placed orders whose fill minute had no tape coverage (no day file) -
    /// counted, not fabricated; no order can be said to have filled.
    pub skipped_no_tape: usize,
    /// Signal bar had no ATR yet (cold / post-gap) - same as the taker path.
    pub skipped_no_atr: usize,
    /// No bar at open `C` (fill bar) or at `C + H·60` (end bar) in the store.
    pub skipped_no_bar: usize,
    pub stops: usize,
    pub targets: usize,
    pub time_exits: usize,
    /// Of the stops/targets, how many resolved inside the fill minute (on
    /// the tape) rather than on a later OHLC bar.
    pub fill_minute_stops: usize,
    pub fill_minute_targets: usize,
    pub bars_held_sum: u64,
    /// Per asset: (signals, fills).
    pub per_asset: PerAssetCounts,
}

pub type PerAssetCounts = BTreeMap<String, (usize, usize)>;

/// The trade-through fill rule over `trades` (one minute of tape, archive
/// order): the first trade with `ts_ms` in `[lo_ms, hi_ms)` printing
/// strictly through `p` for `side` (+1 long: `price < p`; −1 short:
/// `price > p`). Returns `(archive index, ts_ms)`.
pub fn resolve_fill(
    trades: &[TapeTrade],
    side: i8,
    p: f64,
    lo_ms: i64,
    hi_ms: i64,
) -> Option<(usize, i64)> {
    trades.iter().enumerate().find_map(|(i, t)| {
        if t.ts_ms < lo_ms || t.ts_ms >= hi_ms {
            return None;
        }
        let through = if side >= 0 { t.price < p } else { t.price > p };
        if through {
            Some((i, t.ts_ms))
        } else {
            None
        }
    })
}

/// Fill-minute resolution on the tape: over `after` (the trades of the fill
/// minute strictly after the fill trade, archive order), the first print at
/// or through the stop exits at `stop_px`; at or through the target exits
/// at `target_px`. `None` if neither happens inside the minute.
pub fn resolve_fill_minute_exit(
    after: &[TapeTrade],
    side: i8,
    stop_px: f64,
    target_px: f64,
) -> Option<(f64, ExitKind)> {
    for t in after {
        let (stop_hit, target_hit) = if side >= 0 {
            (t.price <= stop_px, t.price >= target_px)
        } else {
            (t.price >= stop_px, t.price <= target_px)
        };
        if stop_hit {
            return Some((stop_px, ExitKind::Stop));
        }
        if target_hit {
            return Some((target_px, ExitKind::Target));
        }
    }
    None
}

/// Amendment 5's maker sibling of `gate::simulate_with_state_atr`. Same
/// inputs plus the per-asset tape cursors (keyed by matrix asset name) that
/// serve the fill minute's trades. `carry` / returned state: per-asset
/// "free again at this ts" - the exit bar's ts for a filled trade, `C + 60`
/// for a miss.
#[allow(clippy::too_many_arguments)]
pub fn simulate_maker(
    preds: &[Pred],
    round_trip_by_asset_day: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
    paths: &BTreeMap<String, AssetPath>,
    tapes: &mut BTreeMap<String, TapeCursor>,
    horizon_min: i64,
    threshold_mult: f64,
    carry: &BTreeMap<String, i64>,
) -> Result<(Vec<Trade>, BTreeMap<String, i64>, MakerStatsAccum), String> {
    use features_scalper::TapeSource;

    let mut sorted: Vec<&Pred> = preds.iter().collect();
    sorted.sort_by(|a, b| (a.asset.as_str(), a.ts).cmp(&(b.asset.as_str(), b.ts)));

    let horizon_secs = horizon_min.max(0) * 60;
    let mut flat_after: BTreeMap<String, i64> = carry.clone();
    let mut trades = Vec::new();
    let mut stats = MakerStatsAccum::default();

    for p in sorted {
        let Some(round_trip) = round_trip_by_asset_day
            .get(p.asset.as_str())
            .and_then(|by_day| by_day.get(&crate::gate::date_of(p.ts)))
            .copied()
        else {
            continue;
        };
        let next_free = *flat_after.get(p.asset.as_str()).unwrap_or(&i64::MIN);
        if p.ts < next_free {
            continue; // resting order or open position
        }
        let threshold = threshold_mult * round_trip;
        let side: i8 = if p.pred_bps > threshold {
            1
        } else if p.pred_bps < -threshold {
            -1
        } else {
            continue;
        };
        let Some(path) = paths.get(p.asset.as_str()) else {
            continue;
        };
        let Some(&idx) = path.index_by_ts.get(&p.ts) else {
            continue;
        };
        let Some(atr) = path.atr[idx] else {
            stats.skipped_no_atr += 1;
            continue;
        };
        // C = signal bar close = open of the fill bar.
        let c_ts = p.ts + 60;
        let (Some(&fill_idx), Some(&end_idx)) = (
            path.index_by_ts.get(&c_ts),
            path.index_by_ts.get(&(c_ts + horizon_secs)),
        ) else {
            stats.skipped_no_bar += 1;
            continue;
        };
        let Some(cursor) = tapes.get_mut(p.asset.as_str()) else {
            stats.skipped_no_tape += 1;
            continue;
        };
        let Some(minute) = cursor.minute(c_ts)? else {
            stats.skipped_no_tape += 1;
            continue;
        };

        // The order is placed.
        stats.signals += 1;
        stats.per_asset.entry(p.asset.clone()).or_default().0 += 1;
        let entry_px = path.bars[idx].close;
        let c_ms = c_ts * 1000;
        let Some((fill_i, fill_ts_ms)) = resolve_fill(
            &minute.trades,
            side,
            entry_px,
            c_ms + PLACEMENT_LATENCY_MS,
            c_ms + REST_WINDOW_MS,
        ) else {
            stats.misses += 1;
            flat_after.insert(p.asset.clone(), c_ts + 60);
            continue;
        };
        stats.fills += 1;
        stats.per_asset.entry(p.asset.clone()).or_default().1 += 1;
        stats.fill_delay_ms_sum += (fill_ts_ms - c_ms) as u64;

        let (stop_px, target_px) = exits::stop_target(entry_px, side, atr);
        // Fill minute on the tape, then OHLC from the next bar.
        let (exit_px, exit_kind, bars_held) = match resolve_fill_minute_exit(
            &minute.trades[fill_i + 1..],
            side,
            stop_px,
            target_px,
        ) {
            Some((px, kind)) => {
                match kind {
                    ExitKind::Stop => stats.fill_minute_stops += 1,
                    ExitKind::Target => stats.fill_minute_targets += 1,
                    ExitKind::Time => {}
                }
                (px, kind, 0usize)
            }
            None => {
                let horizon_bars = end_idx - fill_idx;
                let exit = exits::resolve_exit(
                    entry_px,
                    side,
                    stop_px,
                    target_px,
                    &path.bars[fill_idx..=end_idx],
                    horizon_bars,
                );
                (exit.exit_px, exit.exit_kind, exit.bars_held)
            }
        };

        let gross_bps = side as f64 * (exit_px / entry_px - 1.0) * 1e4;
        let net_bps = gross_bps - round_trip;
        trades.push(Trade {
            asset: p.asset.clone(),
            entry_ts: c_ts,
            side,
            net_bps,
        });
        match exit_kind {
            ExitKind::Stop => stats.stops += 1,
            ExitKind::Target => stats.targets += 1,
            ExitKind::Time => stats.time_exits += 1,
        }
        stats.bars_held_sum += bars_held as u64;
        let exit_ts = path.bars[fill_idx + bars_held].ts_utc.timestamp();
        flat_after.insert(p.asset.clone(), exit_ts);
    }
    Ok((trades, flat_after, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ts_ms: i64, price: f64) -> TapeTrade {
        TapeTrade {
            ts_ms,
            price,
            qty: 1.0,
            is_buy: true,
        }
    }

    const C_MS: i64 = 1_000_000_000_000; // any minute-aligned close, in ms

    #[test]
    fn a_print_at_our_price_never_fills_a_print_through_it_fills_at_that_trade() {
        let trades = vec![
            t(C_MS + 2_000, 100.0), // AT P: no fill (long)
            t(C_MS + 3_000, 100.0),
            t(C_MS + 4_500, 99.9), // through P: fill
            t(C_MS + 5_000, 99.0),
        ];
        assert_eq!(
            resolve_fill(&trades, 1, 100.0, C_MS + 1000, C_MS + 60_000),
            Some((2, C_MS + 4_500))
        );
        // Short mirrored: needs a print strictly ABOVE P.
        assert_eq!(
            resolve_fill(&trades, -1, 100.0, C_MS + 1000, C_MS + 60_000),
            None
        );
        let up = vec![t(C_MS + 2_000, 100.0), t(C_MS + 2_500, 100.1)];
        assert_eq!(
            resolve_fill(&up, -1, 100.0, C_MS + 1000, C_MS + 60_000),
            Some((1, C_MS + 2_500))
        );
    }

    #[test]
    fn a_print_before_the_latency_or_at_the_window_end_does_not_fill() {
        let trades = vec![
            t(C_MS + 999, 99.0),    // before C+1000: not resting yet
            t(C_MS + 60_000, 99.0), // exactly C+60s: window is half-open
        ];
        assert_eq!(
            resolve_fill(&trades, 1, 100.0, C_MS + 1000, C_MS + 60_000),
            None
        );
        let ok = vec![t(C_MS + 1_000, 99.0)];
        assert_eq!(
            resolve_fill(&ok, 1, 100.0, C_MS + 1000, C_MS + 60_000),
            Some((0, C_MS + 1_000))
        );
        let late = vec![t(C_MS + 59_999, 99.0)];
        assert_eq!(
            resolve_fill(&late, 1, 100.0, C_MS + 1000, C_MS + 60_000),
            Some((0, C_MS + 59_999))
        );
    }

    #[test]
    fn no_print_is_a_miss() {
        assert_eq!(
            resolve_fill(&[], 1, 100.0, C_MS + 1000, C_MS + 60_000),
            None
        );
    }

    #[test]
    fn fill_minute_exit_follows_archive_order_and_only_prints_after_the_fill() {
        // Long from 100, stop 96, target 104.8 (k=4, RR 1.2 with ATR=1).
        let (stop, target) = exits::stop_target(100.0, 1, 1.0);
        assert!((stop - 96.0).abs() < 1e-12 && (target - 104.8).abs() < 1e-12);
        // Target print first, then stop print: target wins by order.
        let after = vec![t(1, 105.0), t(2, 90.0)];
        assert_eq!(
            resolve_fill_minute_exit(&after, 1, stop, target),
            Some((target, ExitKind::Target))
        );
        // Stop print first.
        let after = vec![t(1, 96.0), t(2, 106.0)];
        assert_eq!(
            resolve_fill_minute_exit(&after, 1, stop, target),
            Some((stop, ExitKind::Stop))
        );
        // Neither inside the minute.
        let after = vec![t(1, 100.5), t(2, 99.5)];
        assert_eq!(resolve_fill_minute_exit(&after, 1, stop, target), None);
        // Short mirrored: stop above, target below.
        let (stop_s, target_s) = exits::stop_target(100.0, -1, 1.0);
        let after = vec![t(1, 95.0)];
        assert_eq!(
            resolve_fill_minute_exit(&after, -1, stop_s, target_s),
            Some((target_s, ExitKind::Target))
        );
    }
}
