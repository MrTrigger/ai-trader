//! Builds the `MicroMinute` series `features_scalper::compute` needs for
//! fs-2, by reading the four Binance micro archive JSONL families
//! (book/flow/metrics/funding - Task 1's `binance_micro` module writes
//! them) that an asset's Binance symbol produced under
//! `{data-root}/binance-micro/`, and snapping every source DOWNWARD onto
//! each bar's close timestamp: the latest record with `ts <= bar.ts_utc`,
//! per the plan's causal-alignment constraint. Two of the four sources also
//! carry a staleness tolerance beyond "downward" - a snapshot found but too
//! old is worse than no snapshot, so it is dropped rather than reused:
//! book/flow snap within 120s (their native cadence is ~30s/1m), metrics
//! snap within 10 minutes (native cadence 5m, one missed row's slack).
//! Funding has no tolerance window: the rate in effect is whatever the
//! latest settlement at or before `ts` says, however many hours old - that
//! IS the current funding period's rate.
//!
//! Every bar gets `Some(MicroMinute)`; a source with no qualifying record
//! simply leaves that source's fields `None` on it. The outer `Option` in
//! `features_scalper::compute`'s `micro` slice exists for callers with no
//! micro data at all (e.g. `training-matrix` run without `--micro-root`),
//! which pass an all-`None` slice directly rather than going through this
//! module.

use std::path::Path;

use features_crypto::Bar;
use features_scalper::MicroMinute;

use crate::binance_micro::{BookMinute, FlowMinute, FundingRow, MetricsRow};

const BOOK_FLOW_TOLERANCE_S: i64 = 120;
const METRICS_TOLERANCE_S: i64 = 600;

/// The last row in `rows` (sorted ascending by `key`) with `key(row) <=
/// ts`, then discarded if `ts - key(row) > tolerance_s` (when
/// `tolerance_s` is `Some`) - `None` tolerance means "any age is fine",
/// used only for funding.
fn snap_downward<'a, T>(
    rows: &'a [T],
    ts: i64,
    key: impl Fn(&T) -> i64,
    tolerance_s: Option<i64>,
) -> Option<&'a T> {
    let idx = rows.partition_point(|r| key(r) <= ts);
    if idx == 0 {
        return None;
    }
    let candidate = &rows[idx - 1];
    if let Some(tol) = tolerance_s {
        if ts - key(candidate) > tol {
            return None;
        }
    }
    Some(candidate)
}

/// Pure core: given each source's rows (already loaded, any order - this
/// sorts them), build one `MicroMinute` per bar in `bars`. Never errors.
pub fn snap_micro(
    bars: &[Bar],
    book: &[BookMinute],
    flow: &[FlowMinute],
    metrics: &[MetricsRow],
    funding: &[FundingRow],
) -> Vec<Option<MicroMinute>> {
    let mut book = book.to_vec();
    book.sort_by_key(|r| r.ts_s);
    let mut flow = flow.to_vec();
    flow.sort_by_key(|r| r.ts_s);
    let mut metrics = metrics.to_vec();
    metrics.sort_by_key(|r| r.ts_s);
    let mut funding = funding.to_vec();
    funding.sort_by_key(|r| r.ts_s);

    bars.iter()
        .map(|b| {
            let ts = b.ts_utc.timestamp();
            let book_row = snap_downward(&book, ts, |r| r.ts_s, Some(BOOK_FLOW_TOLERANCE_S));
            let flow_row = snap_downward(&flow, ts, |r| r.ts_s, Some(BOOK_FLOW_TOLERANCE_S));
            let metrics_row = snap_downward(&metrics, ts, |r| r.ts_s, Some(METRICS_TOLERANCE_S));
            let funding_row = snap_downward(&funding, ts, |r| r.ts_s, None);

            Some(MicroMinute {
                ts_s: ts,
                spread_bps: flow_row.and_then(|r| r.spread_bps_med),
                taker_buy_ratio: flow_row.map(|r| r.taker_buy_ratio),
                bid_02: book_row.and_then(|r| r.bands.get("-0.2").copied()),
                ask_02: book_row.and_then(|r| r.bands.get("0.2").copied()),
                bid_10: book_row.and_then(|r| r.bands.get("-1.0").copied()),
                ask_10: book_row.and_then(|r| r.bands.get("1.0").copied()),
                oi_value: metrics_row.and_then(|r| r.sum_open_interest_value),
                taker_ls_ratio: metrics_row.and_then(|r| r.sum_taker_long_short_vol_ratio),
                funding_rate: funding_row.map(|r| r.funding_rate),
            })
        })
        .collect()
}

/// Calendar UTC days spanning `bars`' first through last timestamp,
/// inclusive - the set of daily JSONL files that could possibly contain a
/// snappable record for this asset's bars.
fn day_range(bars: &[Bar]) -> Vec<chrono::NaiveDate> {
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Vec::new();
    };
    let mut day = first.ts_utc.date_naive();
    let last_day = last.ts_utc.date_naive();
    let mut out = Vec::new();
    while day <= last_day {
        out.push(day);
        day = day.succ_opt().expect("day within a bounded range");
    }
    out
}

/// One JSONL file's rows, or an empty `Vec` if the file does not exist - a
/// missing day/symbol file is "no coverage that day", not an error, same
/// discipline as the rest of the micro pipeline.
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

/// Read every book/flow/metrics day file (and the one funding file) an
/// asset's Binance `symbol` could have under `{data_root}/binance-micro/`
/// for the span `bars` covers, and snap them onto `bars`. `data_root` is
/// the same root passed to `pull-binance-micro --data-root` (this function
/// joins `binance-micro` itself, mirroring that command's own layout).
pub fn load_micro_series(
    data_root: &Path,
    symbol: &str,
    bars: &[Bar],
) -> Result<Vec<Option<MicroMinute>>, String> {
    let root = data_root.join("binance-micro");
    let mut book = Vec::new();
    let mut flow = Vec::new();
    let mut metrics = Vec::new();
    for day in day_range(bars) {
        book.extend(read_jsonl::<BookMinute>(
            &root.join("book").join(symbol).join(format!("{day}.jsonl")),
        )?);
        flow.extend(read_jsonl::<FlowMinute>(
            &root.join("flow").join(symbol).join(format!("{day}.jsonl")),
        )?);
        metrics.extend(read_jsonl::<MetricsRow>(
            &root
                .join("metrics")
                .join(symbol)
                .join(format!("{day}.jsonl")),
        )?);
    }
    let funding = read_jsonl::<FundingRow>(&root.join("funding").join(format!("{symbol}.jsonl")))?;

    Ok(snap_micro(bars, &book, &flow, &metrics, &funding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn bar(ts_s: i64) -> Bar {
        Bar {
            ts_utc: Utc.timestamp_opt(ts_s, 0).unwrap(),
            asset: "TEST".into(),
            interval_s: 60,
            open: 100.0,
            high: 100.1,
            low: 99.9,
            close: 100.0,
            volume: 5.0,
            quote_volume: Some(500.0),
            trades: Some(10),
        }
    }

    fn book_minute(ts_s: i64, bid02: f64, ask02: f64, bid10: f64, ask10: f64) -> BookMinute {
        let mut bands = std::collections::BTreeMap::new();
        bands.insert("-0.2".to_string(), bid02);
        bands.insert("0.2".to_string(), ask02);
        bands.insert("-1.0".to_string(), bid10);
        bands.insert("1.0".to_string(), ask10);
        BookMinute { ts_s, bands }
    }

    fn flow_minute(ts_s: i64, spread: Option<f64>, tbr: f64) -> FlowMinute {
        FlowMinute {
            ts_s,
            spread_bps_med: spread,
            n_spread_samples: 10,
            distinct_bids: 5,
            taker_buy_ratio: tbr,
            n_trades: 100,
            notional: 10_000.0,
        }
    }

    fn metrics_row(ts_s: i64, oi_value: f64, ls_ratio: f64) -> MetricsRow {
        MetricsRow {
            ts_s,
            sum_open_interest: Some(1.0),
            sum_open_interest_value: Some(oi_value),
            count_toptrader_long_short_ratio: Some(1.0),
            sum_toptrader_long_short_ratio: Some(1.0),
            count_long_short_ratio: Some(1.0),
            sum_taker_long_short_vol_ratio: Some(ls_ratio),
        }
    }

    fn funding_row(ts_s: i64, rate: f64) -> FundingRow {
        FundingRow {
            ts_s,
            funding_interval_hours: 8.0,
            funding_rate: rate,
        }
    }

    #[test]
    fn snaps_every_source_downward_onto_the_bar() {
        let bars = vec![bar(1_000_000)];
        let book = vec![book_minute(999_990, 10.0, 20.0, 100.0, 200.0)];
        let flow = vec![flow_minute(999_995, Some(3.5), 0.6)];
        let metrics = vec![metrics_row(999_500, 5_000.0, 1.2)];
        let funding = vec![funding_row(900_000, 0.0001)];

        let out = snap_micro(&bars, &book, &flow, &metrics, &funding);
        assert_eq!(out.len(), 1);
        let m = out[0].as_ref().unwrap();
        assert_eq!(m.bid_02, Some(10.0));
        assert_eq!(m.ask_02, Some(20.0));
        assert_eq!(m.bid_10, Some(100.0));
        assert_eq!(m.ask_10, Some(200.0));
        assert_eq!(m.spread_bps, Some(3.5));
        assert_eq!(m.taker_buy_ratio, Some(0.6));
        assert_eq!(m.oi_value, Some(5_000.0));
        assert_eq!(m.taker_ls_ratio, Some(1.2));
        assert_eq!(m.funding_rate, Some(0.0001));
    }

    #[test]
    fn a_future_snapshot_is_never_used_even_if_nearest() {
        // The only book row is AFTER the bar - snapping is strictly
        // downward, so this must not be picked even though it is close.
        let bars = vec![bar(1_000_000)];
        let book = vec![book_minute(1_000_010, 10.0, 20.0, 100.0, 200.0)];
        let out = snap_micro(&bars, &book, &[], &[], &[]);
        assert_eq!(out[0].as_ref().unwrap().bid_02, None);
    }

    #[test]
    fn book_and_flow_beyond_120s_are_dropped_but_metrics_tolerates_10_minutes() {
        let bars = vec![bar(1_000_000)];
        let stale_book = vec![book_minute(1_000_000 - 121, 10.0, 20.0, 100.0, 200.0)];
        let fresh_book = vec![book_minute(1_000_000 - 120, 10.0, 20.0, 100.0, 200.0)];
        let stale_metrics = vec![metrics_row(1_000_000 - 601, 5_000.0, 1.2)];
        let fresh_metrics = vec![metrics_row(1_000_000 - 600, 5_000.0, 1.2)];

        let out_stale = snap_micro(&bars, &stale_book, &[], &stale_metrics, &[]);
        assert_eq!(out_stale[0].as_ref().unwrap().bid_02, None);
        assert_eq!(out_stale[0].as_ref().unwrap().oi_value, None);

        let out_fresh = snap_micro(&bars, &fresh_book, &[], &fresh_metrics, &[]);
        assert_eq!(out_fresh[0].as_ref().unwrap().bid_02, Some(10.0));
        assert_eq!(out_fresh[0].as_ref().unwrap().oi_value, Some(5_000.0));
    }

    #[test]
    fn a_present_but_uncomputed_metrics_field_stays_none_not_missing_row() {
        // Distinct from the staleness case above: the metrics row IS found
        // within tolerance, but Binance itself left sum_open_interest_value
        // and sum_taker_long_short_vol_ratio empty for this row (the
        // 2024-08-12 BTCUSDT shape) - that must flow through as `None` on
        // the bar, not silently become 0.0 or panic on a double-Option.
        let bars = vec![bar(1_000_000)];
        let metrics = vec![MetricsRow {
            ts_s: 999_990,
            sum_open_interest: Some(70_705.68),
            sum_open_interest_value: None,
            count_toptrader_long_short_ratio: None,
            sum_toptrader_long_short_ratio: None,
            count_long_short_ratio: None,
            sum_taker_long_short_vol_ratio: None,
        }];
        let out = snap_micro(&bars, &[], &[], &metrics, &[]);
        assert_eq!(out[0].as_ref().unwrap().oi_value, None);
        assert_eq!(out[0].as_ref().unwrap().taker_ls_ratio, None);
    }

    #[test]
    fn funding_has_no_staleness_tolerance() {
        // A funding rate from a week earlier is still "the current period's
        // rate" if nothing more recent has settled.
        let bars = vec![bar(1_000_000)];
        let funding = vec![funding_row(1_000_000 - 7 * 86_400, 0.0002)];
        let out = snap_micro(&bars, &[], &[], &[], &funding);
        assert_eq!(out[0].as_ref().unwrap().funding_rate, Some(0.0002));
    }

    #[test]
    fn a_missing_symbol_directory_yields_all_none_not_an_error() {
        let dir = std::env::temp_dir().join(format!("micro-join-test-{}", std::process::id()));
        let bars = vec![bar(1_786_492_800)]; // 2026-08-12T00:00:00Z
        let out = load_micro_series(&dir, "NOPEUSDT", &bars).unwrap();
        assert_eq!(out.len(), 1);
        let m = out[0].as_ref().unwrap();
        assert_eq!(m.bid_02, None);
        assert_eq!(m.funding_rate, None);
    }

    #[test]
    fn load_micro_series_reads_real_files_on_disk() {
        let dir = std::env::temp_dir().join(format!("micro-join-test-real-{}", std::process::id()));
        let symbol = "TESTUSDT";
        let bars = vec![bar(1_786_492_800)]; // 2026-08-12T00:00:00Z

        let book_dir = dir.join("binance-micro/book").join(symbol);
        std::fs::create_dir_all(&book_dir).unwrap();
        std::fs::write(
            book_dir.join("2026-08-12.jsonl"),
            serde_json::to_string(&book_minute(1_786_492_800, 1.0, 2.0, 3.0, 4.0)).unwrap(),
        )
        .unwrap();

        let funding_dir = dir.join("binance-micro/funding");
        std::fs::create_dir_all(&funding_dir).unwrap();
        std::fs::write(
            funding_dir.join(format!("{symbol}.jsonl")),
            serde_json::to_string(&funding_row(1_786_492_000, 0.00007)).unwrap(),
        )
        .unwrap();

        let out = load_micro_series(&dir, symbol, &bars).unwrap();
        let m = out[0].as_ref().unwrap();
        assert_eq!(m.bid_02, Some(1.0));
        assert_eq!(m.funding_rate, Some(0.00007));

        std::fs::remove_dir_all(&dir).ok();
    }
}
