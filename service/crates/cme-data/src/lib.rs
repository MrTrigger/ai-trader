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
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};

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

fn string_at<'a>(array: &'a dyn Array, index: usize) -> Option<&'a str> {
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
