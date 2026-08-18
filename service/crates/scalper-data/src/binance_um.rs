//! Binance USDT-perp (UM futures) monthly kline archive.
//!
//! Mirrors the spot pipeline in `crypto_portfolio::binance_archive` but is a
//! fresh implementation: that crate is the frozen strategy's and stays
//! untouched. URL shape:
//!   https://data.binance.vision/data/futures/um/monthly/klines/{SYMBOL}/1m/{SYMBOL}-1m-{YYYY-MM}.zip

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use features_crypto::Bar;
use std::io::Read;

const ARCHIVE: &str = "https://data.binance.vision";

/// The Binance UM symbol for a HyperLiquid coin. HL's k-prefix (thousandths)
/// coins trade on Binance with a 1000-prefix. `None` = not listed on UM, and
/// the caller must exclude the asset from Binance-based training rather than
/// silently substituting spot.
pub fn binance_um_symbol(asset: &str) -> Option<String> {
    let unlisted = ["HYPE", "PURR"];
    if unlisted.contains(&asset) {
        return None;
    }
    match asset.strip_prefix('k') {
        Some(rest) => Some(format!("1000{}USDT", rest.to_uppercase())),
        None => Some(format!("{}USDT", asset.to_uppercase())),
    }
}

/// One monthly zip, or `None` on 404 - a missing month is what "not listed
/// yet" and "already delisted" look like, and both are survivorship truth.
pub async fn fetch_um_month(
    client: &reqwest::Client,
    symbol: &str,
    year: i32,
    month: u32,
) -> Result<Option<Vec<u8>>, String> {
    let url = format!(
        "{ARCHIVE}/data/futures/um/monthly/klines/{symbol}/1m/{symbol}-1m-{year:04}-{month:02}.zip"
    );
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

/// URL of one daily UM kline zip. Separate from `fetch_um_day` so the shape
/// can be asserted without a network round trip.
fn um_day_url(symbol: &str, date: NaiveDate) -> String {
    format!("{ARCHIVE}/data/futures/um/daily/klines/{symbol}/1m/{symbol}-1m-{date}.zip")
}

/// One daily zip, or `None` on 404 - Binance publishes the current day's
/// file with a lag, so a 404 for today (or a day that hasn't happened yet)
/// is normal, not an error.
pub async fn fetch_um_day(
    client: &reqwest::Client,
    symbol: &str,
    date: NaiveDate,
) -> Result<Option<Vec<u8>>, String> {
    let url = um_day_url(symbol, date);
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

/// The days of `(year, month)` from the 1st through `min(month_end,
/// today_utc)`, inclusive. Empty if the month hasn't started yet as of
/// `today_utc`. Pure so the "which days does the open month need" decision
/// is testable without a clock or network.
pub fn open_month_days(year: i32, month: u32, today_utc: NaiveDate) -> Vec<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid year/month");
    if first > today_utc {
        return Vec::new();
    }
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("valid year/month");
    let month_end = next_month_first
        .pred_opt()
        .expect("month has at least one day");
    let last = month_end.min(today_utc);

    let mut days = Vec::new();
    let mut day = first;
    while day <= last {
        days.push(day);
        day = day.succ_opt().expect("day within a bounded month range");
    }
    days
}

/// Epochs arrive in ms (pre-2025 files) or µs (2025+). Values, not flags.
///
/// `pub(crate)`: `binance_micro` reuses this exact autodetect for
/// aggTrades/fundingRate epochs rather than re-deriving the threshold.
pub(crate) fn epoch_utc(raw: i64) -> Result<DateTime<Utc>, String> {
    let micros = if raw > 100_000_000_000_000 {
        raw
    } else {
        raw * 1000
    };
    Utc.timestamp_micros(micros)
        .single()
        .ok_or_else(|| format!("unreadable epoch {raw}"))
}

/// Parse one monthly zip into validated 1m bars inside [start, end).
pub fn parse_um_klines_zip(
    bytes: &[u8],
    asset: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<Bar>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{asset}: bad zip: {e}"))?;
    let mut csv = String::new();
    archive
        .by_index(0)
        .map_err(|e| format!("{asset}: empty zip: {e}"))?
        .read_to_string(&mut csv)
        .map_err(|e| format!("{asset}: unreadable csv: {e}"))?;

    let mut bars = Vec::new();
    for line in csv.lines() {
        if line.is_empty() || line.starts_with("open_time") {
            continue; // UM monthly files sometimes carry a header row
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 11 {
            return Err(format!(
                "{asset}: kline row has {} columns: {line}",
                cols.len()
            ));
        }
        let ts = epoch_utc(
            cols[0]
                .parse()
                .map_err(|e| format!("{asset}: {e}: {line}"))?,
        )?;
        if ts < start || ts >= end {
            continue;
        }
        let f = |i: usize| -> Result<f64, String> {
            cols[i]
                .parse()
                .map_err(|e| format!("{asset}: col {i}: {e}: {line}"))
        };
        let bar = Bar {
            ts_utc: ts,
            asset: asset.to_string(),
            interval_s: 60,
            open: f(1)?,
            high: f(2)?,
            low: f(3)?,
            close: f(4)?,
            volume: f(5)?,
            quote_volume: Some(f(7)?),
            trades: Some(
                cols[8]
                    .parse()
                    .map_err(|e| format!("{asset}: col 8: {e}"))?,
            ),
        };
        bar.validate()?;
        bars.push(bar);
    }
    bars.sort_by_key(|b| b.ts_utc);
    bars.dedup_by_key(|b| b.ts_utc);
    Ok(bars)
}

#[cfg(test)]
pub(crate) mod tests {
    use chrono::{TimeZone, Utc};

    /// `pub(crate)`: `binance_micro`'s tests reuse this fixture builder
    /// rather than duplicating it.
    pub(crate) fn zip_with_one_file(name: &str, content: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut z = zip::ZipWriter::new(&mut buf);
        z.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(content).unwrap();
        z.finish().unwrap();
        buf.into_inner()
    }

    #[test]
    fn hl_coins_map_to_um_symbols() {
        assert_eq!(super::binance_um_symbol("BTC").as_deref(), Some("BTCUSDT"));
        assert_eq!(
            super::binance_um_symbol("kPEPE").as_deref(),
            Some("1000PEPEUSDT")
        );
        assert_eq!(
            super::binance_um_symbol("kBONK").as_deref(),
            Some("1000BONKUSDT")
        );
        assert_eq!(
            super::binance_um_symbol("HYPE"),
            None,
            "not listed on Binance UM"
        );
    }

    #[test]
    fn um_kline_zips_parse_with_and_without_header_and_in_ms_or_us() {
        // Two rows: one microsecond epoch (Binance switched in 2025), one with the
        // CSV header UM monthly files carry. 12 columns per kline row.
        let csv = "open_time,open,high,low,close,volume,close_time,quote_volume,count,taker_buy_volume,taker_buy_quote_volume,ignore\n\
            1754956800000000,64000.1,64010.0,63990.0,64005.5,12.5,1754956859999999,800070.0,42,6.0,384033.0,0\n\
            1754956860000000,64005.5,64020.0,64000.0,64018.0,8.25,1754956919999999,528148.0,30,4.1,262476.0,0\n";
        let bytes = zip_with_one_file("BTCUSDT-1m-2025-08.csv", csv.as_bytes());
        // 1_754_956_800 is 2025-08-12T00:00:00Z; the window brackets that date
        // (the fixture's epoch, not the calendar year the file happens to be
        // named after, is what has to fall inside [start, end)).
        let start = Utc.with_ymd_and_hms(2025, 8, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 9, 1, 0, 0, 0).unwrap();
        let bars = super::parse_um_klines_zip(&bytes, "BTC", start, end).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].asset, "BTC");
        assert_eq!(bars[0].interval_s, 60);
        assert_eq!(bars[0].ts_utc.timestamp(), 1_754_956_800);
        assert_eq!(bars[0].close, 64005.5);
        assert_eq!(bars[0].quote_volume, Some(800070.0));
        assert_eq!(bars[0].trades, Some(42));
        // Millisecond epochs (pre-2025 files) parse to the same instant.
        let csv_ms = "1754956800000,64000.1,64010.0,63990.0,64005.5,12.5,1754956859999,800070.0,42,6.0,384033.0,0\n";
        let bytes_ms = zip_with_one_file("BTCUSDT-1m-2025-08.csv", csv_ms.as_bytes());
        let bars_ms = super::parse_um_klines_zip(&bytes_ms, "BTC", start, end).unwrap();
        assert_eq!(bars_ms[0].ts_utc, bars[0].ts_utc);
    }

    #[test]
    fn daily_url_shape() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        assert_eq!(
            super::um_day_url("BTCUSDT", d),
            "https://data.binance.vision/data/futures/um/daily/klines/BTCUSDT/1m/BTCUSDT-1m-2026-08-13.zip"
        );
    }

    #[test]
    fn open_month_days_mid_month_today_covers_the_1st_through_today() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        let days = super::open_month_days(2026, 8, today);
        assert_eq!(
            days.first(),
            Some(&chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap())
        );
        assert_eq!(days.last(), Some(&today));
        assert_eq!(days.len(), 13);
    }

    #[test]
    fn open_month_days_a_past_month_returns_the_full_month() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        let days = super::open_month_days(2026, 7, today);
        assert_eq!(
            days.first(),
            Some(&chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap())
        );
        assert_eq!(
            days.last(),
            Some(&chrono::NaiveDate::from_ymd_opt(2026, 7, 31).unwrap())
        );
        assert_eq!(days.len(), 31);
    }

    #[test]
    fn open_month_days_a_future_month_is_empty() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        assert!(super::open_month_days(2026, 9, today).is_empty());
    }

    #[test]
    fn open_month_days_today_is_the_1st_yields_one_day() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let days = super::open_month_days(2026, 8, today);
        assert_eq!(days, vec![today]);
    }

    #[test]
    fn rows_outside_the_window_are_dropped() {
        let csv = "1754956800000,1,1,1,1,1,1754956859999,1,1,1,1,0\n";
        let bytes = zip_with_one_file("BTCUSDT-1m-2026-08.csv", csv.as_bytes());
        let start = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        assert!(super::parse_um_klines_zip(&bytes, "BTC", start, end)
            .unwrap()
            .is_empty());
    }
}
