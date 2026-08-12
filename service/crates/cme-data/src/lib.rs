//! Shared readers for archived CME bar data.
//!
//! Provider/storage decoding ends here. Strategy and feature crates consume
//! only the validated daily observations below; they never read Parquet or a
//! foreign research repository directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use arrow_array::{
    Array, Float64Array, Int32Array, LargeStringArray, StringArray, StringViewArray,
    TimestampMicrosecondArray,
};
use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::Europe::Stockholm;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use time::{Date, Month, OffsetDateTime};

fn stockholm_date_if_completed_by_close(
    opened_micros: i64,
    interval_seconds: i32,
) -> Result<Option<Date>, String> {
    let opened = chrono::DateTime::<Utc>::from_timestamp_micros(opened_micros)
        .ok_or_else(|| format!("timestamp {opened_micros} is invalid"))?;
    let completed = opened + chrono::Duration::seconds(interval_seconds.into());
    let local = completed.with_timezone(&Stockholm);
    let cutoff = Stockholm
        .with_ymd_and_hms(local.year(), local.month(), local.day(), 17, 30, 0)
        .single()
        .ok_or_else(|| format!("invalid Stockholm close date {}", local.date_naive()))?;
    if local > cutoff {
        return Ok(None);
    }
    let month =
        Month::try_from(local.month() as u8).map_err(|error| format!("invalid month: {error}"))?;
    Date::from_calendar_date(local.year(), month, local.day() as u8)
        .map(Some)
        .map_err(|error| format!("invalid Stockholm date: {error}"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyClose {
    pub date: Date,
    pub close: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyCloseSeries {
    pub symbol: String,
    pub interval_seconds: i32,
    pub source_root: PathBuf,
    pub source_files: usize,
    pub source_rows: usize,
    pub observations: Vec<DailyClose>,
}

/// Stitch contract-size aliases that quote the same underlying price series.
///
/// This is intended for archive symbol migrations such as NQ to MNQ. Inputs
/// must share their interval and source root. The final overlap is the explicit
/// migration point: the old alias owns history through that date, and its close
/// fixes one multiplicative adjustment applied only to later observations from
/// the new alias. Earlier output therefore never changes because of a future
/// scale anchor. Implausibly large level adjustments are rejected.
pub fn stitch_daily_close_aliases(
    symbol: &str,
    aliases: &[DailyCloseSeries],
) -> Result<DailyCloseSeries, String> {
    let Some(first) = aliases.first() else {
        return Err("at least one daily-close alias is required".into());
    };
    if symbol.trim().is_empty() {
        return Err("stitched daily-close symbol must be non-empty".into());
    }
    if aliases.iter().any(|series| {
        series.interval_seconds != first.interval_seconds || series.source_root != first.source_root
    }) {
        return Err("daily-close aliases must share interval and source root".into());
    }
    let mut observations: BTreeMap<Date, f64> = BTreeMap::new();
    for (alias_index, series) in aliases.iter().enumerate() {
        let migration = if alias_index == 0 {
            None
        } else {
            let (date, existing, incoming) = series
                .observations
                .iter()
                .filter_map(|bar| {
                    observations
                        .get(&bar.date)
                        .map(|close| (bar.date, *close, bar.close))
                })
                .next_back()
                .ok_or_else(|| {
                    format!(
                        "daily-close alias {:?} has no overlap for a causal level adjustment",
                        series.symbol
                    )
                })?;
            let scale = existing / incoming;
            if !scale.is_finite() || !(0.75..=1.25).contains(&scale) {
                return Err(format!(
                    "daily-close alias {:?} requires implausible scale {scale}",
                    series.symbol
                ));
            }
            Some((date, scale))
        };
        for bar in &series.observations {
            let (migration_date, scale) = migration.unwrap_or((bar.date, 1.0));
            if alias_index > 0 && bar.date <= migration_date {
                continue;
            }
            let adjusted_close = bar.close * scale;
            observations.insert(bar.date, adjusted_close);
        }
    }
    let observations = observations
        .into_iter()
        .map(|(date, close)| DailyClose { date, close })
        .collect::<Vec<_>>();
    if observations.len() < 252 {
        return Err(format!(
            "{symbol} has only {} stitched daily observations",
            observations.len()
        ));
    }
    Ok(DailyCloseSeries {
        symbol: symbol.into(),
        interval_seconds: first.interval_seconds,
        source_root: first.source_root.clone(),
        source_files: aliases.iter().map(|series| series.source_files).sum(),
        source_rows: aliases.iter().map(|series| series.source_rows).sum(),
        observations,
    })
}

fn string_at(array: &dyn Array, index: usize) -> Option<&str> {
    if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
        (!values.is_null(index)).then(|| values.value(index))
    } else if let Some(values) = array.as_any().downcast_ref::<LargeStringArray>() {
        (!values.is_null(index)).then(|| values.value(index))
    } else if let Some(values) = array.as_any().downcast_ref::<StringViewArray>() {
        (!values.is_null(index)).then(|| values.value(index))
    } else {
        None
    }
}

/// Load the last completed bar close in each UTC calendar day.
///
/// Consumers must still enforce their own availability rule. The Stockholm
/// feature contract uses only a daily close whose UTC date is strictly before
/// the decision date, avoiding any dependence on same-day US-session timing.
pub fn load_daily_closes(
    root: &Path,
    symbol: &str,
    interval_seconds: i32,
) -> Result<DailyCloseSeries, String> {
    if symbol.trim().is_empty() || interval_seconds <= 0 {
        return Err("CME symbol and interval must be non-empty and positive".into());
    }
    let directory = root
        .join("bars")
        .join(format!("symbol={symbol}"))
        .join(format!("interval_s={interval_seconds}"));
    let mut files = std::fs::read_dir(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "parquet")
        })
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "{} contains no Parquet partitions",
            directory.display()
        ));
    }

    let mut source_rows = 0_usize;
    let mut daily: BTreeMap<Date, (i64, f64)> = BTreeMap::new();
    for path in &files {
        let file =
            std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .with_batch_size(16_384)
            .build()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        for batch in reader {
            let batch = batch.map_err(|error| format!("{}: {error}", path.display()))?;
            let timestamp = batch
                .column_by_name("ts_utc")
                .and_then(|column| column.as_any().downcast_ref::<TimestampMicrosecondArray>())
                .ok_or_else(|| format!("{} lacks microsecond ts_utc", path.display()))?;
            let row_symbol = batch
                .column_by_name("symbol")
                .ok_or_else(|| format!("{} lacks symbol", path.display()))?;
            let interval = batch
                .column_by_name("interval_s")
                .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
                .ok_or_else(|| format!("{} lacks int32 interval_s", path.display()))?;
            let close = batch
                .column_by_name("close")
                .and_then(|column| column.as_any().downcast_ref::<Float64Array>())
                .ok_or_else(|| format!("{} lacks float64 close", path.display()))?;
            for index in 0..batch.num_rows() {
                source_rows += 1;
                if timestamp.is_null(index)
                    || interval.is_null(index)
                    || close.is_null(index)
                    || string_at(row_symbol.as_ref(), index) != Some(symbol)
                    || interval.value(index) != interval_seconds
                {
                    return Err(format!(
                        "{} contains a row outside its declared contract",
                        path.display()
                    ));
                }
                let value = close.value(index);
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("{} contains an invalid close", path.display()));
                }
                let micros = timestamp.value(index);
                let instant = OffsetDateTime::from_unix_timestamp_nanos(micros as i128 * 1_000)
                    .map_err(|error| format!("{} timestamp {micros}: {error}", path.display()))?;
                let slot = daily.entry(instant.date()).or_insert((micros, value));
                if micros > slot.0 {
                    *slot = (micros, value);
                }
            }
        }
    }
    let observations = daily
        .into_iter()
        .map(|(date, (_, close))| DailyClose { date, close })
        .collect::<Vec<_>>();
    if observations.len() < 252 {
        return Err(format!(
            "{symbol} has only {} completed UTC days",
            observations.len()
        ));
    }
    Ok(DailyCloseSeries {
        symbol: symbol.into(),
        interval_seconds,
        source_root: root.to_owned(),
        source_files: files.len(),
        source_rows,
        observations,
    })
}

/// Load the last futures bar completed by the Nasdaq Stockholm cash close on
/// each local trading date.
///
/// Archived timestamps identify the bar open. A five-minute bar stamped
/// 17:25 Stockholm is therefore admissible at 17:30, while the 17:30 bar is
/// not. The timezone conversion uses the exchange's historical CET/CEST rules.
/// Consumers may use the observation on the same Stockholm date only for a
/// decision made after the cash close and executed no earlier than the next
/// session.
pub fn load_daily_closes_at_stockholm_close(
    root: &Path,
    symbol: &str,
    interval_seconds: i32,
) -> Result<DailyCloseSeries, String> {
    if symbol.trim().is_empty() || interval_seconds <= 0 {
        return Err("CME symbol and interval must be non-empty and positive".into());
    }
    let directory = root
        .join("bars")
        .join(format!("symbol={symbol}"))
        .join(format!("interval_s={interval_seconds}"));
    let mut files = std::fs::read_dir(&directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "parquet")
        })
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "{} contains no Parquet partitions",
            directory.display()
        ));
    }

    let mut source_rows = 0_usize;
    let mut daily: BTreeMap<Date, (i64, f64)> = BTreeMap::new();
    for path in &files {
        let file =
            std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|error| format!("{}: {error}", path.display()))?
            .with_batch_size(16_384)
            .build()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        for batch in reader {
            let batch = batch.map_err(|error| format!("{}: {error}", path.display()))?;
            let timestamp = batch
                .column_by_name("ts_utc")
                .and_then(|column| column.as_any().downcast_ref::<TimestampMicrosecondArray>())
                .ok_or_else(|| format!("{} lacks microsecond ts_utc", path.display()))?;
            let row_symbol = batch
                .column_by_name("symbol")
                .ok_or_else(|| format!("{} lacks symbol", path.display()))?;
            let interval = batch
                .column_by_name("interval_s")
                .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
                .ok_or_else(|| format!("{} lacks int32 interval_s", path.display()))?;
            let close = batch
                .column_by_name("close")
                .and_then(|column| column.as_any().downcast_ref::<Float64Array>())
                .ok_or_else(|| format!("{} lacks float64 close", path.display()))?;
            for index in 0..batch.num_rows() {
                source_rows += 1;
                if timestamp.is_null(index)
                    || interval.is_null(index)
                    || close.is_null(index)
                    || string_at(row_symbol.as_ref(), index) != Some(symbol)
                    || interval.value(index) != interval_seconds
                {
                    return Err(format!(
                        "{} contains a row outside its declared contract",
                        path.display()
                    ));
                }
                let value = close.value(index);
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("{} contains an invalid close", path.display()));
                }
                let micros = timestamp.value(index);
                let Some(date) = stockholm_date_if_completed_by_close(micros, interval_seconds)
                    .map_err(|error| format!("{}: {error}", path.display()))?
                else {
                    continue;
                };
                let slot = daily.entry(date).or_insert((micros, value));
                if micros > slot.0 {
                    *slot = (micros, value);
                }
            }
        }
    }
    let observations = daily
        .into_iter()
        .map(|(date, (_, close))| DailyClose { date, close })
        .collect::<Vec<_>>();
    if observations.len() < 252 {
        return Err(format!(
            "{symbol} has only {} Stockholm-close observations",
            observations.len()
        ));
    }
    Ok(DailyCloseSeries {
        symbol: symbol.into(),
        interval_seconds,
        source_root: root.to_owned(),
        source_files: files.len(),
        source_rows,
        observations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .unwrap()
            .timestamp_micros()
    }

    #[test]
    fn stockholm_close_cutoff_handles_completed_bars_and_dst() {
        let summer = Date::from_calendar_date(2025, Month::July, 1).unwrap();
        assert_eq!(
            stockholm_date_if_completed_by_close(micros(2025, 7, 1, 15, 25), 300).unwrap(),
            Some(summer),
            "15:25 UTC opens a bar completed exactly at 17:30 CEST"
        );
        assert_eq!(
            stockholm_date_if_completed_by_close(micros(2025, 7, 1, 15, 30), 300).unwrap(),
            None,
            "the bar completed at 17:35 CEST is not known at the cash close"
        );

        let winter = Date::from_calendar_date(2025, Month::January, 2).unwrap();
        assert_eq!(
            stockholm_date_if_completed_by_close(micros(2025, 1, 2, 16, 25), 300).unwrap(),
            Some(winter),
            "16:25 UTC opens a bar completed exactly at 17:30 CET"
        );
    }

    #[test]
    fn contract_size_aliases_stitch_with_a_validated_overlap() {
        let start = Date::from_calendar_date(2020, Month::January, 1).unwrap();
        let series = |symbol: &str, offset: i64, count: i64| DailyCloseSeries {
            symbol: symbol.into(),
            interval_seconds: 300,
            source_root: PathBuf::from("archive"),
            source_files: 1,
            source_rows: count as usize,
            observations: (offset..offset + count)
                .map(|index| DailyClose {
                    date: start + time::Duration::days(index),
                    close: 10_000.0 + index as f64,
                })
                .collect(),
        };
        let stitched =
            stitch_daily_close_aliases("NQ", &[series("NQ", 0, 260), series("MNQ", 250, 260)])
                .unwrap();
        assert_eq!(stitched.symbol, "NQ");
        assert_eq!(stitched.observations.len(), 510);
        assert_eq!(stitched.source_files, 2);
        assert_eq!(stitched.source_rows, 520);

        let mut bad = series("MNQ", 250, 260);
        bad.observations[9].close *= 2.0;
        assert!(
            stitch_daily_close_aliases("NQ", &[series("NQ", 0, 260), bad])
                .unwrap_err()
                .contains("implausible")
        );
    }
}
