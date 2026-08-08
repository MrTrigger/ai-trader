//! Realised daily funding rates, read from the store.
//!
//! One file, `funding/binance_um.parquet`, produced by the recovered
//! `pull_funding.py`: per (asset, day), the sum of that day's 8h funding
//! intervals as a daily rate. Binance USD-M rates stand in for Hyperliquid's
//! own - the harness called this a proxy and so does the plan disclosure.
//!
//! A missing file is an empty table, not an error: funding features read 0.0
//! and the backtest simply carries no funding P&L, which is exactly what a
//! store without funding data can honestly claim.

use std::collections::BTreeMap;
use std::path::Path;

use arrow_array::{Array, Float64Array, LargeStringArray, StringArray, StringViewArray, TimestampMicrosecondArray};
use chrono::DateTime;
use features_crypto::FundingTable;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

pub fn load(root: &Path) -> Result<FundingTable, String> {
    let path = root.join("funding").join("binance_um.parquet");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let mut table: FundingTable = BTreeMap::new();
    for batch in reader {
        let batch = batch.map_err(|e| e.to_string())?;
        // polars picks its own arrow string encoding; accept the three it uses.
        let asset_col = batch
            .column_by_name("asset")
            .ok_or("funding parquet: no asset column")?;
        let asset_at = |i: usize| -> Option<&str> {
            let a = asset_col.as_any();
            if let Some(v) = a.downcast_ref::<StringArray>() {
                (!v.is_null(i)).then(|| v.value(i))
            } else if let Some(v) = a.downcast_ref::<LargeStringArray>() {
                (!v.is_null(i)).then(|| v.value(i))
            } else if let Some(v) = a.downcast_ref::<StringViewArray>() {
                (!v.is_null(i)).then(|| v.value(i))
            } else {
                None
            }
        };
        let days = batch
            .column_by_name("day")
            .and_then(|c| c.as_any().downcast_ref::<TimestampMicrosecondArray>())
            .ok_or("funding parquet: no day column")?;
        let rates = batch
            .column_by_name("daily_rate")
            .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
            .ok_or("funding parquet: no daily_rate column")?;
        for i in 0..batch.num_rows() {
            let Some(asset) = asset_at(i) else { continue };
            if days.is_null(i) || rates.is_null(i) {
                continue;
            }
            let day = DateTime::from_timestamp_micros(days.value(i))
                .ok_or("funding parquet: day out of range")?
                .date_naive();
            table
                .entry(asset.to_owned())
                .or_default()
                .insert(day, rates.value(i));
        }
    }
    Ok(table)
}
