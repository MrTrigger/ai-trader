//! Native daily Lighter candles and a causal execution-universe screen.
//!
//! Current status and the last observed trade date never determine historical
//! membership. A market must have been created before the decision and have
//! positive native base AND quote volume on the preceding completed UTC day.
//! This conservative activity screen is not a listing/delisting ledger.
//! Zero-volume candles remain available for valuation and audit, not selection.
//! Inconsistent OHLC bounds are quarantined with raw-source provenance; no price
//! is repaired. Call validate_period before replay: quarantine inside the test
//! period or its 30-day liquidity warmup fails. Other malformed input fails load.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use features_crypto::Bar;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{config::Config, store, universe};

const DAY: i64 = 86_400;
const PAGE_DAYS: i64 = 480;
const SOURCE: &str = "lighter-created-before-decision-and-prior-completed-day-positive-native-volume; signal-store-liquidity-top30; conservative-activity-screen-not-listing-ledger";

pub struct History {
    /// All validated daily candles before exclusive_end, including zero volume.
    pub daily: Vec<Bar>,
    pub created_at: BTreeMap<String, DateTime<Utc>>,
    pub market_ids: BTreeMap<String, u16>,
    pub exclusive_end: DateTime<Utc>,
    /// Relative input filenames and their verified SHA256 digests.
    pub page_sha256: BTreeMap<String, String>,
    pub quarantined: Vec<QuarantinedCandle>,
    activity: BTreeMap<DateTime<Utc>, BTreeSet<String>>,
}

#[derive(Deserialize)]
struct Markets {
    code: u16,
    order_books: Vec<Market>,
}

#[derive(Deserialize)]
struct Market {
    market_id: u16,
    symbol: String,
    market_type: String,
    created_at: Option<String>,
}

#[derive(Deserialize)]
struct Page {
    code: u16,
    r: String,
    c: Vec<Candle>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Candle {
    t: i64,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
    #[serde(rename = "V")]
    quote_volume: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuarantinedCandle {
    pub asset: String,
    pub date: DateTime<Utc>,
    pub reason: String,
    pub raw_sources: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SelectedMarket {
    pub asset: String,
    pub symbol: String,
    pub market_id: u16,
    pub first_eligible_decision: DateTime<Utc>,
    pub last_eligible_decision: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct MarketCoverage {
    pub asset: String,
    pub market_id: u16,
    pub created_at: DateTime<Utc>,
    pub first_candle: Option<DateTime<Utc>>,
    pub last_candle: Option<DateTime<Utc>>,
    pub candle_days: usize,
    pub positive_volume_days: usize,
}

#[derive(Debug, Serialize)]
pub struct DecisionCoverage {
    pub as_of: DateTime<Utc>,
    pub active_native_markets: usize,
    pub eligible_markets: usize,
}

#[derive(Debug, Serialize)]
pub struct Coverage {
    pub method: String,
    pub signal_root: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub exclusive_native_end: DateTime<Utc>,
    pub top_n: usize,
    pub lookback_days: i64,
    pub min_history_bars: u32,
    pub min_dollar_volume: String,
    pub input_sha256: BTreeMap<String, String>,
    pub markets: Vec<MarketCoverage>,
    pub quarantined: Vec<QuarantinedCandle>,
    pub decisions: Vec<DecisionCoverage>,
    pub selected_markets: Vec<SelectedMarket>,
}

fn midnight(value: DateTime<Utc>) -> Result<(), String> {
    if value.timestamp().rem_euclid(DAY) != 0 || value.timestamp_subsec_nanos() != 0 {
        return Err(format!("expected UTC midnight, got {value}"));
    }
    Ok(())
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn verified(root: &Path, name: &str, hash_name: &str) -> Result<(Vec<u8>, String), String> {
    let bytes = read(&root.join(name))?;
    let hash_bytes = read(&root.join(hash_name))?;
    let recorded = std::str::from_utf8(&hash_bytes)
        .map_err(|e| e.to_string())?
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("empty hash file {hash_name}"))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if recorded != actual {
        return Err(format!("SHA256 mismatch for {name}"));
    }
    Ok((bytes, actual))
}

fn validate(c: &Candle) -> Result<DateTime<Utc>, String> {
    let stamp = DateTime::from_timestamp_millis(c.t)
        .ok_or_else(|| format!("invalid candle timestamp {}", c.t))?;
    midnight(stamp)?;
    if c.t <= 0
        || [c.o, c.h, c.l, c.c]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
    {
        return Err(format!("nonpositive or nonfinite candle prices at {stamp}"));
    }
    if [c.v, c.quote_volume]
        .iter()
        .any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(format!("negative or nonfinite volume at {stamp}"));
    }
    Ok(stamp)
}

impl History {
    /// Load every expected page in the collector's 480-day pagination layout.
    /// Missing pages or hashes fail, including those for currently inactive perps.
    /// count_back may overfetch before a page start; identical duplicates merge.
    /// The snapshot's current status and undocumented candle `i` field are unused.
    pub fn load(root: &Path) -> Result<Self, String> {
        let (bytes, hash) = verified(root, "market-snapshot.json", "market-snapshot.sha256")?;
        let markets: Markets = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if markets.code != 200 {
            return Err(format!("market snapshot API code {}", markets.code));
        }
        let end_bytes = read(&root.join("exclusive-end-day.txt"))?;
        let end_date = NaiveDate::parse_from_str(
            std::str::from_utf8(&end_bytes)
                .map_err(|e| e.to_string())?
                .trim(),
            "%Y-%m-%d",
        )
        .map_err(|e| format!("exclusive-end-day.txt: {e}"))?;
        let exclusive_end = end_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let mut page_sha256 = BTreeMap::from([
            ("market-snapshot.json".into(), hash),
            (
                "exclusive-end-day.txt".into(),
                format!("{:x}", Sha256::digest(&end_bytes)),
            ),
        ]);
        let mut created_at = BTreeMap::new();
        let mut market_ids = BTreeMap::new();
        let mut ids = BTreeSet::new();
        let mut daily = Vec::new();
        let mut quarantined = Vec::new();
        for market in markets.order_books {
            if market.market_type == "spot" {
                continue;
            }
            if market.market_type != "perp"
                || market.symbol.is_empty()
                || !market.symbol.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err("invalid native market type or symbol".into());
            }
            if !ids.insert(market.market_id) || market_ids.contains_key(&market.symbol) {
                return Err("duplicate native perpetual id or symbol".into());
            }
            let millis = market
                .created_at
                .as_deref()
                .ok_or("perp has no created_at")?
                .parse::<i64>()
                .map_err(|e| format!("created_at: {e}"))?;
            let created = DateTime::from_timestamp_millis(millis)
                .filter(|_| millis > 0)
                .ok_or("invalid created_at milliseconds")?;
            created_at.insert(market.symbol.clone(), created);
            market_ids.insert(market.symbol.clone(), market.market_id);
            let mut start = millis / 1000 / DAY * DAY;
            let mut candles = BTreeMap::<DateTime<Utc>, (Candle, Vec<String>)>::new();
            while start < exclusive_end.timestamp() {
                let end = (start + PAGE_DAYS * DAY).min(exclusive_end.timestamp());
                let name = format!("market-{}-{start}-{end}.json", market.market_id);
                let (bytes, hash) = verified(root, &name, &format!("{name}.sha256"))?;
                let page: Page =
                    serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {e}"))?;
                if page.code != 200 || page.r != "1d" {
                    return Err(format!("{name}: expected code200 and resolution1d"));
                }
                page_sha256.insert(name.clone(), hash);
                for candle in page.c {
                    let stamp = validate(&candle).map_err(|e| format!("{name}: {e}"))?;
                    // Backward overfetch is permitted, forward page spill is not.
                    if stamp.timestamp() >= end {
                        return Err(format!(
                            "{name}: candle at/after exclusive page end {stamp}"
                        ));
                    }
                    if let Some((previous, sources)) = candles.get_mut(&stamp) {
                        if *previous != candle {
                            return Err(format!(
                                "conflicting duplicate candle {} {stamp}",
                                market.symbol
                            ));
                        }
                        if !sources.contains(&name) {
                            sources.push(name.clone());
                        }
                    } else {
                        candles.insert(stamp, (candle, vec![name.clone()]));
                    }
                }
                start = end;
            }
            for (ts_utc, (c, raw_sources)) in candles {
                if c.h < c.o.max(c.c) || c.l > c.o.min(c.c) || c.h < c.l {
                    quarantined.push(QuarantinedCandle {
                        asset: market.symbol.clone(),
                        date: ts_utc,
                        reason: format!(
                            "inconsistent OHLC: open={} high={} low={} close={}",
                            c.o, c.h, c.l, c.c
                        ),
                        raw_sources,
                    });
                    continue;
                }
                daily.push(Bar {
                    ts_utc,
                    asset: market.symbol.clone(),
                    interval_s: DAY as i32,
                    open: c.o,
                    high: c.h,
                    low: c.l,
                    close: c.c,
                    volume: c.v,
                    quote_volume: Some(c.quote_volume),
                    trades: None,
                });
            }
        }
        if market_ids.is_empty() || daily.is_empty() {
            return Err("no native perpetual history".into());
        }
        daily.sort_by(|a, b| a.asset.cmp(&b.asset).then(a.ts_utc.cmp(&b.ts_utc)));
        let mut history = Self {
            daily,
            created_at,
            market_ids,
            exclusive_end,
            page_sha256,
            quarantined,
            activity: BTreeMap::new(),
        };
        history.index_activity();
        Ok(history)
    }

    fn index_activity(&mut self) {
        self.activity.clear();
        for bar in &self.daily {
            if bar.volume > 0.0 && bar.quote_volume.is_some_and(|v| v > 0.0) {
                self.activity
                    .entry(bar.ts_utc)
                    .or_default()
                    .insert(bar.asset.clone());
            }
        }
    }

    /// Membership at a daily decision uses exactly the previous UTC day.
    /// An absent/zero-volume previous candle excludes the market; later candles
    /// cannot fill that gap or retroactively establish a delisting date.
    pub fn active_at(&self, as_of: DateTime<Utc>) -> Result<BTreeSet<String>, String> {
        midnight(as_of)?;
        if as_of > self.exclusive_end {
            return Err("decision exceeds native history coverage".into());
        }
        Ok(self
            .activity
            .get(&(as_of - Duration::days(1)))
            .into_iter()
            .flatten()
            .filter(|asset| {
                self.created_at
                    .get(*asset)
                    .is_some_and(|created| *created < as_of)
            })
            .cloned()
            .collect())
    }

    pub fn members_at(
        &self,
        signal_bars: &[Bar],
        cfg: &Config,
        as_of: DateTime<Utc>,
    ) -> Result<Vec<universe::Member>, String> {
        let active = self.active_at(as_of)?;
        let minimum = cfg
            .min_dollar_volume
            .to_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or("invalid minimum dollar volume")?;
        universe::by_liquidity(
            signal_bars,
            as_of,
            30,
            30,
            cfg.min_history_bars as usize,
            minimum,
            Some(&active),
        )
    }

    /// Quarantined candles may not alter the requested period or its 30-day
    /// liquidity warmup. Execution still must reject missing held valuations.
    pub fn validate_period(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<(), String> {
        midnight(start)?;
        midnight(end)?;
        if start > end || end > self.exclusive_end {
            return Err("invalid native history period".into());
        }
        if let Some(bad) = self
            .quarantined
            .iter()
            .find(|q| q.date >= start - Duration::days(30) && q.date <= end)
        {
            return Err(format!(
                "quarantined {} candle {} intersects evaluation/warmup: {}",
                bad.asset, bad.date, bad.reason
            ));
        }
        Ok(())
    }

    /// Write a separate causal universe and downloader coverage. Signal bars and
    /// funding remain owned by the caller; this method creates no data symlinks.
    pub fn build_universes(
        &self,
        signal_root: &Path,
        output_root: &Path,
        cfg: &Config,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Coverage, String> {
        midnight(start)?;
        midnight(end)?;
        if start > end || end > self.exclusive_end {
            return Err("invalid universe date range".into());
        }
        if cfg.interval_s != DAY || cfg.min_history_bars == 0 {
            return Err("daily config with positive history is required".into());
        }
        self.validate_period(start, end)?;
        for name in ["coverage.json", "selected-markets.json"] {
            if output_root.join(name).exists() {
                return Err(format!(
                    "output already exists: {}",
                    output_root.join(name).display()
                ));
            }
        }
        let signal_bars = store::read(signal_root, DAY as i32)?;
        if signal_bars.is_empty() {
            return Err("signal store has no daily bars".into());
        }
        let mut snapshots = Vec::new();
        let mut decisions = Vec::new();
        let mut selected = BTreeMap::<String, SelectedMarket>::new();
        let mut day = start;
        while day <= end {
            if universe::path(output_root, day).exists() {
                return Err(format!("output universe already exists at {day}"));
            }
            // Same-day opens are needed by the eventual replay, even though
            // membership itself must only see prior completed candles.
            if !signal_bars.iter().any(|b| b.ts_utc == day) {
                return Err(format!("signal store lacks daily valuation bars at {day}"));
            }
            let members = self.members_at(&signal_bars, cfg, day)?;
            for member in members.iter().filter(|m| m.eligible) {
                selected
                    .entry(member.asset.clone())
                    .and_modify(|v| v.last_eligible_decision = day)
                    .or_insert(SelectedMarket {
                        asset: member.asset.clone(),
                        symbol: member.asset.clone(),
                        market_id: self.market_ids[&member.asset],
                        first_eligible_decision: day,
                        last_eligible_decision: day,
                    });
            }
            decisions.push(DecisionCoverage {
                as_of: day,
                active_native_markets: self.active_at(day)?.len(),
                eligible_markets: members.iter().filter(|m| m.eligible).count(),
            });
            snapshots.push((day, members));
            day += Duration::days(1);
        }
        let markets = self
            .market_ids
            .iter()
            .map(|(asset, id)| {
                let bars: Vec<_> = self.daily.iter().filter(|b| &b.asset == asset).collect();
                MarketCoverage {
                    asset: asset.clone(),
                    market_id: *id,
                    created_at: self.created_at[asset],
                    first_candle: bars.first().map(|b| b.ts_utc),
                    last_candle: bars.last().map(|b| b.ts_utc),
                    candle_days: bars.len(),
                    positive_volume_days: bars
                        .iter()
                        .filter(|b| b.volume > 0.0 && b.quote_volume.is_some_and(|v| v > 0.0))
                        .count(),
                }
            })
            .collect();
        let coverage = Coverage {
            method: SOURCE.into(),
            signal_root: signal_root.display().to_string(),
            start,
            end,
            exclusive_native_end: self.exclusive_end,
            top_n: 30,
            lookback_days: 30,
            min_history_bars: cfg.min_history_bars,
            min_dollar_volume: cfg.min_dollar_volume.to_string(),
            input_sha256: self.page_sha256.clone(),
            markets,
            quarantined: self.quarantined.clone(),
            decisions,
            selected_markets: selected.into_values().collect(),
        };
        for (day, members) in snapshots {
            universe::write(output_root, day, SOURCE, members, false)?;
        }
        for (name, value) in [
            ("coverage.json", serde_json::to_value(&coverage)),
            (
                "selected-markets.json",
                serde_json::to_value(&coverage.selected_markets),
            ),
        ] {
            let value = value.map_err(|e| e.to_string())?;
            std::fs::write(
                output_root.join(name),
                serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(coverage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "lighter-history-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn put(&self, name: &str, value: &serde_json::Value) {
            let bytes = serde_json::to_vec(value).unwrap();
            std::fs::write(self.0.join(name), &bytes).unwrap();
            let hash_name = if name == "market-snapshot.json" {
                "market-snapshot.sha256".into()
            } else {
                format!("{name}.sha256")
            };
            std::fs::write(
                self.0.join(hash_name),
                format!("{:x}  {name}\n", Sha256::digest(&bytes)),
            )
            .unwrap();
        }
        fn config(&self) -> Config {
            let path = self.0.join("config.toml");
            std::fs::write(&path, include_str!("../../../../config/default.toml")).unwrap();
            Config::load(&path).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn day(value: &str) -> DateTime<Utc> {
        format!("{value}T00:00:00Z").parse().unwrap()
    }
    fn candle(stamp: DateTime<Utc>, volume: f64) -> serde_json::Value {
        json!({"t":stamp.timestamp_millis(),"o":10.0,"h":11.0,"l":9.0,"c":10.5,"v":volume,"V":volume*10.0,"i":123})
    }
    fn page_name(id: u16) -> String {
        format!(
            "market-{id}-{}-{}.json",
            day("2026-01-01").timestamp(),
            day("2026-01-04").timestamp()
        )
    }
    fn fixture() -> Fixture {
        let f = Fixture::new();
        std::fs::write(f.0.join("exclusive-end-day.txt"), "2026-01-04\n").unwrap();
        f.put("market-snapshot.json",&json!({"code":200,"order_books":[
            {"market_id":1,"symbol":"BTC","market_type":"perp","status":"inactive","created_at":(day("2026-01-01")+Duration::hours(7)).timestamp_millis().to_string()},
            {"market_id":2,"symbol":"1000PEPE","market_type":"perp","status":"active","created_at":day("2026-01-01").timestamp_millis().to_string()},
            {"market_id":3,"symbol":"SPOT","market_type":"spot"}
        ]}));
        for id in [1, 2] {
            f.put(&page_name(id),&json!({"code":200,"r":"1d","c":[candle(day("2026-01-01"),1.0),candle(day("2026-01-02"),0.0),candle(day("2026-01-03"),2.0)]}));
        }
        f
    }

    #[test]
    fn inactive_historical_activity_and_native_units_survive_but_zero_day_is_not_active() {
        let f = fixture();
        let h = History::load(&f.0).unwrap();
        assert_eq!(h.daily.len(), 6);
        assert!(h.active_at(day("2026-01-01")).unwrap().is_empty());
        assert_eq!(
            h.active_at(day("2026-01-02")).unwrap(),
            BTreeSet::from(["BTC".into(), "1000PEPE".into()])
        );
        assert!(h.active_at(day("2026-01-03")).unwrap().is_empty());
        assert!(!h.market_ids.contains_key("PEPE"));
        assert!(h.daily.iter().all(|b| b.open == 10.0 && b.trades.is_none()));
        assert!(h.active_at(day("2026-01-02") + Duration::hours(1)).is_err());
        assert!(h.active_at(day("2026-01-05")).is_err());
    }

    #[test]
    fn creation_timestamp_overrides_any_backfilled_prelisting_candle() {
        let f = fixture();
        let mut h = History::load(&f.0).unwrap();
        h.created_at.insert("BTC".into(), day("2026-01-02"));
        assert!(!h.active_at(day("2026-01-02")).unwrap().contains("BTC"));
    }

    #[test]
    fn hashes_resolution_bad_prices_and_conflicting_duplicates_fail() {
        let f = fixture();
        let name = page_name(1);
        std::fs::write(f.0.join(&name), "{}").unwrap();
        assert!(History::load(&f.0).err().unwrap().contains("SHA256"));
        for bad in [
            json!({"code":500,"r":"1d","c":[]}),
            json!({"code":200,"r":"1h","c":[]}),
        ] {
            f.put(&name, &bad);
            assert!(History::load(&f.0).is_err());
        }
        for (key, value) in [
            ("o", json!(0)),
            ("v", json!(-1)),
            ("V", json!(-1)),
            ("t", json!(day("2026-01-01").timestamp())),
        ] {
            let mut c = candle(day("2026-01-01"), 1.0);
            c[key] = value;
            f.put(&name, &json!({"code":200,"r":"1d","c":[c]}));
            assert!(History::load(&f.0).is_err(), "{key}");
        }
        let c = candle(day("2026-01-01"), 1.0);
        f.put(
            &name,
            &json!({"code":200,"r":"1d","c":[c.clone(),c.clone()]}),
        );
        assert_eq!(History::load(&f.0).unwrap().daily.len(), 4);
        f.put(
            &name,
            &json!({"code":200,"r":"1d","c":[c,candle(day("2026-01-01"),2.0)]}),
        );
        assert!(History::load(&f.0)
            .err()
            .unwrap()
            .contains("conflicting duplicate"));
        std::fs::remove_file(f.0.join(name)).unwrap();
        assert!(History::load(&f.0).is_err());
    }

    #[test]
    fn inconsistent_ohlc_is_quarantined_and_blocks_affected_periods() {
        let f = fixture();
        let mut bad = candle(day("2026-01-01"), 1.0);
        bad["c"] = json!(8.0);
        f.put(
            &page_name(1),
            &json!({"code":200,"r":"1d","c":[bad,candle(day("2026-01-03"),2.0)]}),
        );
        let mut h = History::load(&f.0).unwrap();
        assert_eq!(h.quarantined.len(), 1);
        let q = &h.quarantined[0];
        assert_eq!(q.asset, "BTC");
        assert_eq!(q.date, day("2026-01-01"));
        assert_eq!(q.raw_sources, vec![page_name(1)]);
        assert!(serde_json::to_string(q)
            .unwrap()
            .contains("inconsistent OHLC"));
        assert!(!h.active_at(day("2026-01-02")).unwrap().contains("BTC"));
        assert!(h
            .validate_period(day("2026-01-02"), day("2026-01-03"))
            .is_err());
        h.exclusive_end = day("2026-03-01");
        assert!(h
            .validate_period(day("2026-01-31"), day("2026-02-01"))
            .is_err());
        assert!(h
            .validate_period(day("2026-02-01"), day("2026-02-02"))
            .is_ok());
        let as_of = day("2026-02-02");
        h.daily
            .push(signal_bar("BTC", as_of - Duration::days(1), 100.0));
        h.index_activity();
        let signal_root = f.0.join("signal");
        let output_root = f.0.join("output");
        let bars: Vec<_> = (0..101)
            .map(|n| signal_bar("BTC", as_of - Duration::days(n), 10_000_000.0))
            .collect();
        store::write(&signal_root, &bars).unwrap();
        let result = h
            .build_universes(&signal_root, &output_root, &f.config(), as_of, as_of)
            .unwrap();
        assert_eq!(result.selected_markets[0].asset, "BTC");
        assert_eq!(result.selected_markets[0].first_eligible_decision, as_of);
        let saved: serde_json::Value =
            serde_json::from_slice(&read(&output_root.join("coverage.json")).unwrap()).unwrap();
        assert_eq!(saved["quarantined"][0]["asset"], "BTC");
        assert_eq!(saved["quarantined"][0]["raw_sources"][0], page_name(1));
        assert!(h
            .build_universes(&signal_root, &output_root, &f.config(), as_of, as_of)
            .is_err());
    }

    fn signal_bar(asset: &str, stamp: DateTime<Utc>, turnover: f64) -> Bar {
        Bar {
            ts_utc: stamp,
            asset: asset.into(),
            interval_s: DAY as i32,
            open: 10.0,
            high: 11.0,
            low: 9.0,
            close: 10.0,
            volume: turnover / 10.0,
            quote_volume: Some(turnover),
            trades: None,
        }
    }

    #[test]
    fn filter_before_top30_backfills_slots_and_future_candles_do_not_change_prior_members() {
        let f = fixture();
        let mut h = History::load(&f.0).unwrap();
        let cfg = f.config();
        let as_of = day("2026-01-02");
        let mut bars = Vec::new();
        for i in 0..35 {
            let asset = format!("A{i:02}");
            for n in 1..=100 {
                bars.push(signal_bar(
                    &asset,
                    as_of - Duration::days(n),
                    10_000_000.0 + (35 - i) as f64 * 1_000_000.0,
                ));
            }
            if i > 0 {
                h.created_at.insert(asset.clone(), day("2026-01-01"));
                h.market_ids.insert(asset.clone(), 100 + i);
                h.daily
                    .push(signal_bar(&asset, as_of - Duration::days(1), 100.0));
            }
        }
        h.index_activity();
        let before = h.members_at(&bars, &cfg, as_of).unwrap();
        let eligible: Vec<_> = before
            .iter()
            .filter(|m| m.eligible)
            .map(|m| m.asset.as_str())
            .collect();
        assert_eq!(eligible.len(), 30);
        assert!(!eligible.contains(&"A00"));
        assert!(eligible.contains(&"A30"));
        h.daily
            .push(signal_bar("A00", as_of + Duration::days(1), 1e12));
        bars.push(signal_bar("A34", as_of + Duration::days(1), 1e12));
        h.index_activity();
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(h.members_at(&bars, &cfg, as_of).unwrap()).unwrap()
        );
    }
}
