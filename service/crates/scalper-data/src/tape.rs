//! The raw Binance UM aggTrades tape store - `docs/scalper-research.md`
//! Amendment 5, §5.1.
//!
//! `binance_micro.rs` downloads the same daily aggTrades archives and
//! reduces each to one `FlowMinute` per minute; the trades themselves were
//! never kept. Program 2 needs them twice over - for the twelve fs-5 tick
//! features (windows of 10 s / 30 s / 5 min ending strictly before a bar's
//! close) and for the maker fill model (did any trade print strictly
//! through our price inside the rest window) - so this module stores each
//! symbol-day losslessly as
//! `{data-root}/binance-micro/tape/{SYMBOL}/{YYYY-MM-DD}.parquet` with the
//! four columns Amendment 5 names (`ts_ms`, `price`, `qty`,
//! `is_buyer_maker`), in archive row order, zstd-compressed.
//!
//! Rows are parsed by the very same `binance_micro::parse_agg_trade_row`
//! the `FlowMinute` reduction uses, so the tape and the flow store can
//! never disagree about a trade. Nothing is filtered, deduplicated, sorted
//! or rounded on the way in: "lossless" means a stored day, read back, is
//! the archive's rows.
//!
//! The manifest (`.../tape/manifest.json`) records, per symbol, every day
//! that was fetched and stored (with its row count) and every day the
//! archive answered 404 - the run-5 record reports both, and a 404 day is
//! survivorship truth (not published / not listed), never an error and
//! never a fabricated empty file.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use arrow_array::{Array, ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use chrono::NaiveDate;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

/// One aggTrade as stored: Binance's `transact_time` in ms, `price`,
/// `quantity`, and `is_buyer_maker` (true = the taker SOLD - a bid-side
/// print; false = the taker bought at the ask).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TapeTrade {
    pub ts_ms: i64,
    pub price: f64,
    pub qty: f64,
    pub is_buyer_maker: bool,
}

impl TapeTrade {
    /// A taker buy (aggressor lifted the ask).
    pub fn is_buy(&self) -> bool {
        !self.is_buyer_maker
    }
}

pub fn tape_root(data_root: &Path) -> PathBuf {
    data_root.join("binance-micro").join("tape")
}

pub fn day_path(tape_root: &Path, symbol: &str, day: NaiveDate) -> PathBuf {
    tape_root.join(symbol).join(format!("{day}.parquet"))
}

pub fn manifest_path(tape_root: &Path) -> PathBuf {
    tape_root.join("manifest.json")
}

/// Parse one daily aggTrades zip into its rows, streaming the CSV off the
/// zip entry (never `read_to_string` - busy symbols run to hundreds of MB
/// decompressed).
pub fn parse_agg_trades_zip_raw(bytes: &[u8], symbol: &str) -> Result<Vec<TapeTrade>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{symbol}: bad zip: {e}"))?;
    let entry = archive
        .by_index(0)
        .map_err(|e| format!("{symbol}: empty zip: {e}"))?;
    let reader = BufReader::new(entry);
    parse_agg_trades_lines_raw(reader.lines(), symbol)
}

pub(crate) fn parse_agg_trades_lines_raw<I>(
    lines: I,
    symbol: &str,
) -> Result<Vec<TapeTrade>, String>
where
    I: Iterator<Item = std::io::Result<String>>,
{
    let mut out = Vec::new();
    for line in lines {
        let line = line.map_err(|e| format!("{symbol}: {e}"))?;
        if let Some((ts_ms, price, qty, is_buyer_maker)) =
            crate::binance_micro::parse_agg_trade_row(&line, symbol)?
        {
            out.push(TapeTrade {
                ts_ms,
                price,
                qty,
                is_buyer_maker,
            });
        }
    }
    Ok(out)
}

fn schema() -> std::sync::Arc<Schema> {
    std::sync::Arc::new(Schema::new(vec![
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("price", DataType::Float64, false),
        Field::new("qty", DataType::Float64, false),
        Field::new("is_buyer_maker", DataType::Boolean, false),
    ]))
}

/// Write one symbol-day atomically (tmp sibling + rename). An empty slice
/// is a legitimate day (a listed symbol with a published archive that
/// happens to have no rows) and writes an empty file - distinct from a 404,
/// which writes nothing and is recorded in the manifest instead.
pub fn write_day(path: &Path, trades: &[TapeTrade]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let columns: Vec<ArrayRef> = vec![
        std::sync::Arc::new(Int64Array::from(
            trades.iter().map(|t| t.ts_ms).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            trades.iter().map(|t| t.price).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(Float64Array::from(
            trades.iter().map(|t| t.qty).collect::<Vec<_>>(),
        )),
        std::sync::Arc::new(BooleanArray::from(
            trades.iter().map(|t| t.is_buyer_maker).collect::<Vec<_>>(),
        )),
    ];
    let record = RecordBatch::try_new(schema(), columns).map_err(|e| e.to_string())?;
    let tmp = crate::tmp_sibling(path);
    {
        let file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .build();
        let mut writer =
            ArrowWriter::try_new(file, record.schema(), Some(props)).map_err(|e| e.to_string())?;
        writer.write(&record).map_err(|e| e.to_string())?;
        writer.close().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read one symbol-day back, in stored (archive) order.
pub fn read_day(path: &Path) -> Result<Vec<TapeTrade>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .build()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| format!("{}: {e}", path.display()))?;
        let ts = col::<Int64Array>(&batch, "ts_ms", path)?;
        let price = col::<Float64Array>(&batch, "price", path)?;
        let qty = col::<Float64Array>(&batch, "qty", path)?;
        let ibm = col::<BooleanArray>(&batch, "is_buyer_maker", path)?;
        out.reserve(batch.num_rows());
        for i in 0..batch.num_rows() {
            out.push(TapeTrade {
                ts_ms: ts.value(i),
                price: price.value(i),
                qty: qty.value(i),
                is_buyer_maker: ibm.value(i),
            });
        }
    }
    Ok(out)
}

fn col<'a, T: Array + 'static>(
    batch: &'a RecordBatch,
    name: &str,
    path: &Path,
) -> Result<&'a T, String> {
    let i = batch
        .schema()
        .index_of(name)
        .map_err(|_| format!("{}: tape parquet lacks {name}", path.display()))?;
    batch
        .column(i)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| format!("{}: tape column {name} has unexpected type", path.display()))
}

/// Per-symbol pull bookkeeping. `present` maps a stored day to its row
/// count; `missing` lists days the archive answered 404. A day is in at
/// most one of the two.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SymbolManifest {
    pub present: BTreeMap<NaiveDate, u64>,
    pub missing: Vec<NaiveDate>,
}

impl SymbolManifest {
    pub fn record_present(&mut self, day: NaiveDate, rows: u64) {
        self.missing.retain(|d| *d != day);
        self.present.insert(day, rows);
    }
    pub fn record_missing(&mut self, day: NaiveDate) {
        self.present.remove(&day);
        if !self.missing.contains(&day) {
            self.missing.push(day);
            self.missing.sort();
        }
    }
    pub fn has(&self, day: NaiveDate) -> bool {
        self.present.contains_key(&day) || self.missing.contains(&day)
    }
}

pub type Manifest = BTreeMap<String, SymbolManifest>;

pub fn read_manifest(path: &Path) -> Result<Manifest, String> {
    if !path.exists() {
        return Ok(Manifest::new());
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_manifest(path: &Path, manifest: &Manifest) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = crate::tmp_sibling(path);
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(csv: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("x.csv", opts).unwrap();
            w.write_all(csv.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scalper-tape-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_stored_day_round_trips_the_archive_rows_losslessly() {
        // Header row, ms timestamps, mixed-case booleans, an out-of-order
        // row and a duplicate: all must come back exactly as given, in
        // the given order - the store neither sorts nor dedups.
        let csv = "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n\
                   1,100.5,0.25,1,1,1722470400123,true\n\
                   2,100.6,1.5,2,3,1722470400456,False\n\
                   3,100.4,0.001,4,4,1722470400100,TRUE\n\
                   4,100.4,0.001,4,4,1722470400100,TRUE\n";
        let bytes = zip_of(csv);
        let trades = parse_agg_trades_zip_raw(&bytes, "TESTUSDT").unwrap();
        assert_eq!(trades.len(), 4);
        assert_eq!(
            trades[0],
            TapeTrade {
                ts_ms: 1722470400123,
                price: 100.5,
                qty: 0.25,
                is_buyer_maker: true
            }
        );
        assert!(!trades[1].is_buyer_maker && trades[1].is_buy());
        assert_eq!(trades[2], trades[3]);

        let dir = scratch("roundtrip");
        let path = day_path(
            &dir,
            "TESTUSDT",
            NaiveDate::from_ymd_opt(2024, 8, 1).unwrap(),
        );
        write_day(&path, &trades).unwrap();
        let back = read_day(&path).unwrap();
        assert_eq!(back, trades);
        assert!(!crate::tmp_sibling(&path).exists());
    }

    #[test]
    fn microsecond_stamps_are_normalised_to_ms_like_the_flow_store() {
        let csv = "1,1.0,1.0,1,1,1722470400123456,false\n";
        let trades =
            parse_agg_trades_lines_raw(csv.lines().map(|l| Ok(l.to_string())), "T").unwrap();
        assert_eq!(trades[0].ts_ms, 1722470400123);
    }

    #[test]
    fn an_empty_day_writes_an_empty_file_not_nothing() {
        let dir = scratch("empty");
        let path = day_path(
            &dir,
            "TESTUSDT",
            NaiveDate::from_ymd_opt(2024, 8, 2).unwrap(),
        );
        write_day(&path, &[]).unwrap();
        assert!(path.exists());
        assert!(read_day(&path).unwrap().is_empty());
    }

    #[test]
    fn manifest_records_404_days_separately_and_a_day_lives_in_one_place() {
        let d1 = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2024, 8, 2).unwrap();
        let mut m = SymbolManifest::default();
        m.record_missing(d1);
        m.record_present(d2, 10);
        assert!(m.has(d1) && m.has(d2));
        assert_eq!(m.missing, vec![d1]);
        // A later successful fetch of a formerly-404 day moves it over.
        m.record_present(d1, 3);
        assert!(m.missing.is_empty());
        assert_eq!(m.present[&d1], 3);

        let dir = scratch("manifest");
        let path = manifest_path(&dir);
        let mut all = Manifest::new();
        all.insert("TESTUSDT".into(), m.clone());
        write_manifest(&path, &all).unwrap();
        assert_eq!(read_manifest(&path).unwrap(), all);
        assert!(read_manifest(&dir.join("nope.json")).unwrap().is_empty());
    }
}

/// Serves one symbol's tape to `features_scalper::compute` a minute at a
/// time, loading one day file at a time (bars are consumed in ascending
/// order, so at most the current day is resident - ~1M trades for the
/// busiest symbol). A minute on a day with no file answers `None` (no
/// coverage); a minute on a present day with no prints answers
/// `Some(empty)`.
pub struct TapeCursor {
    tape_root: PathBuf,
    symbol: String,
    loaded_day: Option<NaiveDate>,
    day_present: bool,
    minutes: BTreeMap<i64, Vec<features_scalper::TapeTrade>>,
}

impl TapeCursor {
    pub fn new(tape_root: &Path, symbol: &str) -> Self {
        Self {
            tape_root: tape_root.to_path_buf(),
            symbol: symbol.to_string(),
            loaded_day: None,
            day_present: false,
            minutes: BTreeMap::new(),
        }
    }

    fn load(&mut self, day: NaiveDate) -> Result<(), String> {
        self.minutes.clear();
        self.loaded_day = Some(day);
        let path = day_path(&self.tape_root, &self.symbol, day);
        if !path.exists() {
            self.day_present = false;
            return Ok(());
        }
        self.day_present = true;
        for t in read_day(&path)? {
            let minute = t.ts_ms.div_euclid(60_000) * 60;
            self.minutes
                .entry(minute)
                .or_default()
                .push(features_scalper::TapeTrade {
                    ts_ms: t.ts_ms,
                    price: t.price,
                    qty: t.qty,
                    is_buy: !t.is_buyer_maker,
                });
        }
        Ok(())
    }
}

impl features_scalper::TapeSource for TapeCursor {
    fn minute(&mut self, minute_ts_s: i64) -> Result<Option<features_scalper::TapeMinute>, String> {
        let day = chrono::DateTime::<chrono::Utc>::from_timestamp(minute_ts_s, 0)
            .ok_or_else(|| format!("{}: bad minute ts {minute_ts_s}", self.symbol))?
            .date_naive();
        if self.loaded_day != Some(day) {
            self.load(day)?;
        }
        if !self.day_present {
            return Ok(None);
        }
        Ok(Some(features_scalper::TapeMinute {
            trades: self.minutes.get(&minute_ts_s).cloned().unwrap_or_default(),
        }))
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    use features_scalper::TapeSource;

    #[test]
    fn cursor_serves_minutes_from_day_files_and_none_for_absent_days() {
        let dir = std::env::temp_dir().join(format!("scalper-tape-cursor-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let day = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let day_start_ms = day
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let trades = vec![
            TapeTrade {
                ts_ms: day_start_ms + 61_000,
                price: 1.0,
                qty: 1.0,
                is_buyer_maker: true,
            },
            TapeTrade {
                ts_ms: day_start_ms + 119_999,
                price: 2.0,
                qty: 1.0,
                is_buyer_maker: false,
            },
            TapeTrade {
                ts_ms: day_start_ms + 120_000,
                price: 3.0,
                qty: 1.0,
                is_buyer_maker: false,
            },
        ];
        write_day(&day_path(&dir, "T", day), &trades).unwrap();
        let mut cur = TapeCursor::new(&dir, "T");
        let m1 = cur.minute(day_start_ms / 1000 + 60).unwrap().unwrap();
        assert_eq!(m1.trades.len(), 2, "[60s, 120s) holds the first two");
        assert!(!m1.trades[0].is_buy && m1.trades[1].is_buy);
        let m2 = cur.minute(day_start_ms / 1000 + 120).unwrap().unwrap();
        assert_eq!(m2.trades.len(), 1);
        let m0 = cur.minute(day_start_ms / 1000).unwrap().unwrap();
        assert!(
            m0.trades.is_empty(),
            "a covered minute with no prints is Some(empty)"
        );
        // Next day has no file -> None.
        assert!(cur.minute(day_start_ms / 1000 + 86_400).unwrap().is_none());
    }
}
