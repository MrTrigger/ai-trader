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
/// feature is still cold (`None`) or a horizon present anywhere in `fwd` is
/// missing for that row. Pure.
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
            if !all_horizons.iter().all(|h| fwd_row.contains_key(*h)) {
                return None;
            }
            let features: BTreeMap<String, f64> = FEATURE_NAMES
                .iter()
                .zip(row.values.iter())
                .map(|(name, v)| ((*name).to_string(), v.expect("checked Some above")))
                .collect();
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
    use features_scalper::compute;

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

    #[test]
    fn matrix_rows_drop_cold_rows_and_stride_samples() {
        let bars = ramp(200);
        let rows = compute(&bars, &bars).unwrap();
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
}
