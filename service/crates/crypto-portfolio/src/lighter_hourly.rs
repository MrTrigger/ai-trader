//! Verified public native hourly candles. Missing and zero-volume hours are
//! never filled or substituted. A completed download is not a depth snapshot.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Timelike, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub ts_utc: DateTime<Utc>,
    pub asset: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub base_volume: Decimal,
    pub quote_volume: Decimal,
}

impl Bar {
    pub fn traded(&self) -> bool {
        self.base_volume > Decimal::ZERO && self.quote_volume > Decimal::ZERO
    }

    /// Quote/base volume, available only for traded hours with consistent data.
    /// A 1e-12 relative tolerance permits JSON floating-point rounding. Values
    /// are never clamped to OHLC; material inconsistencies return None.
    pub fn vwap(&self) -> Option<Decimal> {
        if !self.traded() {
            return None;
        }
        let value = self.quote_volume.checked_div(self.base_volume)?;
        let tolerance = self.high * Decimal::new(1, 12);
        (value >= self.low - tolerance && value <= self.high + tolerance).then_some(value)
    }
}

#[derive(Debug)]
pub struct UnusableVwap {
    pub asset: String,
    pub ts_utc: DateTime<Utc>,
    pub raw_source: String,
}

#[derive(Debug)]
pub struct History {
    /// Keys are UTC bar starts. Bar values become fully observed one hour later.
    pub bars: BTreeMap<DateTime<Utc>, BTreeMap<String, Bar>>,
    pub market_ids: BTreeMap<String, u16>,
    pub start: DateTime<Utc>,
    pub end_exclusive: DateTime<Utc>,
    pub page_sha256: BTreeMap<String, String>,
    /// Raw OHLC and volumes remain available; vwap() refuses these rows.
    pub unusable_vwap: Vec<UnusableVwap>,
}

/// Validated daily label opens. This sparse collection cannot drive execution.
#[derive(Debug)]
pub struct LabelMarks {
    pub opens: BTreeMap<(DateTime<Utc>, String), Decimal>,
    pub page_sha256: BTreeMap<String, String>,
}

impl LabelMarks {
    /// Validate source provenance and timestamps throughout, and OHLC at 01:00 UTC.
    pub fn load(root: &Path, daily_market_ids: &BTreeMap<String, u16>) -> Result<Self, String> {
        let history = History::load_scoped(root, daily_market_ids, Some(1))?;
        Ok(Self {
            opens: history
                .bars
                .into_iter()
                .flat_map(|(at, bars)| {
                    bars.into_iter()
                        .map(move |(asset, bar)| ((at, asset), bar.open))
                })
                .collect(),
            page_sha256: history.page_sha256,
        })
    }
}

#[derive(Deserialize)]
struct Completion {
    schema: String,
    selected_sha256: String,
    pages: usize,
    resolution: String,
    start_inclusive: DateTime<Utc>,
    end_exclusive: DateTime<Utc>,
}

#[derive(Deserialize)]
struct Selected {
    asset: String,
    symbol: String,
    market_id: u16,
    first_eligible_decision: DateTime<Utc>,
    last_eligible_decision: DateTime<Utc>,
}

fn read(root: &Path, name: &str, hashes: &mut BTreeMap<String, String>) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(root.join(name)).map_err(|e| format!("{name}: {e}"))?;
    hashes.insert(name.into(), format!("{:x}", Sha256::digest(&bytes)));
    Ok(bytes)
}

fn verified(
    root: &Path,
    name: &str,
    marker: &str,
    hashes: &mut BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    let bytes = read(root, name, hashes)?;
    let sha = read(root, marker, hashes)?;
    let expected = std::str::from_utf8(&sha)
        .map_err(|e| e.to_string())?
        .split_whitespace()
        .next();
    if expected != Some(hashes[name].as_str()) {
        return Err(format!("{name}: SHA256 mismatch"));
    }
    Ok(bytes)
}

fn number(value: &Value, omitted_zero: bool) -> Result<Decimal, String> {
    if value.is_null() && omitted_zero {
        return Ok(Decimal::ZERO);
    }
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err("missing or invalid candle decimal".into()),
    };
    text.parse()
        .or_else(|_| Decimal::from_scientific(&text))
        .map_err(|e| format!("invalid candle decimal: {e}"))
}

fn candle_timestamp(value: &Value, asset: &str) -> Result<DateTime<Utc>, String> {
    let millis = value["t"].as_i64().ok_or("missing candle timestamp")?;
    if millis.rem_euclid(3_600_000) != 0 {
        return Err(format!(
            "{asset}: timestamp {millis} is not an hourly UTC start"
        ));
    }
    DateTime::from_timestamp_millis(millis).ok_or_else(|| "invalid candle timestamp".into())
}

fn candle(value: &Value, asset: &str) -> Result<Bar, String> {
    let bar = Bar {
        ts_utc: candle_timestamp(value, asset)?,
        asset: asset.into(),
        open: number(&value["o"], false)?,
        high: number(&value["h"], false)?,
        low: number(&value["l"], false)?,
        close: number(&value["c"], false)?,
        base_volume: number(&value["v"], true)?,
        quote_volume: number(&value["V"], true)?,
    };
    if bar.low <= Decimal::ZERO
        || bar.high < bar.low
        || bar.open < bar.low
        || bar.open > bar.high
        || bar.close < bar.low
        || bar.close > bar.high
        || bar.base_volume < Decimal::ZERO
        || bar.quote_volume < Decimal::ZERO
        || bar.base_volume.is_zero() != bar.quote_volume.is_zero()
    {
        return Err(format!("{asset} {}: invalid OHLC or volume", bar.ts_utc));
    }
    Ok(bar)
}

impl History {
    pub fn load_many(
        roots: &[PathBuf],
        daily_market_ids: &BTreeMap<String, u16>,
    ) -> Result<Self, String> {
        let mut combined: Option<Self> = None;
        for root in roots {
            let mut next = Self::load(root, daily_market_ids)?;
            next.page_sha256 = next
                .page_sha256
                .into_iter()
                .map(|(name, hash)| (root.join(name).display().to_string(), hash))
                .collect();
            if let Some(history) = &mut combined {
                history.merge(next)?;
            } else {
                combined = Some(next);
            }
        }
        combined.ok_or_else(|| "at least one hourly collection is required".into())
    }

    fn merge(&mut self, next: Self) -> Result<(), String> {
        for (asset, id) in &next.market_ids {
            if self.market_ids.get(asset).is_some_and(|old| old != id) {
                return Err(format!("conflicting hourly market identity for {asset}"));
            }
        }
        for (at, bars) in &next.bars {
            for (asset, bar) in bars {
                if self
                    .bars
                    .get(at)
                    .and_then(|row| row.get(asset))
                    .is_some_and(|old| old != bar)
                {
                    return Err(format!("conflicting hourly candle for {asset} at {at}"));
                }
            }
        }
        for (name, hash) in &next.page_sha256 {
            if self.page_sha256.get(name).is_some_and(|old| old != hash) {
                return Err(format!("conflicting hourly source hash for {name}"));
            }
        }
        self.market_ids.extend(next.market_ids);
        for (at, bars) in next.bars {
            self.bars.entry(at).or_default().extend(bars);
        }
        self.page_sha256.extend(next.page_sha256);
        self.unusable_vwap.extend(next.unusable_vwap);
        self.start = self.start.min(next.start);
        self.end_exclusive = self.end_exclusive.max(next.end_exclusive);
        Ok(())
    }

    /// Validate the declared selected subset against the native daily catalog.
    /// Every selected market must have each hourly bar from first eligibility
    /// through the declared end. Earlier missing history stays absent.
    pub fn load(root: &Path, daily_market_ids: &BTreeMap<String, u16>) -> Result<Self, String> {
        Self::load_scoped(root, daily_market_ids, None)
    }

    fn load_scoped(
        root: &Path,
        daily_market_ids: &BTreeMap<String, u16>,
        label_hour: Option<u32>,
    ) -> Result<Self, String> {
        let mut hashes = BTreeMap::new();
        let complete: Completion =
            serde_json::from_slice(&read(root, "COMPLETE.json", &mut hashes)?)
                .map_err(|e| format!("COMPLETE.json: {e}"))?;
        if complete.schema != "lighter-native-hourly-candle-collection-1"
            || complete.resolution != "1h"
            || complete.start_inclusive >= complete.end_exclusive
            || complete.start_inclusive.timestamp().rem_euclid(3600) != 0
            || complete.end_exclusive.timestamp().rem_euclid(3600) != 0
            || complete.start_inclusive.timestamp_subsec_nanos() != 0
            || complete.end_exclusive.timestamp_subsec_nanos() != 0
        {
            return Err("invalid hourly collection declaration".into());
        }
        let selected: Vec<Selected> = serde_json::from_slice(&verified(
            root,
            "selected-markets.json",
            "selected-markets.sha256",
            &mut hashes,
        )?)
        .map_err(|e| format!("selected-markets.json: {e}"))?;
        if selected.is_empty() || complete.selected_sha256 != hashes["selected-markets.json"] {
            return Err("empty or mismatched selected-market declaration".into());
        }
        let mut market_ids = BTreeMap::new();
        let mut ids = BTreeSet::new();
        let mut expected = BTreeMap::new();
        let mut expected_jobs = BTreeSet::new();
        for market in &selected {
            if market.asset != market.symbol
                || market.asset.is_empty()
                || !market.asset.chars().all(|c| c.is_ascii_alphanumeric())
                || daily_market_ids.get(&market.asset) != Some(&market.market_id)
                || market_ids
                    .insert(market.asset.clone(), market.market_id)
                    .is_some()
                || !ids.insert(market.market_id)
                || market.first_eligible_decision < complete.start_inclusive
                || market.first_eligible_decision > market.last_eligible_decision
                || market.last_eligible_decision >= complete.end_exclusive
                || [
                    market.first_eligible_decision,
                    market.last_eligible_decision,
                ]
                .iter()
                .any(|d| {
                    d.time().num_seconds_from_midnight() != 0 || d.timestamp_subsec_nanos() != 0
                })
            {
                return Err(format!(
                    "{}: invalid selected identity or eligibility dates",
                    market.asset
                ));
            }
            let mut at = complete.start_inclusive;
            while at < complete.end_exclusive {
                let end = (at + Duration::hours(480)).min(complete.end_exclusive);
                let name = format!(
                    "market-{}-{}-{}.json",
                    market.market_id,
                    at.timestamp(),
                    end.timestamp()
                );
                expected.insert(name, (market.asset.clone(), at, end));
                expected_jobs.insert(format!(
                    "{}\t{}\t{}\t{}",
                    market.market_id,
                    at.timestamp(),
                    end.timestamp(),
                    market.asset
                ));
                at = end;
            }
        }
        if expected.len() != complete.pages {
            return Err("hourly page count does not cover the declared grid".into());
        }
        let jobs = read(root, "jobs.tsv", &mut hashes)?;
        let jobs: Vec<_> = std::str::from_utf8(&jobs)
            .map_err(|e| e.to_string())?
            .lines()
            .collect();
        if jobs.len() != expected_jobs.len()
            || jobs.iter().copied().collect::<BTreeSet<_>>()
                != expected_jobs.iter().map(String::as_str).collect()
        {
            return Err("jobs.tsv differs from the complete selected hourly grid".into());
        }
        read(root, "downloader.sh", &mut hashes)?;
        let inputs = read(root, "inputs.sha256", &mut hashes)?;
        let mut manifest_names = BTreeSet::new();
        for line in std::str::from_utf8(&inputs)
            .map_err(|e| e.to_string())?
            .lines()
        {
            let (sha, path) = line.split_once("  ").ok_or("malformed inputs.sha256")?;
            let name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("invalid manifest filename")?;
            if !["selected-markets.json", "jobs.tsv", "downloader.sh"].contains(&name)
                || !manifest_names.insert(name)
                || hashes.get(name).map(String::as_str) != Some(sha)
            {
                return Err(format!("{name}: input provenance mismatch"));
            }
        }
        if manifest_names.len() != 3 {
            return Err("incomplete input provenance manifest".into());
        }
        for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("invalid hourly filename")?;
            if name.starts_with("market-")
                && name.ends_with(".json")
                && !expected.contains_key(name)
            {
                return Err(format!("unexpected hourly page {name}"));
            }
        }
        let mut bars = BTreeMap::<DateTime<Utc>, BTreeMap<String, Bar>>::new();
        let mut unusable_vwap = Vec::new();
        for (name, (asset, start, end)) in expected {
            let bytes = verified(root, &name, &format!("{name}.sha256"), &mut hashes)?;
            let page: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {e}"))?;
            if page["code"] != 200 || page["r"] != "1h" {
                return Err(format!("{name}: unsuccessful or non-hourly response"));
            }
            for value in page["c"]
                .as_array()
                .ok_or_else(|| format!("{name}: missing candle array"))?
            {
                let at = candle_timestamp(value, &asset).map_err(|e| format!("{name}: {e}"))?;
                if label_hour.is_some_and(|hour| at.hour() != hour) {
                    continue;
                }
                let bar = candle(value, &asset).map_err(|e| format!("{name}: {e}"))?;
                if bar.ts_utc < start || bar.ts_utc >= end {
                    continue;
                }
                let date = bar.ts_utc;
                let day = bars.entry(date).or_default();
                if let Some(old) = day.get(&asset) {
                    if old != &bar {
                        return Err(format!("{name}: conflicting duplicate {asset} at {date}"));
                    }
                    continue;
                }
                if bar.traded() && bar.vwap().is_none() {
                    unusable_vwap.push(UnusableVwap {
                        asset: asset.clone(),
                        ts_utc: date,
                        raw_source: name.clone(),
                    });
                }
                day.insert(asset.clone(), bar);
            }
        }
        for market in selected {
            let mut at = market.first_eligible_decision;
            while at < complete.end_exclusive {
                if label_hour.is_none_or(|hour| at.hour() == hour)
                    && !bars
                        .get(&at)
                        .is_some_and(|hour| hour.contains_key(&market.asset))
                {
                    return Err(format!(
                        "{}: missing native hour {at} after first eligibility",
                        market.asset
                    ));
                }
                at += Duration::hours(1);
            }
        }
        Ok(Self {
            bars,
            market_ids,
            start: complete.start_inclusive,
            end_exclusive: complete.end_exclusive,
            page_sha256: hashes,
            unusable_vwap,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn save(root: &Path, name: &str, value: &Value) {
        let bytes = serde_json::to_vec(value).unwrap();
        std::fs::write(root.join(name), &bytes).unwrap();
        std::fs::write(
            root.join(format!("{name}.sha256")),
            format!("{:x}", Sha256::digest(bytes)),
        )
        .unwrap();
    }
    fn fixture() -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "lighter-hourly-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let selected = json!([{"asset":"BTC","symbol":"BTC","market_id":1,"first_eligible_decision":"2026-01-01T00:00:00Z","last_eligible_decision":"2026-01-01T00:00:00Z"}]);
        save(&root, "selected-markets.json", &selected);
        std::fs::rename(
            root.join("selected-markets.json.sha256"),
            root.join("selected-markets.sha256"),
        )
        .unwrap();
        let selected_sha = std::fs::read_to_string(root.join("selected-markets.sha256")).unwrap();
        save(
            &root,
            "COMPLETE.json",
            &json!({"schema":"lighter-native-hourly-candle-collection-1","resolution":"1h","selected_sha256":selected_sha,"pages":1,"start_inclusive":"2026-01-01T00:00:00Z","end_exclusive":"2026-01-01T02:00:00Z"}),
        );
        std::fs::write(root.join("jobs.tsv"), "1\t1767225600\t1767232800\tBTC\n").unwrap();
        std::fs::write(root.join("downloader.sh"), "fixture").unwrap();
        let mut input = String::new();
        for name in ["selected-markets.json", "jobs.tsv", "downloader.sh"] {
            input.push_str(&format!(
                "{:x}  {name}\n",
                Sha256::digest(std::fs::read(root.join(name)).unwrap())
            ));
        }
        std::fs::write(root.join("inputs.sha256"), input).unwrap();
        save(
            &root,
            "market-1-1767225600-1767232800.json",
            &json!({"code":200,"r":"1h","c":[{"t":1767225600000i64,"o":10,"h":12,"l":9,"c":11,"v":2,"V":22},{"t":1767229200000i64,"o":11,"h":11,"l":11,"c":11}]}),
        );
        Fixture(root)
    }
    fn ids() -> BTreeMap<String, u16> {
        BTreeMap::from([("BTC".into(), 1), ("ETH".into(), 0)])
    }
    #[test]
    fn label_marks_scope_prices_but_preserve_timestamp_and_hash_validation() {
        let source = fixture();
        let name = "market-1-1767225600-1767232800.json";
        let mut page: Value =
            serde_json::from_slice(&std::fs::read(source.0.join(name)).unwrap()).unwrap();
        page["c"][0]["o"] = json!(100);
        save(&source.0, name, &page);
        assert!(History::load(&source.0, &ids())
            .unwrap_err()
            .contains("invalid OHLC"));
        let marks = LabelMarks::load(&source.0, &ids()).unwrap();
        assert_eq!(marks.opens.len(), 1);
        assert_eq!(*marks.opens.values().next().unwrap(), Decimal::from(11));
        page["c"][1]["o"] = json!(100);
        save(&source.0, name, &page);
        assert!(LabelMarks::load(&source.0, &ids())
            .unwrap_err()
            .contains("invalid OHLC"));
        page["c"][1]["o"] = json!(11);
        page["c"][0]["t"] = json!(1767225600001i64);
        save(&source.0, name, &page);
        assert!(LabelMarks::load(&source.0, &ids())
            .unwrap_err()
            .contains("hourly UTC start"));
        page["c"][0]["t"] = json!(1767225600000i64);
        save(&source.0, name, &page);
        std::fs::write(source.0.join(format!("{name}.sha256")), "incorrect").unwrap();
        assert!(LabelMarks::load(&source.0, &ids())
            .unwrap_err()
            .contains("SHA256 mismatch"));
    }

    #[test]
    fn multiple_collections_verify_each_source_and_reject_changed_overlap() {
        let first = fixture();
        let second = fixture();
        let roots = vec![first.0.clone(), second.0.clone()];
        let combined = History::load_many(&roots, &ids()).unwrap();
        assert_eq!(combined.bars, History::load(&first.0, &ids()).unwrap().bars);
        assert!(combined
            .page_sha256
            .contains_key(&second.0.join("COMPLETE.json").display().to_string()));
        let name = "market-1-1767225600-1767232800.json";
        let mut page: Value =
            serde_json::from_slice(&std::fs::read(second.0.join(name)).unwrap()).unwrap();
        page["c"][0]["o"] = json!(11);
        save(&second.0, name, &page);
        assert!(History::load_many(&roots, &ids())
            .err()
            .unwrap()
            .contains("conflicting hourly candle"));
        assert!(History::load_many(&[], &ids()).is_err());
    }

    #[test]
    fn merging_extends_coverage_without_filling_gaps_or_mutating_on_conflict() {
        let source = fixture();
        let mut first = History::load(&source.0, &ids()).unwrap();
        let original = first.bars.clone();
        let mut next = History::load(&source.0, &ids()).unwrap();
        let at = next.end_exclusive + Duration::hours(1);
        let mut bar = next.bars.values().next().unwrap()["BTC"].clone();
        bar.ts_utc = at;
        next.bars.insert(at, BTreeMap::from([("BTC".into(), bar)]));
        next.end_exclusive = at + Duration::hours(1);
        first.merge(next).unwrap();
        assert!(first.bars.contains_key(&at));
        assert!(!first.bars.contains_key(&(at - Duration::hours(1))));
        for (at, bars) in original {
            assert_eq!(first.bars[&at], bars);
        }
        let mut conflict = History::load(&source.0, &ids()).unwrap();
        conflict.market_ids.insert("BTC".into(), 2);
        let before = first.bars.clone();
        assert!(first.merge(conflict).is_err());
        assert_eq!(first.bars, before);
    }

    #[test]
    fn subset_grid_zero_volume_and_vwap_are_preserved() {
        let f = fixture();
        let h = History::load(&f.0, &ids()).unwrap();
        assert_eq!(h.market_ids.len(), 1);
        assert_eq!(h.bars.len(), 2);
        let bars: Vec<_> = h.bars.values().map(|v| &v["BTC"]).collect();
        assert_eq!(bars[0].vwap(), Some(Decimal::from(11)));
        assert!(!bars[1].traded());
        assert_eq!(bars[1].vwap(), None);
    }
    #[test]
    fn tampered_missing_or_wrong_identity_sources_fail() {
        let f = fixture();
        assert!(History::load(&f.0, &BTreeMap::from([("BTC".into(), 2)])).is_err());
        std::fs::write(f.0.join("market-1-1767225600-1767232800.json"), "{}").unwrap();
        assert!(History::load(&f.0, &ids()).is_err());
        let f = fixture();
        std::fs::remove_file(f.0.join("market-1-1767225600-1767232800.json.sha256")).unwrap();
        assert!(History::load(&f.0, &ids()).is_err());
    }
    #[test]
    fn missing_after_eligibility_and_invalid_ohlc_fail() {
        let f = fixture();
        save(
            &f.0,
            "market-1-1767225600-1767232800.json",
            &json!({"code":200,"r":"1h","c":[]}),
        );
        assert!(History::load(&f.0, &ids())
            .err()
            .unwrap()
            .contains("missing native hour"));
        assert!(candle(
            &json!({"t":1767225600000i64,"o":10,"h":9,"l":8,"c":8}),
            "BTC"
        )
        .is_err());
    }
    #[test]
    fn materially_inconsistent_vwap_is_unavailable_without_repair() {
        let b = candle(
            &json!({"t":1767225600000i64,"o":10,"h":12,"l":9,"c":11,"v":2,"V":100}),
            "BTC",
        )
        .unwrap();
        assert!(b.traded());
        assert_eq!(b.vwap(), None);
        assert_eq!(b.quote_volume, Decimal::from(100));
    }
}
