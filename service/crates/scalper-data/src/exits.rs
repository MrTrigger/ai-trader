//! ATR-based stop/target exit resolution - Amendment 3 of
//! `docs/scalper-research.md` (end of file, "Amendment 3 (2026-08-16): ATR
//! stop/target exits, pre-registered"). That amendment fixes every
//! parameter here (`k = 4`, R:R = 1.2:1, ATR(14) Wilder on 1m bars,
//! stop-wins-ties, time exit as fallback) - nothing in this module is
//! tunable; `gate.rs`'s `--exit` flag only switches between this path and
//! the Amendment-1 fixed-time exit, it does not parameterize this one.
//!
//! Pure functions only, same discipline as `gate.rs`'s simulation core - the
//! CLI wiring that reads bars from the store and threads them into the
//! simulation lives in `gate.rs::cmd_gate`.
//!
//! PnL convention: `gate.rs::simulate_with_state_atr` prices a resolved
//! `Exit` as a SIMPLE (linear) return, `side * (exit_px / entry_px - 1) *
//! 1e4` bps, not the log return `matrix.rs::forward_returns_bps` uses for
//! `fwd_bps` (`1e4 * ln(close_t+H / close_t)`). That's intentional, not an
//! inconsistency to reconcile: a stop/target fill is a real linear-perp
//! fill price, and linear PnL is exactly `exit/entry - 1` on a perpetual
//! (no compounding within the trade to justify a log return). The two
//! conventions diverge by `~r^2/2` bps for a move of `r` (since `ln(1+r) ≈
//! r - r^2/2`) - negligible at the sub-1%-per-trade moves this strategy
//! trades, so the `--exit time` path's `fwd_bps`-based net_bps and the
//! `--exit atr` path's `exit_px`-based net_bps stay numerically
//! comparable despite the different formulas.

use chrono::{DateTime, Utc};
use features_crypto::Bar;

/// Wilder's smoothing period for ATR.
const ATR_PERIOD: usize = 14;

/// Same >120s bar-gap-reset discipline `features_scalper::compute` applies
/// to its own rolling windows (that crate's `GAP_LIMIT_S`) - a gap resets
/// ATR's warm-up exactly the same way, so ATR at a bar right after a gap
/// never blends true range across the missing span.
const GAP_LIMIT_S: i64 = 120;

/// k in "stop = k * ATR(14)" (Amendment 3): k = 4, pre-registered and fixed
/// - not a CLI knob.
pub const ATR_STOP_K: f64 = 4.0;

/// Reward:risk multiple on the stop distance (Amendment 3): target = 1.2 *
/// stop.
pub const ATR_REWARD_RISK: f64 = 1.2;

/// Wilder-smoothed ATR(14) over `bars` (one asset's ascending 1m bars),
/// index-aligned 1:1 with `bars` - `out[i]` is ATR at `bars[i]`'s close.
///
/// `TR_t = max(h_t - l_t, |h_t - close_{t-1}|, |l_t - close_{t-1}|)`. The
/// very first bar in a run (the series start, or the first bar after a gap
/// exceeding 120s - see below) has no prior close, so it contributes no TR
/// at all: the first 14 bars that DO have a usable prior close produce the
/// first 14 TRs, and ATR is seeded as their simple mean, reported at the
/// bar that closes that 14-TR window. Concretely, for a gap-free run
/// starting at index `s`: `out[s..=s+13]` are `None` (bar `s` has no TR;
/// bars `s+1..=s+13` are still accumulating the seed window), `out[s+14]`
/// is the seeded mean of TR at `s+1..=s+14`, and `out[s+15..]` follow
/// Wilder's recursion `ATR_t = (ATR_{t-1} * 13 + TR_t) / 14`.
///
/// Gap handling: a >120s jump between consecutive bars' `ts_utc` clears the
/// prior close, the in-progress seed accumulator, and the running ATR - the
/// warm-up restarts from scratch at the bar after the gap, same as every
/// other rolling window in `features_scalper`.
pub fn atr14(bars: &[Bar]) -> Vec<Option<f64>> {
    let mut out = vec![None; bars.len()];
    let mut prev_ts: Option<DateTime<Utc>> = None;
    let mut prev_close: Option<f64> = None;
    let mut atr_prev: Option<f64> = None;
    let mut seed_trs: Vec<f64> = Vec::with_capacity(ATR_PERIOD);

    for (i, b) in bars.iter().enumerate() {
        let gapped = match prev_ts {
            Some(pts) => (b.ts_utc - pts).num_seconds() > GAP_LIMIT_S,
            None => false,
        };
        if gapped {
            prev_close = None;
            atr_prev = None;
            seed_trs.clear();
        }
        prev_ts = Some(b.ts_utc);

        if let Some(pc) = prev_close {
            let tr = (b.high - b.low)
                .max((b.high - pc).abs())
                .max((b.low - pc).abs());
            out[i] = match atr_prev {
                Some(prev_atr) => {
                    let atr = (prev_atr * (ATR_PERIOD as f64 - 1.0) + tr) / ATR_PERIOD as f64;
                    atr_prev = Some(atr);
                    Some(atr)
                }
                None => {
                    seed_trs.push(tr);
                    if seed_trs.len() == ATR_PERIOD {
                        let seed = seed_trs.iter().sum::<f64>() / ATR_PERIOD as f64;
                        atr_prev = Some(seed);
                        Some(seed)
                    } else {
                        None
                    }
                }
            };
        }

        prev_close = Some(b.close);
    }
    out
}

/// How a trade's stop/target resolution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    Stop,
    Target,
    Time,
}

/// The realized outcome of resolving one trade's exit against its asset's
/// bar path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Exit {
    pub exit_px: f64,
    pub exit_kind: ExitKind,
    /// How many bars after entry the exit happened on (0 if the path had no
    /// bars after entry at all - a degenerate end-of-series case).
    pub bars_held: usize,
}

/// Long/short stop and target price levels, k*ATR / RR*k*ATR away from
/// `entry_px`, per Amendment 3 (`stop = k*ATR`, `target = RR*stop`, k=4,
/// RR=1.2). `side` is +1 (long) or -1 (short, or anything `< 0`); long:
/// stop below entry, target above; short: mirrored.
pub fn stop_target(entry_px: f64, side: i8, atr: f64) -> (f64, f64) {
    let stop_dist = ATR_STOP_K * atr;
    let target_dist = ATR_REWARD_RISK * stop_dist;
    if side >= 0 {
        (entry_px - stop_dist, entry_px + target_dist)
    } else {
        (entry_px + stop_dist, entry_px - target_dist)
    }
}

/// Resolve a trade's exit against `path`, where `path[0]` is the ENTRY bar
/// itself and `path[1..]` are the bars that follow it, in order. Entry
/// happens at `path[0]`'s close (the signal bar's close, unchanged from
/// Amendment 1), so `path[0]` is never read for a stop/target hit - only
/// `path[1..]` can trigger one. This is the only look-ahead the ATR path is
/// allowed: bars strictly after the entry bar, resolving the trade's own
/// realized outcome, never used to inform the entry decision itself.
///
/// `horizon_bars` MUST be sized by the caller as the exact array-index
/// distance to the bar at entry_ts + H*60 (the same bar Amendment 1's
/// fixed-time exit reads its `fwd_bps` label from), not a raw
/// minutes-equals-bars assumption - `gate.rs::simulate_with_state_atr`
/// does this via an exact ts lookup (`AssetPath::index_by_ts`). Sizing it
/// that way is what keeps a data hole INSIDE the window from extending the
/// walk past the labeled horizon: a gap just shortens `path` (fewer array
/// elements between the entry and end bars), it never changes which bar
/// the walk is required to stop at. This function itself stays ignorant of
/// wall-clock time - it just walks the bars it's given - so getting this
/// right is entirely on the caller.
///
/// Walks at most `horizon_bars` bars after entry (or however many `path`
/// actually has beyond the entry bar, if fewer - a defensive fallback, not
/// the normal path once callers size `horizon_bars` as above). Per bar, in
/// this order: stop first (long: `low <= stop_px`; short: `high >=
/// stop_px`) - so a bar that touches both stop and target in the same bar
/// counts as a stop, conservative and automatic purely from checking stop
/// before target, per Amendment 3 ("intrabar order is unknowable from
/// bars") - then target (long: `high >= target_px`; short: `low <=
/// target_px`). If neither hits within the walked bars, exits at the last
/// bar actually walked's close - the Amendment-1 time exit, as fallback
/// (this also covers `path` simply running short of `horizon_bars`, the
/// defensive case above).
///
/// Gapped-through-stop conservatism: if the bar that triggers the stop is
/// itself the first bar after a >120s gap (the same `GAP_LIMIT_S` ATR's
/// warm-up resets on) AND its OPEN has already breached the stop level
/// (long: `open <= stop_px`; short: `open >= stop_px`), the fill is at
/// that bar's OPEN, not the nominal `stop_px` - the missing span could
/// have crossed the stop well before the bar even opened, and pricing the
/// fill at the nicer `stop_px` would be optimistic about a level the book
/// may never have traded at post-gap. This is deliberately NOT symmetric:
/// a gapped-through TARGET still fills at `target_px`, never the more
/// favorable open - Amendment 3 registers one conservative adjustment
/// (worse fills get worse, better fills don't get better), not a general
/// "use whichever price is realistic" rule.
pub fn resolve_exit(
    entry_px: f64,
    side: i8,
    stop_px: f64,
    target_px: f64,
    path: &[Bar],
    horizon_bars: usize,
) -> Exit {
    let available = path.len().saturating_sub(1).min(horizon_bars);
    let mut last_close = entry_px;
    let mut bars_held = 0usize;
    for (i, bar) in path.iter().enumerate().take(available + 1).skip(1) {
        bars_held = i;
        last_close = bar.close;
        let gapped_here = (bar.ts_utc - path[i - 1].ts_utc).num_seconds() > GAP_LIMIT_S;

        let stop_hit = if side >= 0 {
            bar.low <= stop_px
        } else {
            bar.high >= stop_px
        };
        if stop_hit {
            let gapped_through = gapped_here
                && if side >= 0 {
                    bar.open <= stop_px
                } else {
                    bar.open >= stop_px
                };
            return Exit {
                exit_px: if gapped_through { bar.open } else { stop_px },
                exit_kind: ExitKind::Stop,
                bars_held,
            };
        }

        let target_hit = if side >= 0 {
            bar.high >= target_px
        } else {
            bar.low <= target_px
        };
        if target_hit {
            return Exit {
                exit_px: target_px,
                exit_kind: ExitKind::Target,
                bars_held,
            };
        }
    }
    Exit {
        exit_px: last_close,
        exit_kind: ExitKind::Time,
        bars_held,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn bar_at(ts: DateTime<Utc>, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            ts_utc: ts,
            asset: "TEST".into(),
            interval_s: 60,
            open,
            high,
            low,
            close,
            volume: 1.0,
            quote_volume: None,
            trades: None,
        }
    }

    fn minute_bars(closes: &[(f64, f64, f64, f64)]) -> Vec<Bar> {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        closes
            .iter()
            .enumerate()
            .map(|(i, &(o, h, l, c))| bar_at(start + Duration::minutes(i as i64), o, h, l, c))
            .collect()
    }

    // -- atr14 ---------------------------------------------------------

    #[test]
    fn atr14_seeds_at_index_14_and_takes_one_wilder_step() {
        // 16 bars: index 0 has no prior close (no TR). Indices 1..=14 give
        // the first 14 TRs, seeding ATR at index 14. Index 15 takes one
        // Wilder step off that seed.
        let ohlc: Vec<(f64, f64, f64, f64)> = (0..16)
            .map(|i| {
                let base = 100.0 + i as f64;
                (base, base + 2.0, base - 1.0, base + 0.5)
            })
            .collect();
        let bars = minute_bars(&ohlc);
        let atr = atr14(&bars);
        assert_eq!(atr.len(), 16);
        assert!(atr[..14].iter().all(Option::is_none), "indices 0..=13 must be None: {atr:?}");
        assert!(atr[14].is_some(), "index 14 must be the seeded ATR");

        // Hand-compute the seed: TR_t for t=1..=14 uses close_{t-1} =
        // bars[t-1].close.
        let mut trs = Vec::new();
        for t in 1..=14 {
            let h = bars[t].high;
            let l = bars[t].low;
            let pc = bars[t - 1].close;
            trs.push((h - l).max((h - pc).abs()).max((l - pc).abs()));
        }
        let seed: f64 = trs.iter().sum::<f64>() / 14.0;
        assert!((atr[14].unwrap() - seed).abs() < 1e-9);

        // One Wilder step at index 15.
        let h15 = bars[15].high;
        let l15 = bars[15].low;
        let pc15 = bars[14].close;
        let tr15 = (h15 - l15).max((h15 - pc15).abs()).max((l15 - pc15).abs());
        let expected_15 = (seed * 13.0 + tr15) / 14.0;
        assert!((atr[15].unwrap() - expected_15).abs() < 1e-9);
    }

    #[test]
    fn a_gap_over_120s_resets_the_atr_warmup() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut bars: Vec<Bar> = (0..20)
            .map(|i| {
                let base = 100.0 + i as f64;
                bar_at(
                    start + Duration::minutes(i),
                    base,
                    base + 2.0,
                    base - 1.0,
                    base + 0.5,
                )
            })
            .collect();
        // Splice a >120s gap between bar 5 and bar 6's ts.
        for b in bars.iter_mut().skip(6) {
            b.ts_utc += Duration::minutes(3);
        }
        let atr = atr14(&bars);
        // Warm-up restarts at index 6 (the bar after the gap): index 6 has
        // no prior close post-reset, so indices 6..=19 (14 more bars needed)
        // never reach a seed within this 20-bar series.
        assert!(atr[6..].iter().all(Option::is_none));
    }

    // -- resolve_exit ----------------------------------------------------

    fn flat_path(n: usize, px: f64) -> Vec<Bar> {
        minute_bars(&vec![(px, px, px, px); n])
    }

    #[test]
    fn long_stop_hit() {
        let mut path = flat_path(5, 100.0);
        path[2] = {
            let mut b = path[2].clone();
            b.low = 90.0; // below stop
            b.high = 100.0;
            b
        };
        let exit = resolve_exit(100.0, 1, 95.0, 106.0, &path, 4);
        assert_eq!(exit.exit_kind, ExitKind::Stop);
        assert_eq!(exit.exit_px, 95.0);
        assert_eq!(exit.bars_held, 2);
    }

    #[test]
    fn long_target_hit() {
        let mut path = flat_path(5, 100.0);
        path[3] = {
            let mut b = path[3].clone();
            b.high = 110.0; // above target
            b.low = 100.0;
            b
        };
        let exit = resolve_exit(100.0, 1, 95.0, 106.0, &path, 4);
        assert_eq!(exit.exit_kind, ExitKind::Target);
        assert_eq!(exit.exit_px, 106.0);
        assert_eq!(exit.bars_held, 3);
    }

    #[test]
    fn short_stop_hit() {
        let mut path = flat_path(5, 100.0);
        path[1] = {
            let mut b = path[1].clone();
            b.high = 110.0; // above short stop
            b.low = 100.0;
            b
        };
        let exit = resolve_exit(100.0, -1, 105.0, 94.0, &path, 4);
        assert_eq!(exit.exit_kind, ExitKind::Stop);
        assert_eq!(exit.exit_px, 105.0);
        assert_eq!(exit.bars_held, 1);
    }

    #[test]
    fn short_target_hit() {
        let mut path = flat_path(5, 100.0);
        path[1] = {
            let mut b = path[1].clone();
            b.low = 90.0; // below short target
            b.high = 100.0;
            b
        };
        let exit = resolve_exit(100.0, -1, 105.0, 94.0, &path, 4);
        assert_eq!(exit.exit_kind, ExitKind::Target);
        assert_eq!(exit.exit_px, 94.0);
        assert_eq!(exit.bars_held, 1);
    }

    #[test]
    fn neither_touched_falls_back_to_the_horizon_close() {
        let path = flat_path(6, 100.0);
        let exit = resolve_exit(100.0, 1, 90.0, 120.0, &path, 4);
        assert_eq!(exit.exit_kind, ExitKind::Time);
        assert_eq!(exit.bars_held, 4);
        assert_eq!(exit.exit_px, path[4].close);
    }

    #[test]
    fn both_touched_in_one_bar_counts_as_a_stop() {
        let mut path = flat_path(4, 100.0);
        path[1] = {
            let mut b = path[1].clone();
            b.low = 90.0; // hits stop
            b.high = 110.0; // AND hits target
            b
        };
        let long = resolve_exit(100.0, 1, 95.0, 106.0, &path, 3);
        assert_eq!(long.exit_kind, ExitKind::Stop, "stop-wins-ties for longs");

        let mut short_path = flat_path(4, 100.0);
        short_path[1] = {
            let mut b = short_path[1].clone();
            b.high = 110.0; // hits short stop
            b.low = 90.0; // AND hits short target
            b
        };
        let short = resolve_exit(100.0, -1, 105.0, 94.0, &short_path, 3);
        assert_eq!(short.exit_kind, ExitKind::Stop, "stop-wins-ties for shorts");
    }

    #[test]
    fn the_entry_bar_itself_is_never_eligible_for_a_hit() {
        // path[0] (the entry bar) would trigger the stop on its own low if
        // it were checked - it must not be, since entry happens at its
        // close, not before it.
        let mut path = flat_path(4, 100.0);
        path[0] = {
            let mut b = path[0].clone();
            b.low = 10.0; // would trigger the stop if index 0 were checked
            b
        };
        let exit = resolve_exit(100.0, 1, 95.0, 106.0, &path, 3);
        assert_ne!(
            exit.exit_kind,
            ExitKind::Stop,
            "the entry bar's own low must not be read for a stop hit"
        );
    }

    #[test]
    fn running_out_of_path_before_the_horizon_still_exits_at_the_last_bar() {
        // Only 2 bars after entry available, horizon asks for 5.
        let path = flat_path(3, 100.0);
        let exit = resolve_exit(100.0, 1, 90.0, 120.0, &path, 5);
        assert_eq!(exit.exit_kind, ExitKind::Time);
        assert_eq!(exit.bars_held, 2);
        assert_eq!(exit.exit_px, path[2].close);
    }

    // -- gapped-through fills --------------------------------------------

    #[test]
    fn a_gapped_through_stop_fills_at_the_bars_open_not_stop_px() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entry = bar_at(start, 100.0, 100.05, 99.95, 100.0);
        // A >120s (5 minute) gap; this bar's own OPEN already breached the
        // stop (95.0), well before it printed a low - the missing span
        // could have crossed 95.0 at any point, so the fill must not be
        // priced at the nicer stop_px.
        let gapped = bar_at(start + Duration::minutes(5), 93.0, 94.0, 90.0, 91.0);
        let path = vec![entry, gapped];

        let exit = resolve_exit(100.0, 1, 95.0, 110.0, &path, 5);
        assert_eq!(exit.exit_kind, ExitKind::Stop);
        assert_eq!(
            exit.exit_px, 93.0,
            "a gapped-through stop must fill at the bar's open, not stop_px"
        );
        assert_eq!(exit.bars_held, 1);
    }

    #[test]
    fn a_gapped_through_target_still_fills_at_target_px() {
        let start = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let entry = bar_at(start, 100.0, 100.05, 99.95, 100.0);
        // Same shape as the stop case, mirrored: a >120s gap bar whose
        // OPEN already blew through the target (105.0). Unlike a stop, a
        // gapped-through TARGET gets no favorable treatment - Amendment 3
        // registers one conservative adjustment (worse fills get worse),
        // not a symmetric "best available price" rule.
        let gapped = bar_at(start + Duration::minutes(5), 110.0, 115.0, 108.0, 112.0);
        let path = vec![entry, gapped];

        let exit = resolve_exit(100.0, 1, 90.0, 105.0, &path, 5);
        assert_eq!(exit.exit_kind, ExitKind::Target);
        assert_eq!(
            exit.exit_px, 105.0,
            "a gapped-through target must still fill at target_px, not the more favorable open"
        );
        assert_eq!(exit.bars_held, 1);
    }

    #[test]
    fn a_stop_touched_without_a_gap_fills_at_stop_px_even_if_open_looks_breached() {
        // No gap here (1-minute spacing) - the open-fill rule must only
        // ever trigger on a bar immediately after a >120s gap, never on an
        // ordinary consecutive bar, regardless of what its open looks like
        // relative to the stop.
        let mut path = flat_path(3, 100.0);
        path[1] = {
            let mut b = path[1].clone();
            b.open = 90.0; // "beyond" the stop, but this bar is NOT gapped
            b.low = 90.0;
            b
        };
        let exit = resolve_exit(100.0, 1, 95.0, 110.0, &path, 3);
        assert_eq!(exit.exit_kind, ExitKind::Stop);
        assert_eq!(
            exit.exit_px, 95.0,
            "without a preceding gap, a stop hit must still fill at stop_px"
        );
    }
}
