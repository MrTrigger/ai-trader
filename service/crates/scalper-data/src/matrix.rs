//! The training matrix: join feature rows with forward returns, then keep a
//! strided, fully-warm subset for the trainer.
//!
//! Two pure functions do the real work (`forward_returns_bps`,
//! `matrix_rows`); the CLI wiring in `main.rs` is the only place that
//! touches the store or the filesystem.

use std::collections::BTreeMap;

use features_crypto::Bar;
use features_scalper::{FeatureRow, FEATURE_NAMES};
use serde::{Deserialize, Serialize};

/// One trainer-ready row: causal features and forward-looking labels for a
/// single asset at a single timestamp. `asset` is the universe coin name
/// (e.g. `kPEPE`), not the uppercased store key - see the identity rule in
/// `main.rs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixRow {
    pub ts: i64,
    pub asset: String,
    pub features: BTreeMap<String, f64>,
    pub fwd_bps: BTreeMap<String, f64>,
}

/// Forward log-returns in bps, one map per bar in `bars`.
///
/// For bar `t` and horizon `H` (minutes), looks up the bar at exact
/// timestamp `t + H*60s` in a ts->close index and records
/// `1e4 * ln(close[t+H]/close[t])` under key `H.to_string()`. No bar at that
/// exact timestamp (series ends, or a gap swallowed it) means the entry is
/// simply absent for that horizon at that row - never a fabricated value.
/// Pure: no knowledge of `interval_s` or timezones beyond what `Bar` already
/// carries.
pub fn forward_returns_bps(bars: &[Bar], horizons_min: &[i64]) -> Vec<BTreeMap<String, f64>> {
    let close_by_ts: BTreeMap<i64, f64> = bars
        .iter()
        .map(|b| (b.ts_utc.timestamp(), b.close))
        .collect();

    bars.iter()
        .map(|b| {
            let t = b.ts_utc.timestamp();
            let mut out = BTreeMap::new();
            for &h in horizons_min {
                let target_ts = t + h * 60;
                if let Some(&target_close) = close_by_ts.get(&target_ts) {
                    out.insert(h.to_string(), 1e4 * (target_close / b.close).ln());
                }
            }
            out
        })
        .collect()
}

/// Join `rows` (from `features_scalper::compute`) with `fwd` (from
/// `forward_returns_bps`, same length and index alignment as `rows`) and
/// keep every `stride_min`-th row by index, dropping any row where a
/// feature is still cold (`None`), a horizon present anywhere in `fwd` is
/// missing for that row, or a feature is non-finite. Pure.
///
/// `features_scalper::compute` upholds "a feature is `Some` only if it is
/// finite" as an invariant, but this function does not trust that from the
/// caller's side either: any row carrying a `Some(NaN)`/`Some(inf)` feature
/// is dropped here (and counted as dropped, same as a cold row), and a
/// `debug_assert!` on every row this function actually emits re-checks
/// that postcondition in dev/test builds - so a gate's strict f64 JSONL
/// reader can never be handed a `null`, whether or not the upstream
/// invariant held.
///
/// Rows are returned in the order they were given - one asset's block at a
/// time, ts ascending within the block, when the CLI calls this once per
/// asset and appends the results. That order carries no meaning for
/// training: the trainer shuffles and splits by time itself.
pub fn matrix_rows(
    rows: &[FeatureRow],
    fwd: &[BTreeMap<String, f64>],
    stride_min: usize,
    coin: &str,
) -> Vec<MatrixRow> {
    let stride = stride_min.max(1);

    // The full horizon set is whatever ever appears in `fwd` - a row only
    // counts as "all horizons present" if it matches that set exactly, not
    // merely "non-empty".
    let all_horizons: std::collections::BTreeSet<&str> = fwd
        .iter()
        .flat_map(|m| m.keys())
        .map(String::as_str)
        .collect();

    rows.iter()
        .zip(fwd.iter())
        .enumerate()
        .filter(|(idx, _)| idx % stride == 0)
        .filter_map(|(_, (row, fwd_row))| {
            if row.values.iter().any(Option::is_none) {
                return None;
            }
            // A `Some` feature must be finite - `features_scalper::compute`
            // guarantees this, but a matrix that reaches the gate's strict
            // f64 JSONL reader is exactly the artifact a violation here
            // would corrupt (NaN serializes as `null`), so this function
            // enforces the postcondition itself rather than trusting the
            // caller.
            if row.values.iter().any(|v| v.is_some_and(|x| !x.is_finite())) {
                return None;
            }
            if !all_horizons.iter().all(|h| fwd_row.contains_key(*h)) {
                return None;
            }
            let features: BTreeMap<String, f64> = FEATURE_NAMES
                .iter()
                .zip(row.values.iter())
                .map(|(name, v)| ((*name).to_string(), v.expect("checked Some above")))
                .collect();
            debug_assert!(
                features.values().all(|v| v.is_finite()),
                "matrix_rows emitted a non-finite feature value - the finite filter above has a gap"
            );
            Some(MatrixRow {
                ts: row.ts_utc.timestamp(),
                asset: coin.to_string(),
                features,
                fwd_bps: fwd_row.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use features_scalper::{compute, MicroMinute};

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
    fn forward_returns_look_up_exact_timestamps_and_vanish_at_the_edge() {
        let bars = ramp(100);
        let fwd = forward_returns_bps(&bars, &[15, 30]);
        assert_eq!(fwd.len(), 100);
        let expect = 1e4 * (bars[15].close / bars[0].close).ln();
        assert!((fwd[0]["15"] - expect).abs() < 1e-9);
        // 100 bars, indices 0..=99: t=84 is the last with a 15m target (needs bar 99);
        // t=85 would need bar 100. No index has a 30m target beyond t=69.
        assert!(fwd[84].contains_key("15") && !fwd[84].contains_key("30"));
        assert!(!fwd[85].contains_key("15"));
    }

    /// fs-2's matrix rows require ALL 38 features Some, so a full micro
    /// series (every field covered every minute) is what makes any row
    /// warm here - an all-`None` micro slice would drop every row (that
    /// behavior has its own dedicated test elsewhere: `training-matrix`
    /// without `--micro-root`).
    fn full_micro(bars: &[Bar]) -> Vec<Option<MicroMinute>> {
        bars.iter()
            .map(|b| {
                Some(MicroMinute {
                    ts_s: b.ts_utc.timestamp(),
                    spread_bps: Some(4.0),
                    taker_buy_ratio: Some(0.55),
                    bid_02: Some(100.0),
                    ask_02: Some(90.0),
                    bid_10: Some(500.0),
                    ask_10: Some(480.0),
                    oi_value: Some(5_000.0),
                    taker_ls_ratio: Some(1.2),
                    funding_rate: Some(0.0001),
                })
            })
            .collect()
    }

    #[test]
    fn matrix_rows_drop_cold_rows_and_stride_samples() {
        let bars = ramp(200);
        let rows = compute(&bars, &bars, &full_micro(&bars)).unwrap();
        let fwd = forward_returns_bps(&bars, &[15]);
        let m = matrix_rows(&rows, &fwd, 5, "kTEST");
        assert!(!m.is_empty());
        assert!(m.iter().all(|r| r.asset == "kTEST"));
        assert!(
            m.iter().all(|r| r.features.len() == FEATURE_NAMES.len()),
            "only fully-warm rows survive"
        );
        assert!(m.iter().all(|r| r.fwd_bps.contains_key("15")));
        let ts: Vec<i64> = m.iter().map(|r| r.ts).collect();
        assert!(
            ts.windows(2).all(|w| w[1] - w[0] >= 300),
            "stride 5 = >=300s apart"
        );
    }

    /// `features_scalper::compute` upholds "Some only if finite" as an
    /// invariant, but `matrix_rows` must not simply trust that from its
    /// caller - a hand-built `FeatureRow` carrying `Some(NaN)` (the exact
    /// shape that would otherwise serialize as JSON `null` and be refused
    /// by the gate's strict f64 reader) must be dropped, and only that
    /// row, not the whole batch.
    #[test]
    fn a_non_finite_feature_value_is_dropped_and_counted_as_dropped() {
        let bars = ramp(200);
        let rows = compute(&bars, &bars, &full_micro(&bars)).unwrap();
        let fwd = forward_returns_bps(&bars, &[1]);

        let mut hostile_rows = rows.clone();
        // Rows 100/101 are otherwise fully warm; corrupt one feature each
        // to NaN/infinity, as if an upstream formula's own guard had a gap.
        hostile_rows[100].values[0] = Some(f64::NAN);
        hostile_rows[101].values[1] = Some(f64::INFINITY);

        let clean = matrix_rows(&rows, &fwd, 1, "kTEST");
        let with_hostile = matrix_rows(&hostile_rows, &fwd, 1, "kTEST");
        assert!(
            clean
                .iter()
                .any(|r| r.ts == bars[100].ts_utc.timestamp())
                && clean.iter().any(|r| r.ts == bars[101].ts_utc.timestamp()),
            "rows 100/101 must be warm and kept without corruption, or this test proves nothing"
        );

        assert_eq!(
            with_hostile.len(),
            clean.len() - 2,
            "the NaN and inf rows must be dropped, and only those two"
        );
        let kept_ts: std::collections::HashSet<i64> = with_hostile.iter().map(|r| r.ts).collect();
        assert!(!kept_ts.contains(&rows[100].ts_utc.timestamp()));
        assert!(!kept_ts.contains(&rows[101].ts_utc.timestamp()));
        assert!(
            with_hostile
                .iter()
                .flat_map(|r| r.features.values())
                .all(|v| v.is_finite()),
            "no surviving row may carry a non-finite feature"
        );
    }

    #[test]
    fn no_micro_coverage_at_all_drops_every_row_under_fs2() {
        let bars = ramp(200);
        let rows = compute(&bars, &bars, &vec![None; bars.len()]).unwrap();
        let fwd = forward_returns_bps(&bars, &[15]);
        let m = matrix_rows(&rows, &fwd, 5, "kTEST");
        assert!(
            m.is_empty(),
            "fs-2 rows require all 38 features Some; no micro data means the 12 \
             microstructure features are None everywhere, so nothing survives"
        );
    }
}
