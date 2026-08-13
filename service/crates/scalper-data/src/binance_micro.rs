//! Binance UM microstructure archives: bookDepth, aggTrades, fundingRate,
//! metrics — the free daily/monthly zips under data.binance.vision that turn
//! into per-minute book/flow/funding/metrics series for the scalper research
//! matrix.
//!
//! Mirrors `binance_um`'s fetch/404 idiom (a literal 404 is "not published
//! yet" or "delisted", not an error - anything else aborts) but these four
//! sources are minute- or 5-minute-granularity microstructure, not OHLC
//! bars, so each source gets its own parser and JSONL row shape. aggTrades
//! files run to hundreds of MB decompressed, so that parser streams
//! line-by-line off the zip entry's own reader and never materializes the
//! whole CSV as one `String` - `bookDepth`/`fundingRate`/`metrics` are small
//! enough (tens of KB to a few MB) that `read_to_string` is fine, same as
//! `binance_um::parse_um_klines_zip`.
//!
//! Archive shapes verified against a live 2026-08 download: bookDepth and
//! metrics are daily-only (no monthly archive exists for either - a monthly
//! probe 404s). aggTrades DOES have a monthly archive too, but this module
//! deliberately pulls it daily anyway: monthly aggTrades for a busy symbol
//! would be tens of times the daily file's size, which the fetch path (a
//! full zip buffered into memory via `reqwest::Response::bytes`) can't
//! stream around. fundingRate is monthly-only (a daily probe 404s) - funding
//! settles every 8h, so a whole month's events fit in well under a
//! kilobyte.
//!
//! Identity note: unlike `data/perp` (keyed by the uppercased HL store key),
//! micro data is written keyed by the BINANCE SYMBOL (e.g. `1000PEPEUSDT`,
//! not the HL store key `KPEPE`). The matrix layer is what joins HL coin
//! identity back to this data, via the universe file's `binance_um` field -
//! do not key these paths by store asset.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use std::io::{BufRead, BufReader, Read};

use crate::binance_um::epoch_utc;

const ARCHIVE: &str = "https://data.binance.vision";

pub const KIND_BOOK_DEPTH: &str = "bookDepth";
pub const KIND_AGG_TRADES: &str = "aggTrades";
pub const KIND_FUNDING_RATE: &str = "fundingRate";
pub const KIND_METRICS: &str = "metrics";

fn daily_zip_url(kind: &str, symbol: &str, date: NaiveDate) -> String {
    format!("{ARCHIVE}/data/futures/um/daily/{kind}/{symbol}/{symbol}-{kind}-{date}.zip")
}

fn monthly_zip_url(kind: &str, symbol: &str, year: i32, month: u32) -> String {
    format!("{ARCHIVE}/data/futures/um/monthly/{kind}/{symbol}/{symbol}-{kind}-{year:04}-{month:02}.zip")
}

/// One daily archive zip, or `None` on a literal 404 - "not published yet"
/// (today's file lags) and "this symbol has no recorded rows that day" both
/// look like a 404 and both are survivorship truth, not an error.
pub async fn fetch_daily(
    client: &reqwest::Client,
    kind: &str,
    symbol: &str,
    date: NaiveDate,
) -> Result<Option<Vec<u8>>, String> {
    let url = daily_zip_url(kind, symbol, date);
    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        return Err(format!("{url}: HTTP {}", res.status()));
    }
    let bytes = res.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    Ok(Some(bytes.to_vec()))
}

/// One monthly archive zip, or `None` on a literal 404.
pub async fn fetch_monthly(
    client: &reqwest::Client,
    kind: &str,
    symbol: &str,
    year: i32,
    month: u32,
) -> Result<Option<Vec<u8>>, String> {
    let url = monthly_zip_url(kind, symbol, year, month);
    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        return Err(format!("{url}: HTTP {}", res.status()));
    }
    let bytes = res.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    Ok(Some(bytes.to_vec()))
}

/// Calendar UTC days in `[start, end)` - the same exclusive-end convention
/// `main::months` uses for monthly enumeration.
pub(crate) fn days(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<NaiveDate> {
    let mut day = start.date_naive();
    let last = (end - chrono::Duration::microseconds(1)).date_naive();
    let mut out = Vec::new();
    while day <= last {
        out.push(day);
        day = day.succ_opt().expect("day within a bounded range");
    }
    out
}

// ---------------------------------------------------------------------
// bookDepth
// ---------------------------------------------------------------------

/// One minute's downsampled bookDepth snapshot: the last raw snapshot at or
/// before the minute's close, keeping only the ±0.2% and ±1.0% bands (the
/// rest are noise for $5-20k clips). Keys are the literal band percentages
/// as strings (`"-1.0"`, `"-0.2"`, `"0.2"`, `"1.0"`), values are notional.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BookMinute {
    pub ts_s: i64,
    pub bands: std::collections::BTreeMap<String, f64>,
}

/// The four percentage bands kept out of Binance's full ±5% ladder, paired
/// with the exact JSON key each renders as.
const BOOK_BANDS: [(&str, f64); 4] = [("-1.0", -1.0), ("-0.2", -0.2), ("0.2", 0.2), ("1.0", 1.0)];

fn band_key(percentage: f64) -> Option<&'static str> {
    BOOK_BANDS
        .iter()
        .find(|(_, v)| (percentage - v).abs() < 1e-6)
        .map(|(k, _)| *k)
}

/// Parse one bookDepth daily zip into per-minute snapshots. Binance stamps
/// each row `timestamp,percentage,depth,notional` at second precision
/// (space-separated, UTC, e.g. `2026-08-10 00:00:01`); several rows share
/// one timestamp (one per percentage band).
///
/// Downsampling keeps, for each minute bucket, the LAST raw snapshot whose
/// timestamp falls in that bucket - equivalently the last snapshot at or
/// before the minute's close, since a snapshot timestamped in minute N+1
/// is by definition after minute N's close. This is the causal
/// snap-downward rule from the plan's global constraints.
pub fn parse_book_depth_zip(bytes: &[u8], asset: &str) -> Result<Vec<BookMinute>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{asset}: bad zip: {e}"))?;
    let mut csv = String::new();
    archive
        .by_index(0)
        .map_err(|e| format!("{asset}: empty zip: {e}"))?
        .read_to_string(&mut csv)
        .map_err(|e| format!("{asset}: unreadable csv: {e}"))?;

    // minute bucket -> (latest raw snapshot ts seen in the bucket, bands
    // collected at that latest ts; an earlier ts in the same bucket resets
    // the collected bands rather than merging with the newer ones).
    let mut buckets: std::collections::BTreeMap<
        i64,
        (i64, std::collections::BTreeMap<String, f64>),
    > = std::collections::BTreeMap::new();

    for line in csv.lines() {
        if line.is_empty() || line.starts_with("timestamp") {
            continue; // header row some files carry
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 4 {
            return Err(format!(
                "{asset}: bookDepth row has {} columns: {line}",
                cols.len()
            ));
        }
        let ts = NaiveDateTime::parse_from_str(cols[0].trim(), "%Y-%m-%d %H:%M:%S")
            .map_err(|e| format!("{asset}: bad timestamp {:?}: {e}", cols[0]))?
            .and_utc();
        let ts_epoch = ts.timestamp();
        let percentage: f64 = cols[1]
            .trim()
            .parse()
            .map_err(|e| format!("{asset}: percentage: {e}: {line}"))?;
        let Some(key) = band_key(percentage) else {
            continue; // outside the ±0.2/±1.0 bands we keep
        };
        let notional: f64 = cols[3]
            .trim()
            .parse()
            .map_err(|e| format!("{asset}: notional: {e}: {line}"))?;

        let minute = ts_epoch.div_euclid(60) * 60;
        let entry = buckets
            .entry(minute)
            .or_insert_with(|| (i64::MIN, std::collections::BTreeMap::new()));
        match ts_epoch.cmp(&entry.0) {
            std::cmp::Ordering::Greater => {
                entry.0 = ts_epoch;
                entry.1.clear();
                entry.1.insert(key.to_string(), notional);
            }
            std::cmp::Ordering::Equal => {
                entry.1.insert(key.to_string(), notional);
            }
            std::cmp::Ordering::Less => {} // an out-of-order older row: ignored
        }
    }

    let mut out: Vec<BookMinute> = buckets
        .into_iter()
        .map(|(ts_s, (_, bands))| BookMinute { ts_s, bands })
        .collect();
    out.sort_by_key(|b| b.ts_s);
    Ok(out)
}

// ---------------------------------------------------------------------
// aggTrades
// ---------------------------------------------------------------------

/// Per-minute flow aggregate distilled from the raw aggTrades tape - the
/// tape itself is never stored.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FlowMinute {
    pub ts_s: i64,
    /// `None` when fewer than 5 valid spread samples were found that
    /// minute (thin trading, or no maker/taker pairs within 1000ms).
    pub spread_bps_med: Option<f64>,
    pub n_spread_samples: usize,
    pub taker_buy_ratio: f64,
    pub n_trades: u64,
    pub notional: f64,
}

/// Pre-registered spread estimator: pair each ask-side trade (a taker buy,
/// `is_buyer_maker == false` - the taker crossed the spread and bought at
/// the ask) with the time-nearest bid-side trade (a taker sell,
/// `is_buyer_maker == true`) within 1000ms; sample = `(ask_px - bid_px) /
/// mid * 1e4`; negative samples (an out-of-order or stale pair) are
/// discarded. Requires ≥5 valid samples, else no spread estimate that
/// minute - `n_spread_samples` still reports however many were actually
/// found, so a near-miss ("4 samples, no estimate") stays visible.
///
/// `ask_trades`/`bid_trades` are `(ts_ms, price)`, each already sorted
/// ascending by `ts_ms` (the archive's native row order) - the binary
/// search below assumes that.
pub(crate) fn spread_estimate_bps(
    ask_trades: &[(i64, f64)],
    bid_trades: &[(i64, f64)],
) -> (Option<f64>, usize) {
    const MAX_GAP_MS: i64 = 1000;
    let mut samples = Vec::new();
    for &(ask_ts, ask_px) in ask_trades {
        let idx = bid_trades.partition_point(|&(ts, _)| ts < ask_ts);
        let mut best: Option<(i64, f64)> = None;
        if idx < bid_trades.len() {
            let (ts, px) = bid_trades[idx];
            best = Some(((ts - ask_ts).abs(), px));
        }
        if idx > 0 {
            let (ts, px) = bid_trades[idx - 1];
            let gap = (ts - ask_ts).abs();
            if best.map(|(g, _)| gap < g).unwrap_or(true) {
                best = Some((gap, px));
            }
        }
        let Some((gap, bid_px)) = best else {
            continue; // no bid-side trade at all this minute
        };
        if gap > MAX_GAP_MS {
            continue;
        }
        let mid = (ask_px + bid_px) / 2.0;
        if mid <= 0.0 {
            continue;
        }
        let sample = (ask_px - bid_px) / mid * 1e4;
        if sample < 0.0 {
            continue; // discarded, per the pre-registered rule
        }
        samples.push(sample);
    }
    let n = samples.len();
    if n < 5 {
        return (None, n);
    }
    (Some(crate::costs::percentile(&samples, 0.5)), n)
}

/// Accumulates one minute's aggTrades rows into the fields `FlowMinute`
/// needs, without keeping the individual trades around any longer than the
/// minute they belong to - the bound on memory that makes streaming a
/// hundreds-of-MB day file safe.
struct MinuteAcc {
    minute: i64,
    ask_trades: Vec<(i64, f64)>,
    bid_trades: Vec<(i64, f64)>,
    buy_notional: f64,
    total_notional: f64,
    n_trades: u64,
}

impl MinuteAcc {
    fn new(minute: i64) -> Self {
        Self {
            minute,
            ask_trades: Vec::new(),
            bid_trades: Vec::new(),
            buy_notional: 0.0,
            total_notional: 0.0,
            n_trades: 0,
        }
    }

    fn push(&mut self, ts_ms: i64, price: f64, qty: f64, is_buyer_maker: bool) {
        let notional = price * qty;
        self.total_notional += notional;
        self.n_trades += 1;
        if is_buyer_maker {
            self.bid_trades.push((ts_ms, price));
        } else {
            self.ask_trades.push((ts_ms, price));
            self.buy_notional += notional;
        }
    }

    fn finish(self) -> FlowMinute {
        let (spread_bps_med, n_spread_samples) =
            spread_estimate_bps(&self.ask_trades, &self.bid_trades);
        let taker_buy_ratio = if self.total_notional > 0.0 {
            self.buy_notional / self.total_notional
        } else {
            0.0
        };
        FlowMinute {
            ts_s: self.minute,
            spread_bps_med,
            n_spread_samples,
            taker_buy_ratio,
            n_trades: self.n_trades,
            notional: self.total_notional,
        }
    }
}

/// Parse aggTrades CSV lines (already split, e.g. by `BufRead::lines`) into
/// per-minute flow aggregates - streamed one line at a time, buffering at
/// most one minute's worth of trades. Columns:
/// `agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,
/// is_buyer_maker`; a header row (some files carry one, starting with
/// `agg`) is skipped. `transact_time` uses the same ms/µs autodetect as
/// klines' `open_time` (`binance_um::epoch_utc`).
fn parse_agg_trades_lines<I>(lines: I, asset: &str) -> Result<Vec<FlowMinute>, String>
where
    I: Iterator<Item = std::io::Result<String>>,
{
    let mut out = Vec::new();
    let mut cur: Option<MinuteAcc> = None;
    for line in lines {
        let line = line.map_err(|e| format!("{asset}: {e}"))?;
        if line.is_empty() || line.starts_with("agg") {
            continue; // header row some files carry
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 7 {
            return Err(format!(
                "{asset}: aggTrades row has {} columns: {line}",
                cols.len()
            ));
        }
        let price: f64 = cols[1]
            .parse()
            .map_err(|e| format!("{asset}: price: {e}: {line}"))?;
        let qty: f64 = cols[2]
            .parse()
            .map_err(|e| format!("{asset}: quantity: {e}: {line}"))?;
        let raw_ts: i64 = cols[5]
            .parse()
            .map_err(|e| format!("{asset}: transact_time: {e}: {line}"))?;
        let ts = epoch_utc(raw_ts)?;
        let is_buyer_maker: bool = cols[6]
            .trim()
            .parse()
            .map_err(|e| format!("{asset}: is_buyer_maker: {e}: {line}"))?;

        let ts_epoch = ts.timestamp();
        let ts_ms = ts.timestamp_millis();
        let minute = ts_epoch.div_euclid(60) * 60;

        if cur.as_ref().map(|a| a.minute) != Some(minute) {
            if let Some(acc) = cur.take() {
                out.push(acc.finish());
            }
            cur = Some(MinuteAcc::new(minute));
        }
        cur.as_mut()
            .expect("just set above")
            .push(ts_ms, price, qty, is_buyer_maker);
    }
    if let Some(acc) = cur.take() {
        out.push(acc.finish());
    }
    Ok(out)
}

/// Parse one aggTrades daily zip, streaming the decompressed CSV
/// line-by-line off the zip entry's own reader - never `read_to_string`,
/// since these files run to hundreds of MB decompressed for busy symbols.
pub fn parse_agg_trades_zip(bytes: &[u8], asset: &str) -> Result<Vec<FlowMinute>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{asset}: bad zip: {e}"))?;
    let entry = archive
        .by_index(0)
        .map_err(|e| format!("{asset}: empty zip: {e}"))?;
    let reader = BufReader::new(entry);
    parse_agg_trades_lines(reader.lines(), asset)
}

// ---------------------------------------------------------------------
// fundingRate
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FundingRow {
    pub ts_s: i64,
    pub funding_interval_hours: f64,
    pub funding_rate: f64,
}

/// Parse one fundingRate monthly zip. Columns verified against a live
/// download (2026-08): `calc_time,funding_interval_hours,
/// last_funding_rate` - `calc_time` an epoch (ms pre-2025 / µs 2025+, same
/// autodetect as klines' `open_time`).
pub fn parse_funding_rate_zip(bytes: &[u8], asset: &str) -> Result<Vec<FundingRow>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{asset}: bad zip: {e}"))?;
    let mut csv = String::new();
    archive
        .by_index(0)
        .map_err(|e| format!("{asset}: empty zip: {e}"))?
        .read_to_string(&mut csv)
        .map_err(|e| format!("{asset}: unreadable csv: {e}"))?;

    let mut out = Vec::new();
    for line in csv.lines() {
        if line.is_empty() || line.starts_with("calc_time") {
            continue; // header row some files carry
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 3 {
            return Err(format!(
                "{asset}: fundingRate row has {} columns: {line}",
                cols.len()
            ));
        }
        let raw_ts: i64 = cols[0]
            .parse()
            .map_err(|e| format!("{asset}: calc_time: {e}: {line}"))?;
        let ts = epoch_utc(raw_ts)?;
        let funding_interval_hours: f64 = cols[1]
            .parse()
            .map_err(|e| format!("{asset}: funding_interval_hours: {e}: {line}"))?;
        let funding_rate: f64 = cols[2]
            .parse()
            .map_err(|e| format!("{asset}: last_funding_rate: {e}: {line}"))?;
        out.push(FundingRow {
            ts_s: ts.timestamp(),
            funding_interval_hours,
            funding_rate,
        });
    }
    out.sort_by_key(|r| r.ts_s);
    out.dedup_by_key(|r| r.ts_s);
    Ok(out)
}

/// Merge freshly parsed funding rows into whatever a symbol's funding file
/// already holds - "append-safe by ts": a fresh row wins on a timestamp
/// collision (a re-pull with corrected upstream data should not need a
/// duplicate line to take effect), and the result comes back sorted, ready
/// to overwrite the file in full.
pub fn merge_funding_rows(existing: Vec<FundingRow>, fresh: Vec<FundingRow>) -> Vec<FundingRow> {
    let mut by_ts: std::collections::BTreeMap<i64, FundingRow> =
        existing.into_iter().map(|r| (r.ts_s, r)).collect();
    for r in fresh {
        by_ts.insert(r.ts_s, r);
    }
    by_ts.into_values().collect()
}

// ---------------------------------------------------------------------
// metrics
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetricsRow {
    pub ts_s: i64,
    pub sum_open_interest: f64,
    pub sum_open_interest_value: f64,
    pub count_toptrader_long_short_ratio: f64,
    pub sum_toptrader_long_short_ratio: f64,
    pub count_long_short_ratio: f64,
    pub sum_taker_long_short_vol_ratio: f64,
}

/// Parse one metrics daily zip. Columns verified against a live download
/// (2026-08): `create_time,symbol,sum_open_interest,
/// sum_open_interest_value,count_toptrader_long_short_ratio,
/// sum_toptrader_long_short_ratio,count_long_short_ratio,
/// sum_taker_long_short_vol_ratio` - 5-minute rows. `create_time` is
/// space-separated UTC (`2026-08-12 00:00:00`), the same format as
/// bookDepth's `timestamp` column, NOT an epoch. `symbol` is dropped: the
/// output file is already keyed by symbol via its path.
pub fn parse_metrics_zip(bytes: &[u8], asset: &str) -> Result<Vec<MetricsRow>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{asset}: bad zip: {e}"))?;
    let mut csv = String::new();
    archive
        .by_index(0)
        .map_err(|e| format!("{asset}: empty zip: {e}"))?
        .read_to_string(&mut csv)
        .map_err(|e| format!("{asset}: unreadable csv: {e}"))?;

    let mut out = Vec::new();
    for line in csv.lines() {
        if line.is_empty() || line.starts_with("create_time") {
            continue; // header row some files carry
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 8 {
            return Err(format!(
                "{asset}: metrics row has {} columns: {line}",
                cols.len()
            ));
        }
        let ts = NaiveDateTime::parse_from_str(cols[0].trim(), "%Y-%m-%d %H:%M:%S")
            .map_err(|e| format!("{asset}: bad create_time {:?}: {e}", cols[0]))?
            .and_utc();
        let f = |i: usize| -> Result<f64, String> {
            cols[i]
                .parse()
                .map_err(|e| format!("{asset}: col {i}: {e}: {line}"))
        };
        out.push(MetricsRow {
            ts_s: ts.timestamp(),
            sum_open_interest: f(2)?,
            sum_open_interest_value: f(3)?,
            count_toptrader_long_short_ratio: f(4)?,
            sum_toptrader_long_short_ratio: f(5)?,
            count_long_short_ratio: f(6)?,
            sum_taker_long_short_vol_ratio: f(7)?,
        });
    }
    out.sort_by_key(|r| r.ts_s);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_um::tests::zip_with_one_file;

    #[test]
    fn daily_and_monthly_url_shapes() {
        let d = NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        assert_eq!(
            daily_zip_url(KIND_BOOK_DEPTH, "BTCUSDT", d),
            "https://data.binance.vision/data/futures/um/daily/bookDepth/BTCUSDT/BTCUSDT-bookDepth-2026-08-13.zip"
        );
        assert_eq!(
            daily_zip_url(KIND_AGG_TRADES, "BTCUSDT", d),
            "https://data.binance.vision/data/futures/um/daily/aggTrades/BTCUSDT/BTCUSDT-aggTrades-2026-08-13.zip"
        );
        assert_eq!(
            daily_zip_url(KIND_METRICS, "BTCUSDT", d),
            "https://data.binance.vision/data/futures/um/daily/metrics/BTCUSDT/BTCUSDT-metrics-2026-08-13.zip"
        );
        assert_eq!(
            monthly_zip_url(KIND_FUNDING_RATE, "BTCUSDT", 2026, 8),
            "https://data.binance.vision/data/futures/um/monthly/fundingRate/BTCUSDT/BTCUSDT-fundingRate-2026-08.zip"
        );
    }

    #[test]
    fn days_covers_the_half_open_range() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, 0).unwrap();
        let got = days(start, end);
        assert_eq!(
            got,
            vec![
                NaiveDate::from_ymd_opt(2026, 8, 12).unwrap(),
                NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
            ]
        );
    }

    // -- bookDepth -----------------------------------------------------

    #[test]
    fn book_depth_downsample_keeps_the_last_snapshot_and_only_the_kept_bands() {
        let csv = "timestamp,percentage,depth,notional\n\
            2026-08-12 00:00:01,-5.00,100,1000\n\
            2026-08-12 00:00:01,-1.00,50,500\n\
            2026-08-12 00:00:01,-0.20,10,100\n\
            2026-08-12 00:00:01,0.20,10,111\n\
            2026-08-12 00:00:01,1.00,50,555\n\
            2026-08-12 00:00:31,-1.00,60,600\n\
            2026-08-12 00:00:31,-0.20,12,120\n\
            2026-08-12 00:00:31,0.20,12,132\n\
            2026-08-12 00:00:31,1.00,60,660\n";
        let bytes = zip_with_one_file("BTCUSDT-bookDepth-2026-08-12.csv", csv.as_bytes());
        let rows = parse_book_depth_zip(&bytes, "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 1, "both snapshots fall in the same minute bucket");
        let row = &rows[0];
        assert_eq!(row.ts_s, 1_786_492_800); // 2026-08-12T00:00:00Z
        // Only the LAST snapshot's (00:00:31) values survive, and only the
        // ±0.2/±1.0 bands - the ±5% row never had a matching key at all.
        assert_eq!(row.bands.len(), 4);
        assert_eq!(row.bands["-1.0"], 600.0);
        assert_eq!(row.bands["-0.2"], 120.0);
        assert_eq!(row.bands["0.2"], 132.0);
        assert_eq!(row.bands["1.0"], 660.0);
    }

    #[test]
    fn book_depth_downsample_splits_across_minute_buckets() {
        let csv = "2026-08-12 00:00:01,0.20,10,100\n\
            2026-08-12 00:01:01,0.20,20,200\n";
        let bytes = zip_with_one_file("BTCUSDT-bookDepth-2026-08-12.csv", csv.as_bytes());
        let rows = parse_book_depth_zip(&bytes, "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ts_s, 1_786_492_800);
        assert_eq!(rows[0].bands["0.2"], 100.0);
        assert_eq!(rows[1].ts_s, 1_786_492_860);
        assert_eq!(rows[1].bands["0.2"], 200.0);
    }

    // -- spread estimator -----------------------------------------------

    #[test]
    fn spread_estimator_known_pairs_produce_the_known_median() {
        // Three ask trades, each with a bid trade close in time. Samples:
        // (100.10-100.00)/100.05*1e4 ~= 9.995
        // (100.20-100.00)/100.10*1e4 ~= 19.98
        // (100.30-100.10)/100.20*1e4 ~= 19.96
        // Median of the three is the middle one, ~19.96..19.98.
        let asks = vec![(1_000, 100.10), (2_000, 100.20), (3_000, 100.30)];
        let bids = vec![(1_100, 100.00), (2_050, 100.00), (3_400, 100.10)];
        let (median, n) = spread_estimate_bps(&asks, &bids);
        assert_eq!(n, 3);
        assert!(median.is_none(), "n=3 < 5 required samples, so no estimate");

        // Pad past the 5-sample floor with two more clean pairs.
        let asks2 = vec![
            (1_000, 100.10),
            (2_000, 100.20),
            (3_000, 100.30),
            (4_000, 100.10),
            (5_000, 100.10),
        ];
        let bids2 = vec![
            (1_100, 100.00),
            (2_050, 100.00),
            (3_400, 100.10),
            (4_200, 100.00),
            (5_300, 100.00),
        ];
        let (median2, n2) = spread_estimate_bps(&asks2, &bids2);
        assert_eq!(n2, 5);
        let m = median2.expect("5 samples clears the floor");
        assert!(m > 0.0 && m < 30.0, "got {m}");
    }

    #[test]
    fn spread_estimator_ignores_ask_trades_with_no_nearby_bid() {
        // The ask at ts=10_000 has no bid within 1000ms of it (nearest bid
        // is 5000ms away), so it contributes no sample.
        let asks = vec![(10_000, 100.10)];
        let bids = vec![(5_000, 100.00)];
        let (median, n) = spread_estimate_bps(&asks, &bids);
        assert_eq!(n, 0);
        assert!(median.is_none());
    }

    #[test]
    fn spread_estimator_discards_negative_samples() {
        // ask_px < bid_px would be a negative sample (crossed/out-of-order
        // quotes) and must be dropped, not folded into the median.
        let asks = vec![(1_000, 99.90)];
        let bids = vec![(1_050, 100.00)];
        let (median, n) = spread_estimate_bps(&asks, &bids);
        assert_eq!(n, 0, "the only candidate sample was negative and got discarded");
        assert!(median.is_none());
    }

    #[test]
    fn spread_estimator_below_five_samples_reports_none_but_keeps_the_count() {
        let asks = vec![(1_000, 100.10), (2_000, 100.10), (3_000, 100.10)];
        let bids = vec![(1_100, 100.00), (2_100, 100.00), (3_100, 100.00)];
        let (median, n) = spread_estimate_bps(&asks, &bids);
        assert_eq!(n, 3);
        assert!(median.is_none());
    }

    // -- aggTrades --------------------------------------------------------

    fn agg_trades_csv() -> String {
        // Minute 0 (ts 00:00:xx): three asks + three bids, well within
        // 1000ms of each other, plenty to clear the 5-sample floor when
        // combined... actually we only get 3 pairs here, so spread stays
        // None but taker_buy_ratio/notional/n_trades are exercised.
        // Minute 1 (ts 00:01:xx): one lone trade.
        let mut s = String::new();
        s.push_str("agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n");
        // minute 0: three ask (taker buy) trades, three bid (taker sell)
        s.push_str("1,100.10,1.0,1,1,1754956800100,false\n");
        s.push_str("2,100.00,1.0,2,2,1754956800150,true\n");
        s.push_str("3,100.20,1.0,3,3,1754956801100,false\n");
        s.push_str("4,100.00,1.0,4,4,1754956801150,true\n");
        s.push_str("5,100.30,2.0,5,5,1754956802100,false\n");
        s.push_str("6,100.10,2.0,6,6,1754956802150,true\n");
        // minute 1: single taker-buy trade, no pairing possible
        s.push_str("7,100.50,0.5,7,7,1754956860500,false\n");
        s
    }

    #[test]
    fn agg_trades_stream_buckets_by_minute_and_computes_flow_fields() {
        let csv = agg_trades_csv();
        let bytes = zip_with_one_file("BTCUSDT-aggTrades-2026-08-12.csv", csv.as_bytes());
        let rows = parse_agg_trades_zip(&bytes, "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 2);

        let m0 = &rows[0];
        assert_eq!(m0.ts_s, 1_754_956_800);
        assert_eq!(m0.n_trades, 6);
        // notional = 100.10+100.00+100.20+100.00+100.30*2+100.10*2
        let expected_notional =
            100.10 + 100.00 + 100.20 + 100.00 + 100.30 * 2.0 + 100.10 * 2.0;
        assert!((m0.notional - expected_notional).abs() < 1e-6);
        // buy (ask-side) notional = 100.10 + 100.20 + 100.30*2
        let expected_buy = 100.10 + 100.20 + 100.30 * 2.0;
        assert!((m0.taker_buy_ratio - expected_buy / expected_notional).abs() < 1e-9);
        // 3 ask trades each pair with a close-in-time bid trade -> 3
        // samples, below the 5-sample floor, so no median yet.
        assert_eq!(m0.n_spread_samples, 3);
        assert!(m0.spread_bps_med.is_none());

        let m1 = &rows[1];
        assert_eq!(m1.ts_s, 1_754_956_860);
        assert_eq!(m1.n_trades, 1);
        assert_eq!(m1.taker_buy_ratio, 1.0);
        assert_eq!(m1.n_spread_samples, 0);
        assert!(m1.spread_bps_med.is_none());
    }

    #[test]
    fn agg_trades_stream_skips_the_header_and_autodetects_us_epochs() {
        // Same minute-0 trade as above but with a microsecond transact_time
        // (2025+ files) instead of milliseconds - must resolve to the same
        // minute bucket.
        let csv = "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker\n\
            1,100.10,1.0,1,1,1754956800100000,false\n";
        let bytes = zip_with_one_file("BTCUSDT-aggTrades-2026-08-12.csv", csv.as_bytes());
        let rows = parse_agg_trades_zip(&bytes, "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts_s, 1_754_956_800);
    }

    // -- fundingRate ------------------------------------------------------

    #[test]
    fn funding_rows_pass_through_with_epoch_autodetect() {
        let csv_ms = "calc_time,funding_interval_hours,last_funding_rate\n\
            1754956800000,8,0.00005532\n";
        let bytes_ms = zip_with_one_file("BTCUSDT-fundingRate-2026-08.csv", csv_ms.as_bytes());
        let rows_ms = parse_funding_rate_zip(&bytes_ms, "BTCUSDT").unwrap();
        assert_eq!(rows_ms.len(), 1);
        assert_eq!(rows_ms[0].ts_s, 1_754_956_800);
        assert_eq!(rows_ms[0].funding_interval_hours, 8.0);
        assert_eq!(rows_ms[0].funding_rate, 0.00005532);

        let csv_us = "1754956800000000,8,0.00005532\n";
        let bytes_us = zip_with_one_file("BTCUSDT-fundingRate-2026-08.csv", csv_us.as_bytes());
        let rows_us = parse_funding_rate_zip(&bytes_us, "BTCUSDT").unwrap();
        assert_eq!(rows_us[0].ts_s, rows_ms[0].ts_s);
    }

    #[test]
    fn merge_funding_rows_dedups_by_ts_and_prefers_the_fresh_row() {
        let existing = vec![
            FundingRow {
                ts_s: 100,
                funding_interval_hours: 8.0,
                funding_rate: 0.0001,
            },
            FundingRow {
                ts_s: 200,
                funding_interval_hours: 8.0,
                funding_rate: 0.0002,
            },
        ];
        let fresh = vec![
            FundingRow {
                ts_s: 200,
                funding_interval_hours: 8.0,
                funding_rate: 0.00025, // corrected value for an existing ts
            },
            FundingRow {
                ts_s: 300,
                funding_interval_hours: 8.0,
                funding_rate: 0.0003,
            },
        ];
        let merged = merge_funding_rows(existing, fresh);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].ts_s, 100);
        assert_eq!(merged[1].ts_s, 200);
        assert_eq!(merged[1].funding_rate, 0.00025, "fresh row wins on collision");
        assert_eq!(merged[2].ts_s, 300);
    }

    // -- metrics ------------------------------------------------------

    #[test]
    fn metrics_rows_pass_through_and_drop_the_symbol_column() {
        let csv = "create_time,symbol,sum_open_interest,sum_open_interest_value,\
            count_toptrader_long_short_ratio,sum_toptrader_long_short_ratio,\
            count_long_short_ratio,sum_taker_long_short_vol_ratio\n\
            2026-08-12 00:00:00,BTCUSDT,109311.972,6946232440.128,1.90829977,1.66235,1.80442398,0.839761\n\
            2026-08-12 00:05:00,BTCUSDT,109320.0,6947000000.0,1.9,1.6,1.8,0.84\n";
        let bytes = zip_with_one_file("BTCUSDT-metrics-2026-08-12.csv", csv.as_bytes());
        let rows = parse_metrics_zip(&bytes, "BTCUSDT").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].ts_s, 1_786_492_800);
        assert_eq!(rows[0].sum_open_interest, 109311.972);
        assert_eq!(rows[0].sum_open_interest_value, 6946232440.128);
        assert_eq!(rows[0].count_toptrader_long_short_ratio, 1.90829977);
        assert_eq!(rows[0].sum_toptrader_long_short_ratio, 1.66235);
        assert_eq!(rows[0].count_long_short_ratio, 1.80442398);
        assert_eq!(rows[0].sum_taker_long_short_vol_ratio, 0.839761);
        assert_eq!(rows[1].ts_s, 1_786_493_100);
    }
}
