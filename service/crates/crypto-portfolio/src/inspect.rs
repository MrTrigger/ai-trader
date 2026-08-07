//! Read-only evidence about the bar store and its timestamp convention.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use features_crypto::Bar;

use crate::store;

#[derive(Debug, Clone, serde::Serialize)]
pub struct InventoryRow {
    pub asset: String,
    pub interval_s: i32,
    pub rows: usize,
    pub first_ts: DateTime<Utc>,
    pub last_ts: DateTime<Utc>,
    pub content_hash: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContinuityReport {
    pub asset: String,
    pub interval_s: i32,
    pub checked: usize,
    pub breaks: usize,
    pub worst_rel_gap: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContinuityBreak {
    pub asset: String,
    pub interval_s: i32,
    pub previous_ts: DateTime<Utc>,
    pub current_ts: DateTime<Utc>,
    pub previous_close: f64,
    pub current_open: f64,
    pub relative_gap: f64,
}

impl ContinuityReport {
    pub fn ok(&self) -> bool {
        self.checked > 0 && self.breaks == 0
    }

    pub fn render(&self) -> String {
        if self.checked == 0 {
            return format!(
                "{}/{}s: too few contiguous bars to check",
                self.asset, self.interval_s
            );
        }
        let verdict = if self.ok() {
            "no price gaps above tolerance"
        } else {
            "price gaps above tolerance"
        };
        format!(
            "{}/{}s: {} adjacent pairs, {} breaks, worst {:.2}% - {verdict}",
            self.asset,
            self.interval_s,
            self.checked,
            self.breaks,
            self.worst_rel_gap * 100.0
        )
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TimestampReport {
    pub asset: String,
    pub interval_s: i32,
    pub rows: usize,
    pub off_grid: usize,
    pub duplicates: usize,
}

impl TimestampReport {
    pub fn ok(&self) -> bool {
        self.rows > 0 && self.off_grid == 0 && self.duplicates == 0
    }

    pub fn render(&self) -> String {
        format!(
            "{}/{}s: {} timestamps, {} off UTC interval grid, {} duplicates - {}",
            self.asset,
            self.interval_s,
            self.rows,
            self.off_grid,
            self.duplicates,
            if self.ok() {
                "timestamp grid OK"
            } else {
                "TIMESTAMP ERROR"
            }
        )
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AlignmentReport {
    pub asset: String,
    pub checked_days: usize,
    pub incomplete_days: usize,
    pub mismatches: usize,
    pub worst_price_relative_error: f64,
    pub mismatch_samples: Vec<AlignmentMismatch>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AlignmentMismatch {
    pub ts_utc: DateTime<Utc>,
    pub field: &'static str,
    pub daily_value: f64,
    pub hourly_aggregate: f64,
    pub relative_error: f64,
}

impl AlignmentReport {
    pub fn ok(&self) -> bool {
        self.checked_days > 0 && self.mismatches == 0
    }

    pub fn render(&self) -> String {
        format!(
            "{}: {} complete daily↔hourly aggregates, {} OHLC mismatches, {} incomplete days, worst {:.8}% - {}",
            self.asset,
            self.checked_days,
            self.mismatches,
            self.incomplete_days,
            self.worst_price_relative_error * 100.0,
            if self.ok() {
                "interval alignment OK"
            } else {
                "ALIGNMENT ERROR"
            }
        )
    }
}

fn intervals(root: &Path) -> Result<Vec<i32>, String> {
    let base = root.join("bars");
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for asset in std::fs::read_dir(&base).map_err(|error| format!("{}: {error}", base.display()))? {
        let path = asset.map_err(|error| error.to_string())?.path();
        if !path.is_dir() {
            continue;
        }
        for entry in
            std::fs::read_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?
        {
            let name = entry
                .map_err(|error| error.to_string())?
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF8 store path".to_owned())?;
            if let Some(value) = name.strip_prefix("interval_s=") {
                out.push(value.parse::<i32>().map_err(|error| error.to_string())?);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

pub fn inventory(root: &Path) -> Result<Vec<InventoryRow>, String> {
    let mut out = Vec::new();
    for interval_s in intervals(root)? {
        let bars = store::read(root, interval_s)?;
        let mut grouped: BTreeMap<&str, Vec<&Bar>> = BTreeMap::new();
        for bar in &bars {
            grouped.entry(&bar.asset).or_default().push(bar);
        }
        for (asset, rows) in grouped {
            let first_ts = rows.first().ok_or("empty store group")?.ts_utc;
            let last_ts = rows.last().ok_or("empty store group")?.ts_utc;
            let owned: Vec<_> = rows.into_iter().cloned().collect();
            out.push(InventoryRow {
                asset: asset.to_owned(),
                interval_s,
                rows: owned.len(),
                first_ts,
                last_ts,
                content_hash: store::content_hash(&owned),
            });
        }
    }
    Ok(out)
}

pub fn continuity(bars: &[Bar], tolerance: f64) -> Result<Vec<ContinuityReport>, String> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err("continuity tolerance must be finite and non-negative".into());
    }
    let mut grouped: BTreeMap<(&str, i32), Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        // Diagnostics cover the raw store, including source identifiers that
        // are intentionally rejected by the feature/planning boundary.
        grouped
            .entry((&bar.asset, bar.interval_s))
            .or_default()
            .push(bar);
    }
    let mut out = Vec::new();
    for ((asset, interval_s), mut rows) in grouped {
        rows.sort_by_key(|bar| bar.ts_utc);
        let mut checked = 0;
        let mut breaks = 0;
        let mut worst: f64 = 0.0;
        for pair in rows.windows(2) {
            if (pair[1].ts_utc - pair[0].ts_utc).num_seconds() != i64::from(interval_s) {
                continue;
            }
            checked += 1;
            if !pair[1].open.is_finite() || !pair[0].close.is_finite() || pair[0].close <= 0.0 {
                breaks += 1;
                worst = f64::INFINITY;
                continue;
            }
            let gap = (pair[1].open - pair[0].close).abs() / pair[0].close;
            worst = worst.max(gap);
            if gap > tolerance {
                breaks += 1;
            }
        }
        out.push(ContinuityReport {
            asset: asset.to_owned(),
            interval_s,
            checked,
            breaks,
            worst_rel_gap: worst,
        });
    }
    Ok(out)
}

pub fn continuity_breaks(bars: &[Bar], tolerance: f64) -> Result<Vec<ContinuityBreak>, String> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err("continuity tolerance must be finite and non-negative".into());
    }
    let mut grouped: BTreeMap<(&str, i32), Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        grouped
            .entry((&bar.asset, bar.interval_s))
            .or_default()
            .push(bar);
    }
    let mut out = Vec::new();
    for ((asset, interval_s), mut rows) in grouped {
        rows.sort_by_key(|bar| bar.ts_utc);
        for pair in rows.windows(2) {
            if (pair[1].ts_utc - pair[0].ts_utc).num_seconds() != i64::from(interval_s) {
                continue;
            }
            let gap =
                if pair[1].open.is_finite() && pair[0].close.is_finite() && pair[0].close > 0.0 {
                    (pair[1].open - pair[0].close).abs() / pair[0].close
                } else {
                    f64::INFINITY
                };
            if gap > tolerance {
                out.push(ContinuityBreak {
                    asset: asset.to_owned(),
                    interval_s,
                    previous_ts: pair[0].ts_utc,
                    current_ts: pair[1].ts_utc,
                    previous_close: pair[0].close,
                    current_open: pair[1].open,
                    relative_gap: gap,
                });
            }
        }
    }
    out.sort_by(|left, right| {
        right
            .relative_gap
            .total_cmp(&left.relative_gap)
            .then_with(|| left.asset.cmp(&right.asset))
            .then_with(|| left.current_ts.cmp(&right.current_ts))
    });
    Ok(out)
}

pub fn timestamp_grid(bars: &[Bar]) -> Vec<TimestampReport> {
    let mut grouped: BTreeMap<(&str, i32), Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        grouped
            .entry((&bar.asset, bar.interval_s))
            .or_default()
            .push(bar);
    }
    grouped
        .into_iter()
        .map(|((asset, interval_s), mut rows)| {
            rows.sort_by_key(|bar| bar.ts_utc);
            TimestampReport {
                asset: asset.to_owned(),
                interval_s,
                rows: rows.len(),
                off_grid: rows
                    .iter()
                    .filter(|bar| {
                        bar.ts_utc.timestamp().rem_euclid(i64::from(interval_s)) != 0
                            || bar.ts_utc.timestamp_subsec_nanos() != 0
                    })
                    .count(),
                duplicates: rows
                    .windows(2)
                    .filter(|pair| pair[0].ts_utc == pair[1].ts_utc)
                    .count(),
            }
        })
        .collect()
}

fn relative_error(left: f64, right: f64) -> f64 {
    (left - right).abs() / left.abs().max(right.abs()).max(f64::MIN_POSITIVE)
}

/// Compare each daily candle with the exact aggregation of its 24 hourly
/// candles. This is much stronger timestamp-convention evidence than price
/// continuity: a thin market may legitimately open away from its prior close.
pub fn daily_hourly_alignment(
    daily: &[Bar],
    hourly: &[Bar],
    tolerance: f64,
) -> Result<Vec<AlignmentReport>, String> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err("alignment tolerance must be finite and non-negative".into());
    }
    let mut hours: BTreeMap<&str, BTreeMap<DateTime<Utc>, &Bar>> = BTreeMap::new();
    for bar in hourly {
        if bar.interval_s != 3_600 {
            return Err(format!("{} has non-hourly input", bar.asset));
        }
        hours.entry(&bar.asset).or_default().insert(bar.ts_utc, bar);
    }
    let mut reports: BTreeMap<&str, AlignmentReport> = BTreeMap::new();
    for bar in daily {
        if bar.interval_s != 86_400 {
            return Err(format!("{} has non-daily input", bar.asset));
        }
        let Some(series) = hours.get(bar.asset.as_str()) else {
            continue;
        };
        let (Some(first), Some(last)) = (series.keys().next(), series.keys().next_back()) else {
            continue;
        };
        if bar.ts_utc < *first || bar.ts_utc + chrono::Duration::hours(23) > *last {
            continue;
        }
        let report = reports
            .entry(&bar.asset)
            .or_insert_with(|| AlignmentReport {
                asset: bar.asset.clone(),
                checked_days: 0,
                incomplete_days: 0,
                mismatches: 0,
                worst_price_relative_error: 0.0,
                mismatch_samples: Vec::new(),
            });
        let values = (0..24)
            .map(|hour| series.get(&(bar.ts_utc + chrono::Duration::hours(hour))))
            .collect::<Option<Vec<_>>>();
        let Some(values) = values else {
            report.incomplete_days += 1;
            continue;
        };
        report.checked_days += 1;
        let aggregated = [
            values[0].open,
            values
                .iter()
                .map(|hour| hour.high)
                .fold(f64::NEG_INFINITY, f64::max),
            values
                .iter()
                .map(|hour| hour.low)
                .fold(f64::INFINITY, f64::min),
            values[23].close,
        ];
        let errors = [bar.open, bar.high, bar.low, bar.close]
            .into_iter()
            .zip(aggregated)
            .map(|(expected, actual)| (expected, actual, relative_error(expected, actual)))
            .collect::<Vec<_>>();
        let worst = errors
            .iter()
            .map(|(_, _, error)| *error)
            .fold(0.0, f64::max);
        report.worst_price_relative_error = report.worst_price_relative_error.max(worst);
        if worst > tolerance {
            report.mismatches += 1;
            for (field, (daily_value, hourly_aggregate, error)) in
                ["open", "high", "low", "close"].into_iter().zip(errors)
            {
                if error > tolerance && report.mismatch_samples.len() < 10 {
                    report.mismatch_samples.push(AlignmentMismatch {
                        ts_utc: bar.ts_utc,
                        field,
                        daily_value,
                        hourly_aggregate,
                        relative_error: error,
                    });
                }
            }
        }
    }
    Ok(reports.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn bar(day: i64, open: f64, close: f64) -> Bar {
        let ts = "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        Bar {
            ts_utc: ts + Duration::days(day),
            asset: "BTC".into(),
            interval_s: 86_400,
            open,
            high: open.max(close),
            low: open.min(close),
            close,
            volume: 1.0,
            quote_volume: Some(100.0),
            trades: Some(1),
        }
    }

    #[test]
    fn price_continuity_ignores_real_time_gaps() {
        let rows = vec![
            bar(0, 100.0, 101.0),
            bar(1, 101.0, 102.0),
            bar(3, 50.0, 51.0),
        ];
        let report = continuity(&rows, 0.005).unwrap().remove(0);
        assert_eq!(report.checked, 1);
        assert_eq!(report.breaks, 0);
        assert!(report.ok());
    }

    #[test]
    fn price_gap_above_tolerance_is_reported() {
        let rows = vec![bar(0, 100.0, 101.0), bar(1, 103.0, 104.0)];
        let report = continuity(&rows, 0.005).unwrap().remove(0);
        assert_eq!(report.breaks, 1);
        assert!(!report.ok());
    }

    #[test]
    fn break_details_name_both_bars_and_are_worst_first() {
        let rows = vec![
            bar(0, 100.0, 101.0),
            bar(1, 103.0, 104.0),
            bar(2, 110.0, 111.0),
        ];
        let breaks = continuity_breaks(&rows, 0.005).unwrap();
        assert_eq!(breaks.len(), 2);
        assert_eq!(breaks[0].current_ts.date_naive().to_string(), "2025-01-03");
        assert_eq!(breaks[1].previous_ts.date_naive().to_string(), "2025-01-01");
    }

    #[test]
    fn interval_aggregation_is_stronger_than_close_to_next_open() {
        let start = "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let hours = (0..24)
            .map(|hour| Bar {
                ts_utc: start + Duration::hours(hour),
                asset: "BTC".into(),
                interval_s: 3_600,
                open: 100.0 + hour as f64,
                high: 101.0 + hour as f64,
                low: 99.0 + hour as f64,
                close: 100.5 + hour as f64,
                volume: 1.0,
                quote_volume: Some(100.0),
                trades: Some(1),
            })
            .collect::<Vec<_>>();
        let daily = Bar {
            ts_utc: start,
            asset: "BTC".into(),
            interval_s: 86_400,
            open: 100.0,
            high: 124.0,
            low: 99.0,
            close: 123.5,
            volume: 24.0,
            quote_volume: Some(2_400.0),
            trades: Some(24),
        };
        let report = daily_hourly_alignment(std::slice::from_ref(&daily), &hours, 1e-12)
            .unwrap()
            .remove(0);
        assert!(report.ok());
        let mut shifted = daily;
        shifted.open = 101.0;
        assert_eq!(
            daily_hourly_alignment(&[shifted], &hours, 1e-12).unwrap()[0].mismatches,
            1
        );
    }

    #[test]
    fn timestamp_grid_catches_close_stamped_rows() {
        let mut row = bar(0, 100.0, 101.0);
        row.ts_utc += Duration::seconds(86_399);
        let report = timestamp_grid(&[row]).remove(0);
        assert_eq!(report.off_grid, 1);
        assert!(!report.ok());
    }
}
