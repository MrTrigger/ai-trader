//! Reader for the existing partitioned Parquet bar store.
//!
//! Rust takes the store over in place; no Python conversion step and no second
//! copy of 500MB of market data. Every row is immediately normalised into the
//! canonical [`features_crypto::Bar`] type and validated by the feature crate.

use std::path::{Path, PathBuf};

use arrow_array::{
    Array, ArrayRef, Float64Array, Int32Array, Int64Array, LargeStringArray, RecordBatch,
    StringArray, StringViewArray, TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::{TimeZone, Utc};
use features_crypto::Bar;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use sha2::{Digest, Sha256};

fn files_under(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            files_under(&path, out)?;
        } else if path.extension().is_some_and(|v| v == "parquet") {
            out.push(path);
        }
    }
    Ok(())
}

fn col<'a, T: Array + 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T, String> {
    let i = batch
        .schema()
        .index_of(name)
        .map_err(|_| format!("Parquet batch lacks {name}"))?;
    batch
        .column(i)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| format!("Parquet column {name} has unexpected type"))
}

fn optional_f64<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<Option<&'a Float64Array>, String> {
    let Ok(i) = batch.schema().index_of(name) else {
        return Ok(None);
    };
    batch
        .column(i)
        .as_any()
        .downcast_ref::<Float64Array>()
        .map(Some)
        .ok_or_else(|| format!("Parquet column {name} has unexpected type"))
}

fn optional_i64<'a>(batch: &'a RecordBatch, name: &str) -> Result<Option<&'a Int64Array>, String> {
    let Ok(i) = batch.schema().index_of(name) else {
        return Ok(None);
    };
    batch
        .column(i)
        .as_any()
        .downcast_ref::<Int64Array>()
        .map(Some)
        .ok_or_else(|| format!("Parquet column {name} has unexpected type"))
}

fn read_batch(batch: &RecordBatch, out: &mut Vec<Bar>) -> Result<(), String> {
    let ts = col::<TimestampMicrosecondArray>(batch, "ts_utc")?;
    let asset_i = batch
        .schema()
        .index_of("asset")
        .map_err(|_| "Parquet batch lacks asset")?;
    let asset = batch.column(asset_i);
    let interval = col::<Int32Array>(batch, "interval_s")?;
    let open = col::<Float64Array>(batch, "open")?;
    let high = col::<Float64Array>(batch, "high")?;
    let low = col::<Float64Array>(batch, "low")?;
    let close = col::<Float64Array>(batch, "close")?;
    let volume = col::<Float64Array>(batch, "volume")?;
    let quote = optional_f64(batch, "quote_volume")?;
    let trades = optional_i64(batch, "trades")?;
    for i in 0..batch.num_rows() {
        let stamp = Utc
            .timestamp_micros(ts.value(i))
            .single()
            .ok_or_else(|| format!("invalid microsecond timestamp {}", ts.value(i)))?;
        let asset = if let Some(values) = asset.as_any().downcast_ref::<StringArray>() {
            values.value(i)
        } else if let Some(values) = asset.as_any().downcast_ref::<LargeStringArray>() {
            values.value(i)
        } else if let Some(values) = asset.as_any().downcast_ref::<StringViewArray>() {
            values.value(i)
        } else {
            return Err(format!(
                "Parquet column asset has unexpected type {:?}",
                asset.data_type()
            ));
        };
        out.push(Bar {
            ts_utc: stamp,
            asset: asset.to_owned(),
            interval_s: interval.value(i),
            open: open.value(i),
            high: high.value(i),
            low: low.value(i),
            close: close.value(i),
            volume: volume.value(i),
            quote_volume: quote.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
            trades: trades.and_then(|a| (!a.is_null(i)).then(|| a.value(i))),
        });
    }
    Ok(())
}

fn read_files(mut files: Vec<PathBuf>, interval_s: i32) -> Result<Vec<Bar>, String> {
    let interval_dir = format!("interval_s={interval_s}");
    files.retain(|p| {
        p.parent()
            .is_some_and(|d| d.file_name().is_some_and(|n| n == interval_dir.as_str()))
    });
    files.sort();
    let mut bars = Vec::new();
    for path in files {
        let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let reader = builder
            .with_batch_size(8192)
            .build()
            .map_err(|e| e.to_string())?;
        for batch in reader {
            read_batch(
                &batch.map_err(|e| format!("{}: {e}", path.display()))?,
                &mut bars,
            )?;
        }
    }
    // Lexically sorted partition paths are already (asset, time) ordered and
    // each writer orders rows within its partition. Avoid sorting millions of
    // bars on every read; retain a defensive fallback for a foreign partition
    // that violates the store contract.
    if bars.windows(2).any(|pair| {
        pair[0].asset > pair[1].asset
            || (pair[0].asset == pair[1].asset && pair[0].ts_utc > pair[1].ts_utc)
    }) {
        bars.sort_by(|a, b| a.asset.cmp(&b.asset).then_with(|| a.ts_utc.cmp(&b.ts_utc)));
    }
    Ok(bars)
}

pub fn read(root: &Path, interval_s: i32) -> Result<Vec<Bar>, String> {
    let mut files = Vec::new();
    files_under(&root.join("bars"), &mut files)?;
    read_files(files, interval_s)
}

/// Read a single asset without scanning every Parquet partition. Diagnostics
/// and one-name investigations should not pay the multi-gigabyte store cost.
pub fn read_asset(root: &Path, interval_s: i32, asset: &str) -> Result<Vec<Bar>, String> {
    let mut files = Vec::new();
    files_under(
        &root.join("bars").join(format!("asset={asset}")),
        &mut files,
    )?;
    read_files(files, interval_s)
}

pub fn known_assets(root: &Path) -> Result<Vec<String>, String> {
    let path = root.join("bars");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut assets = std::fs::read_dir(&path)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let asset = name.strip_prefix("asset=")?.to_owned();
            // The archive loader can land venue oddities in the store - one
            // Binance listing has a CJK ticker - and a refresh that dies on
            // them kills the nightly cycle. Skip what Bar::validate would
            // refuse; the canonical-id rule lives at write time, this mirror
            // of it lives at enumerate time.
            (!asset.is_empty()
                && asset.len() <= 20
                && asset
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
            .then_some(asset)
        })
        .collect::<Vec<_>>();
    assets.sort();
    assets.dedup();
    Ok(assets)
}

pub fn funding_listings(
    root: &Path,
) -> Result<std::collections::BTreeMap<String, chrono::NaiveDate>, String> {
    let path = root.join("funding").join("binance_um.parquet");
    if !path.exists() {
        return Ok(Default::default());
    }
    let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let mut listings = std::collections::BTreeMap::new();
    for batch in reader {
        let batch = batch.map_err(|e| e.to_string())?;
        let day = col::<TimestampMicrosecondArray>(&batch, "day")?;
        let asset_i = batch
            .schema()
            .index_of("asset")
            .map_err(|_| "funding Parquet lacks asset")?;
        let asset = batch.column(asset_i);
        for i in 0..batch.num_rows() {
            let name = if let Some(v) = asset.as_any().downcast_ref::<LargeStringArray>() {
                v.value(i)
            } else if let Some(v) = asset.as_any().downcast_ref::<StringArray>() {
                v.value(i)
            } else if let Some(v) = asset.as_any().downcast_ref::<StringViewArray>() {
                v.value(i)
            } else {
                return Err(format!(
                    "funding asset has unexpected type {:?}",
                    asset.data_type()
                ));
            };
            let date = Utc
                .timestamp_micros(day.value(i))
                .single()
                .ok_or("invalid funding timestamp")?
                .date_naive();
            listings
                .entry(name.to_owned())
                .and_modify(|old| {
                    if date < *old {
                        *old = date;
                    }
                })
                .or_insert(date);
        }
    }
    Ok(listings)
}

fn partition(bar: &Bar) -> String {
    if bar.interval_s >= 86_400 {
        bar.ts_utc.format("%Y").to_string()
    } else {
        bar.ts_utc.format("%Y-%m").to_string()
    }
}

fn batch(bars: &[Bar]) -> Result<RecordBatch, String> {
    let schema = std::sync::Arc::new(Schema::new(vec![
        Field::new(
            "ts_utc",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("asset", DataType::LargeUtf8, false),
        Field::new("interval_s", DataType::Int32, false),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Float64, false),
        Field::new("quote_volume", DataType::Float64, true),
        Field::new("trades", DataType::Int64, true),
    ]));
    let columns: Vec<ArrayRef> = vec![
        std::sync::Arc::new(
            TimestampMicrosecondArray::from(
                bars.iter()
                    .map(|b| b.ts_utc.timestamp_micros())
                    .collect::<Vec<_>>(),
            )
            .with_timezone("UTC"),
        ),
        std::sync::Arc::new(LargeStringArray::from_iter_values(
            bars.iter().map(|b| b.asset.as_str()),
        )),
        std::sync::Arc::new(Int32Array::from(
            bars.iter().map(|b| b.interval_s).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            bars.iter().map(|b| b.open).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            bars.iter().map(|b| b.high).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            bars.iter().map(|b| b.low).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            bars.iter().map(|b| b.close).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            bars.iter().map(|b| b.volume).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            bars.iter().map(|b| b.quote_volume).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Int64Array::from(
            bars.iter().map(|b| b.trades).collect::<Vec<_>>(),
        )),
    ];
    RecordBatch::try_new(schema, columns).map_err(|e| e.to_string())
}

fn read_one(path: &Path) -> Result<Vec<Bar>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for batch in reader {
        read_batch(&batch.map_err(|e| e.to_string())?, &mut out)?;
    }
    Ok(out)
}

pub fn write(root: &Path, incoming: &[Bar]) -> Result<(), String> {
    let mut groups: std::collections::BTreeMap<(String, i32, String), Vec<Bar>> =
        std::collections::BTreeMap::new();
    for bar in incoming {
        bar.validate()?;
        groups
            .entry((bar.asset.clone(), bar.interval_s, partition(bar)))
            .or_default()
            .push(bar.clone());
    }
    for ((asset, interval, key), fresh) in groups {
        let dir = root
            .join("bars")
            .join(format!("asset={asset}"))
            .join(format!("interval_s={interval}"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{key}.parquet"));
        let mut merged: std::collections::BTreeMap<_, _> = read_one(&path)?
            .into_iter()
            .map(|b| (b.ts_utc, b))
            .collect();
        for bar in fresh {
            merged.insert(bar.ts_utc, bar);
        }
        let rows: Vec<_> = merged.into_values().collect();
        let record = batch(&rows)?;
        let temp = path.with_extension("parquet.tmp");
        let file = std::fs::File::create(&temp).map_err(|e| e.to_string())?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .build();
        let mut writer =
            ArrowWriter::try_new(file, record.schema(), Some(props)).map_err(|e| e.to_string())?;
        writer.write(&record).map_err(|e| e.to_string())?;
        writer.close().map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Stable content hash for Rust plans. It intentionally replaces Polars'
/// implementation-specific `hash_rows`: inputs must remain reproducible after
/// Polars and Python leave the runtime.
pub fn content_hash(bars: &[Bar]) -> String {
    content_hash_sets(&[bars])
}

/// Hash several input intervals without concatenating or duplicating their
/// potentially multi-gigabyte buffers.
pub fn content_hash_sets(sets: &[&[Bar]]) -> String {
    let mut hash = Sha256::new();
    for bars in sets {
        for bar in *bars {
            hash.update(bar.asset.as_bytes());
            hash.update(bar.interval_s.to_le_bytes());
            hash.update(bar.ts_utc.timestamp_micros().to_le_bytes());
            for value in [bar.open, bar.high, bar.low, bar.close, bar.volume] {
                hash.update(value.to_bits().to_le_bytes());
            }
        }
    }
    format!("{:x}", hash.finalize())[..16].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_existing_python_written_store_when_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../data");
        if !root.join("bars").exists() {
            return;
        }
        let bars = read(&root, 86_400).unwrap();
        assert!(!bars.is_empty());
        assert!(bars.iter().any(|b| b.asset == "BTC"));
        assert_eq!(content_hash(&bars).len(), 16);
        let btc = read_asset(&root, 86_400, "BTC").unwrap();
        assert_eq!(
            btc.len(),
            bars.iter().filter(|bar| bar.asset == "BTC").count()
        );
        assert!(btc.iter().all(|bar| bar.asset == "BTC"));
    }
}
