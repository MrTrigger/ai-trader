use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, TimeZone, Utc};
use features_crypto::Bar;

pub struct Binance {
    client: reqwest::blocking::Client,
}

impl Binance {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(StdDuration::from_secs(20))
            .user_agent("ai-trader-rust/0.1 (public market data)")
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }

    pub fn fetch(
        &self,
        asset: &str,
        interval_s: i32,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Bar>, String> {
        let interval = match interval_s {
            60 => "1m",
            300 => "5m",
            900 => "15m",
            3_600 => "1h",
            14_400 => "4h",
            86_400 => "1d",
            _ => return Err(format!("unsupported Binance interval {interval_s}")),
        };
        let symbol = format!("{}USDT", asset.to_uppercase());
        let mut cursor = start;
        let mut rows = Vec::new();
        while cursor < end {
            let mut last = None;
            for attempt in 0..3 {
                let response = self
                    .client
                    .get("https://api.binance.com/api/v3/klines")
                    .query(&[
                        ("symbol", symbol.as_str()),
                        ("interval", interval),
                        ("startTime", &cursor.timestamp_millis().to_string()),
                        ("endTime", &end.timestamp_millis().to_string()),
                        ("limit", "1000"),
                    ])
                    .send();
                match response {
                    Ok(r) if r.status().is_success() => {
                        let batch: Vec<Vec<serde_json::Value>> =
                            r.json().map_err(|e| e.to_string())?;
                        if batch.is_empty() {
                            return Ok(rows);
                        }
                        for k in &batch {
                            let num = |i: usize| {
                                k.get(i)
                                    .and_then(|v| v.as_str())
                                    .ok_or_else(|| format!("{symbol}: malformed kline field {i}"))?
                                    .parse::<f64>()
                                    .map_err(|e| e.to_string())
                            };
                            let open_ms = k
                                .first()
                                .and_then(|v| v.as_i64())
                                .ok_or_else(|| format!("{symbol}: malformed open time"))?;
                            let ts = Utc
                                .timestamp_millis_opt(open_ms)
                                .single()
                                .ok_or("bad Binance timestamp")?;
                            if ts >= end {
                                continue;
                            }
                            rows.push(Bar {
                                ts_utc: ts,
                                asset: asset.to_uppercase(),
                                interval_s,
                                open: num(1)?,
                                high: num(2)?,
                                low: num(3)?,
                                close: num(4)?,
                                volume: num(5)?,
                                quote_volume: Some(num(7)?),
                                trades: k.get(8).and_then(|v| v.as_i64()),
                            });
                        }
                        let next =
                            rows.last().unwrap().ts_utc + Duration::seconds(interval_s as i64);
                        if next <= cursor {
                            return Err(format!("{symbol}: upstream made no progress"));
                        }
                        cursor = next;
                        if batch.len() < 1000 {
                            return Ok(rows);
                        }
                        std::thread::sleep(StdDuration::from_millis(150));
                        last = None;
                        break;
                    }
                    Ok(r) if r.status().as_u16() == 400 => {
                        return Err(format!(
                            "{symbol}: rejected by upstream: {}",
                            r.text().unwrap_or_default()
                        ))
                    }
                    Ok(r) => last = Some(format!("HTTP {}", r.status())),
                    Err(e) => last = Some(e.to_string()),
                }
                std::thread::sleep(StdDuration::from_millis(1500 * (attempt + 1)));
            }
            if let Some(error) = last {
                return Err(format!(
                    "{symbol}: data fetch failed after 3 attempts: {error}"
                ));
            }
        }
        Ok(rows)
    }
}
