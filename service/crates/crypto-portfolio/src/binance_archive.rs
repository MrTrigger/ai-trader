//! Binance's public monthly archive, including delisted USDT symbols.

use std::io::{Cursor, Read};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use features_crypto::Bar;

const ARCHIVE: &str = "https://data.binance.vision";
const LISTING: &str = "https://s3-ap-northeast-1.amazonaws.com/data.binance.vision";

pub struct BinanceArchive {
    client: reqwest::blocking::Client,
}

fn interval_name(interval_s: i32) -> Result<&'static str, String> {
    match interval_s {
        60 => Ok("1m"),
        300 => Ok("5m"),
        900 => Ok("15m"),
        3_600 => Ok("1h"),
        14_400 => Ok("4h"),
        86_400 => Ok("1d"),
        _ => Err(format!("unsupported Binance archive interval {interval_s}")),
    }
}

impl BinanceArchive {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(StdDuration::from_secs(60))
            .user_agent("ai-trader-rust/0.1 (public archive)")
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { client })
    }

    pub fn fetch(
        &self,
        asset: &str,
        interval_s: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Bar>, String> {
        if start >= end {
            return Err("archive start must precede end".into());
        }
        let interval = interval_name(interval_s)?;
        let asset = asset.to_uppercase();
        let symbol = format!("{asset}USDT");
        let mut rows = Vec::new();
        for month in months(start, end) {
            let name = format!("{symbol}-{interval}-{month}");
            let url = format!("{ARCHIVE}/data/spot/monthly/klines/{symbol}/{interval}/{name}.zip");
            let response = self
                .client
                .get(&url)
                .send()
                .map_err(|error| format!("{name}: archive request failed: {error}"))?;
            if response.status().as_u16() == 404 {
                continue;
            }
            if !response.status().is_success() {
                return Err(format!(
                    "{name}: archive returned HTTP {}",
                    response.status()
                ));
            }
            let bytes = response.bytes().map_err(|error| error.to_string())?;
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
                .map_err(|error| format!("{name}: invalid zip: {error}"))?;
            if archive.is_empty() {
                continue;
            }
            let mut text = String::new();
            archive
                .by_index(0)
                .map_err(|error| format!("{name}: {error}"))?
                .read_to_string(&mut text)
                .map_err(|error| format!("{name}: {error}"))?;
            for (line_number, line) in text.lines().enumerate() {
                let fields = line.split(',').collect::<Vec<_>>();
                if fields
                    .first()
                    .is_none_or(|value| !value.bytes().all(|b| b.is_ascii_digit()))
                {
                    continue;
                }
                if fields.len() < 9 {
                    return Err(format!(
                        "{name}: line {} has fewer than 9 fields",
                        line_number + 1
                    ));
                }
                let parse = |index: usize| {
                    fields[index]
                        .parse::<f64>()
                        .map_err(|error| format!("{name}: field {index}: {error}"))
                };
                let ts = epoch_utc(fields[0], &name)?;
                if ts < start || ts >= end {
                    continue;
                }
                let bar = Bar {
                    ts_utc: ts,
                    asset: asset.clone(),
                    interval_s,
                    open: parse(1)?,
                    high: parse(2)?,
                    low: parse(3)?,
                    close: parse(4)?,
                    volume: parse(5)?,
                    quote_volume: Some(parse(7)?),
                    trades: Some(
                        fields[8]
                            .parse::<i64>()
                            .map_err(|error| format!("{name}: trades: {error}"))?,
                    ),
                };
                bar.validate()?;
                rows.push(bar);
            }
        }
        rows.sort_by_key(|bar| bar.ts_utc);
        rows.dedup_by_key(|bar| bar.ts_utc);
        Ok(rows)
    }

    /// Every USDT base asset represented by the archive, including the dead.
    pub fn listed_assets(&self, include_leveraged: bool) -> Result<Vec<String>, String> {
        let prefix = "data/spot/monthly/klines/";
        let mut marker = String::new();
        let mut found = std::collections::BTreeSet::new();
        loop {
            let body = self
                .client
                .get(LISTING)
                .query(&[
                    ("delimiter", "/"),
                    ("prefix", prefix),
                    ("max-keys", "1000"),
                    ("marker", marker.as_str()),
                ])
                .send()
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?
                .text()
                .map_err(|error| error.to_string())?;
            let open = format!("<Prefix>{prefix}");
            for tail in body.split(&open).skip(1) {
                if let Some(symbol) = tail.split("/</Prefix>").next() {
                    if let Some(asset) = symbol.strip_suffix("USDT") {
                        if !asset.is_empty() && (include_leveraged || !is_leveraged_token(asset)) {
                            found.insert(asset.to_owned());
                        }
                    }
                }
            }
            if !body.contains("<IsTruncated>true</IsTruncated>") {
                break;
            }
            marker = tag(&body, "NextMarker")
                .ok_or("archive listing was truncated without NextMarker")?
                .to_owned();
        }
        Ok(found.into_iter().collect())
    }
}

fn tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    body.split(&open).nth(1)?.split(&close).next()
}

fn months(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<String> {
    let mut year = start.year();
    let mut month = start.month();
    let last_included = end - chrono::Duration::microseconds(1);
    let end_key = (last_included.year(), last_included.month());
    let mut out = Vec::new();
    while (year, month) <= end_key {
        out.push(format!("{year:04}-{month:02}"));
        if month == 12 {
            year += 1;
            month = 1;
        } else {
            month += 1;
        }
    }
    out
}

fn epoch_utc(raw: &str, context: &str) -> Result<DateTime<Utc>, String> {
    let value = raw
        .parse::<i64>()
        .map_err(|error| format!("{context}: timestamp {raw}: {error}"))?;
    let stamp = if value >= 1_000_000_000_000_000 {
        Utc.timestamp_micros(value).single()
    } else if value >= 1_000_000_000_000 {
        Utc.timestamp_millis_opt(value).single()
    } else if value >= 1_000_000_000 {
        Utc.timestamp_opt(value, 0).single()
    } else {
        None
    }
    .ok_or_else(|| format!("{context}: timestamp {value} is invalid in every supported unit"))?;
    let earliest = Utc.with_ymd_and_hms(2015, 1, 1, 0, 0, 0).unwrap();
    let latest = Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap();
    if stamp < earliest || stamp > latest {
        return Err(format!(
            "{context}: timestamp {value} resolves to {stamp}, outside any plausible trading era"
        ));
    }
    Ok(stamp)
}

pub fn is_leveraged_token(asset: &str) -> bool {
    let asset = asset.to_uppercase();
    ["UP", "DOWN", "BEAR", "BULL"]
        .iter()
        .any(|suffix| asset.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_epoch_accepts_milliseconds_and_microseconds() {
        let ms = epoch_utc("1704067200000", "test").unwrap();
        let micros = epoch_utc("1704067200000000", "test").unwrap();
        assert_eq!(ms, micros);
        assert_eq!(ms.to_rfc3339(), "2024-01-01T00:00:00+00:00");
    }

    #[test]
    fn archive_epoch_refuses_silent_1970_unit_errors() {
        assert!(epoch_utc("1704067", "test").is_err());
    }

    #[test]
    fn leveraged_tokens_are_not_historical_assets() {
        assert!(is_leveraged_token("ETHBULL"));
        assert!(is_leveraged_token("adaDown"));
        assert!(!is_leveraged_token("BULLA"));
    }

    #[test]
    fn exclusive_month_boundary_does_not_download_the_next_month() {
        let start = "2024-01-01T00:00:00Z".parse().unwrap();
        let end = "2024-02-01T00:00:00Z".parse().unwrap();
        assert_eq!(months(start, end), ["2024-01"]);
    }
}
