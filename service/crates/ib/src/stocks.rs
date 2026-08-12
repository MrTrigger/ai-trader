//! Reusable Interactive Brokers stock market-data source.
//!
//! This module owns IB contract resolution and wire-level data semantics. A
//! portfolio bot receives the owned records below; it never constructs an IB
//! request or interprets an IB tick. Execution support belongs beside this
//! module in the `ib` crate when added, not in a Stockholm bot crate.

use std::collections::BTreeSet;
use std::path::Path;

use ibapi::contracts::tick_types::TickType;
use ibapi::contracts::{Contract, ContractDetails, SecurityIdType, SecurityType};
use ibapi::market_data::historical::{BarSize, BarTimestamp, Duration, WhatToShow};
use ibapi::prelude::{Client, StreamExt, SubscriptionItem, TickTypes};
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use venue::VenueError;

use crate::{connect_verified, GatewayConfig};

const SNAPSHOT_TIMEOUT_S: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StockQuery {
    #[serde(default)]
    pub conid: Option<i32>,
    #[serde(default)]
    pub isin: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub primary_exchange: Option<String>,
    #[serde(default = "sek")]
    pub currency: String,
}

fn sek() -> String {
    "SEK".into()
}

impl StockQuery {
    pub fn by_conid(conid: i32) -> Self {
        Self {
            conid: Some(conid),
            isin: None,
            symbol: None,
            primary_exchange: None,
            currency: sek(),
        }
    }

    pub fn by_symbol(symbol: impl Into<String>, primary_exchange: impl Into<String>) -> Self {
        Self {
            conid: None,
            isin: None,
            symbol: Some(symbol.into()),
            primary_exchange: Some(primary_exchange.into()),
            currency: sek(),
        }
    }

    pub fn by_isin(isin: impl Into<String>, primary_exchange: impl Into<String>) -> Self {
        Self {
            conid: None,
            isin: Some(isin.into()),
            symbol: None,
            primary_exchange: Some(primary_exchange.into()),
            currency: sek(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.conid.is_none()
            && self.isin.as_deref().is_none_or(str::is_empty)
            && self.symbol.as_deref().is_none_or(str::is_empty)
        {
            return Err("stock query requires conid, ISIN, or symbol".into());
        }
        if self.currency.trim().is_empty() {
            return Err("stock query currency is empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedStock {
    pub conid: i32,
    pub symbol: String,
    pub local_symbol: String,
    pub primary_exchange: String,
    pub currency: String,
    pub isin: Option<String>,
    pub long_name: String,
    pub stock_type: String,
    pub min_tick: f64,
    #[serde(skip)]
    contract: Contract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DailySeries {
    Trades,
    AdjustedLast,
    FeeRate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyBar {
    #[serde(with = "date_serde")]
    pub date: Date,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: Option<f64>,
    pub trades: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyCoverage {
    pub observations: usize,
    pub first_date: Option<String>,
    pub last_date: Option<String>,
}

impl DailyCoverage {
    pub fn from_bars(bars: &[DailyBar]) -> Self {
        let first_date = bars
            .iter()
            .map(|bar| bar.date)
            .min()
            .map(|date| date.to_string());
        let last_date = bars
            .iter()
            .map(|bar| bar.date)
            .max()
            .map(|date| date.to_string());
        Self {
            observations: bars.len(),
            first_date,
            last_date,
        }
    }
}

/// Serializable IB identity safe to persist with research data. This is kept
/// separate from [`ResolvedStock`], whose private wire contract must only be
/// constructed by a live, verified IB resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StockIdentity {
    pub conid: i32,
    pub symbol: String,
    pub local_symbol: String,
    pub primary_exchange: String,
    pub currency: String,
    pub isin: Option<String>,
    pub long_name: String,
    pub stock_type: String,
    pub min_tick: f64,
}

impl From<&ResolvedStock> for StockIdentity {
    fn from(stock: &ResolvedStock) -> Self {
        Self {
            conid: stock.conid,
            symbol: stock.symbol.clone(),
            local_symbol: stock.local_symbol.clone(),
            primary_exchange: stock.primary_exchange.clone(),
            currency: stock.currency.clone(),
            isin: stock.isin.clone(),
            long_name: stock.long_name.clone(),
            stock_type: stock.stock_type.clone(),
            min_tick: stock.min_tick,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StockHistoryRecord {
    pub format_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub retrieved_at: OffsetDateTime,
    pub requested_years: i32,
    pub stock: StockIdentity,
    pub series: DailySeries,
    pub coverage: DailyCoverage,
    pub bars: Vec<DailyBar>,
}

/// Load and validate immutable history records produced by the shared IB
/// collector. Consumers select the series explicitly; unrelated archives in
/// the same directory are ignored.
pub fn load_history_records(
    root: &Path,
    series: DailySeries,
) -> Result<Vec<StockHistoryRecord>, String> {
    let audit_path = root.join("audit.json");
    let audit_bytes = std::fs::read(&audit_path)
        .map_err(|error| format!("cannot read completed {}: {error}", audit_path.display()))?;
    let audit: serde_json::Value = serde_json::from_slice(&audit_bytes)
        .map_err(|error| format!("{}: {error}", audit_path.display()))?;
    if audit.get("format_version").and_then(|value| value.as_str())
        != Some("ib-stockholm-main-history-audit-1")
    {
        return Err(format!(
            "{} has an unsupported format",
            audit_path.display()
        ));
    }
    let total_field = match series {
        DailySeries::Trades => "trades_with_data",
        DailySeries::AdjustedLast => "adjusted_last_with_data",
        DailySeries::FeeRate => "fee_rate_with_data",
    };
    let expected_records = audit
        .pointer(&format!("/totals/{total_field}"))
        .and_then(|value| value.as_u64())
        .ok_or_else(|| format!("{} lacks totals.{total_field}", audit_path.display()))?
        as usize;
    let audit_modified = std::fs::metadata(&audit_path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| format!("cannot stat {}: {error}", audit_path.display()))?;
    let directory = root.join("series");
    let entries = std::fs::read_dir(&directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    let mut records = Vec::new();
    let mut identities = BTreeSet::new();
    for path in entries.filter_map(|entry| entry.ok().map(|entry| entry.path())) {
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let record: StockHistoryRecord = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if record.series != series {
            continue;
        }
        let modified = std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
        if modified > audit_modified {
            return Err(format!(
                "{} is newer than {}; the collection is incomplete or stale",
                path.display(),
                audit_path.display()
            ));
        }
        validate_history_record(&record).map_err(|error| format!("{}: {error}", path.display()))?;
        if !identities.insert(record.stock.conid) {
            return Err(format!(
                "duplicate IB contract {} in {}",
                record.stock.conid,
                directory.display()
            ));
        }
        records.push(record);
    }
    records.sort_by_key(|record| record.stock.conid);
    if records.len() != expected_records {
        return Err(format!(
            "{} declares {expected_records} {:?} histories but {} validated records exist",
            audit_path.display(),
            series,
            records.len()
        ));
    }
    Ok(records)
}

fn validate_history_record(record: &StockHistoryRecord) -> Result<(), String> {
    if record.format_version != "ib-stock-daily-history-1"
        || record.requested_years <= 0
        || record.stock.conid <= 0
        || record.stock.currency.trim().is_empty()
        || record.stock.local_symbol.trim().is_empty()
        || record.coverage != DailyCoverage::from_bars(&record.bars)
    {
        return Err("invalid IB stock-history identity or coverage".into());
    }
    if record
        .bars
        .windows(2)
        .any(|pair| pair[0].date >= pair[1].date)
    {
        return Err("IB stock-history dates are not strictly increasing".into());
    }
    for bar in &record.bars {
        let values = [bar.open, bar.high, bar.low, bar.close];
        if values.iter().any(|value| !value.is_finite())
            || bar.low > bar.high
            || bar.open < bar.low
            || bar.open > bar.high
            || bar.close < bar.low
            || bar.close > bar.high
            || (record.series == DailySeries::FeeRate && values.iter().any(|value| *value < 0.0))
            || (record.series != DailySeries::FeeRate && values.iter().any(|value| *value <= 0.0))
        {
            return Err(format!("invalid {:?} bar on {}", record.series, bar.date));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StockSnapshot {
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub last: Option<f64>,
    pub bid_size: Option<f64>,
    pub ask_size: Option<f64>,
    /// IB's numeric shortability tier. Preserve the broker value; portfolio
    /// policy decides whether it is acceptable.
    pub shortable: Option<f64>,
    pub available_shares: Option<f64>,
}

pub struct StockDataSource {
    client: Client,
    account: String,
    paper: bool,
}

impl StockDataSource {
    pub async fn connect(cfg: GatewayConfig) -> Result<Self, VenueError> {
        let account = cfg.account.clone();
        let (client, paper) = connect_verified(&cfg).await?;
        Ok(Self {
            client,
            account,
            paper,
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn is_paper(&self) -> bool {
        self.paper
    }

    pub async fn resolve(&self, query: &StockQuery) -> Result<ResolvedStock, VenueError> {
        query.validate().map_err(VenueError::Unreachable)?;
        let mut probe = Contract {
            contract_id: query.conid.unwrap_or_default(),
            security_type: SecurityType::Stock,
            exchange: "SMART".into(),
            currency: query.currency.as_str().into(),
            ..Default::default()
        };
        if let Some(symbol) = &query.symbol {
            probe.symbol = symbol.as_str().into();
        }
        if let Some(isin) = &query.isin {
            probe.security_id_type = Some(SecurityIdType::Isin);
            probe.security_id = isin.clone();
        }
        if let Some(exchange) = &query.primary_exchange {
            probe.primary_exchange = exchange.as_str().into();
        }
        let details = self
            .client
            .contract_details(&probe)
            .await
            .map_err(unreachable)?;
        select_contract(query, details).map_err(VenueError::Unreachable)
    }

    /// Request the most recent `years` of daily data. IB requires
    /// `ADJUSTED_LAST` to be anchored at now, so this API intentionally has no
    /// arbitrary end date. Historical backfills are versioned by the caller.
    pub async fn daily_history(
        &self,
        stock: &ResolvedStock,
        series: DailySeries,
        years: i32,
    ) -> Result<Vec<DailyBar>, VenueError> {
        if !(1..=20).contains(&years) {
            return Err(VenueError::Unreachable(format!(
                "IB daily history years must be in 1..=20, got {years}"
            )));
        }
        let what = match series {
            DailySeries::Trades => WhatToShow::Trades,
            DailySeries::AdjustedLast => WhatToShow::AdjustedLast,
            DailySeries::FeeRate => WhatToShow::FeeRate,
        };
        let history = self
            .client
            .historical_data(&stock.contract, BarSize::Day)
            .what_to_show(what)
            .duration(Duration::years(years))
            .fetch()
            .await
            .map_err(unreachable)?;
        history
            .bars
            .into_iter()
            .map(|bar| {
                let BarTimestamp::Date(date) = bar.date else {
                    return Err(VenueError::Unreachable(
                        "IB returned an intraday timestamp for a daily stock request".into(),
                    ));
                };
                Ok(DailyBar {
                    date,
                    open: bar.open,
                    high: bar.high,
                    low: bar.low,
                    close: bar.close,
                    volume: (series == DailySeries::Trades).then_some(bar.volume),
                    trades: (series == DailySeries::Trades).then_some(bar.count),
                })
            })
            .collect()
    }

    /// Fetch and package one immutable historical series. The returned record
    /// contains no live wire contract and can be persisted by any research
    /// collector without importing IB protocol details.
    pub async fn history_record(
        &self,
        stock: &ResolvedStock,
        series: DailySeries,
        years: i32,
    ) -> Result<StockHistoryRecord, VenueError> {
        let bars = self.daily_history(stock, series, years).await?;
        Ok(StockHistoryRecord {
            format_version: "ib-stock-daily-history-1".into(),
            retrieved_at: OffsetDateTime::now_utc(),
            requested_years: years,
            stock: StockIdentity::from(stock),
            series,
            coverage: DailyCoverage::from_bars(&bars),
            bars,
        })
    }

    /// Current L1 and account-visible shortable quantity. This is a snapshot,
    /// never a historical availability claim.
    pub async fn snapshot(&self, stock: &ResolvedStock) -> Result<StockSnapshot, VenueError> {
        let mut subscription = self
            .client
            .market_data(&stock.contract)
            .generic_ticks(&["236"])
            // IB rejects generic tick 236 on a one-shot snapshot. Use a
            // bounded streaming subscription and cancel it ourselves.
            .streaming()
            .subscribe()
            .await
            .map_err(unreachable)?;
        let mut out = StockSnapshot {
            taken_at: OffsetDateTime::now_utc(),
            bid: None,
            ask: None,
            last: None,
            bid_size: None,
            ask_size: None,
            shortable: None,
            available_shares: None,
        };
        let read = async {
            while let Some(item) = subscription.next().await {
                let SubscriptionItem::Data(item) = item.map_err(unreachable)? else {
                    continue;
                };
                match item {
                    TickTypes::Price(tick) => match tick.tick_type {
                        TickType::Bid | TickType::DelayedBid => out.bid = positive(tick.price),
                        TickType::Ask | TickType::DelayedAsk => out.ask = positive(tick.price),
                        TickType::Last | TickType::DelayedLast => out.last = positive(tick.price),
                        _ => {}
                    },
                    TickTypes::Size(tick) => match tick.tick_type {
                        TickType::BidSize | TickType::DelayedBidSize => {
                            out.bid_size = non_negative(tick.size)
                        }
                        TickType::AskSize | TickType::DelayedAskSize => {
                            out.ask_size = non_negative(tick.size)
                        }
                        TickType::ShortableShares => out.available_shares = non_negative(tick.size),
                        _ => {}
                    },
                    TickTypes::PriceSize(tick) => {
                        match tick.price_tick_type {
                            TickType::Bid | TickType::DelayedBid => out.bid = positive(tick.price),
                            TickType::Ask | TickType::DelayedAsk => out.ask = positive(tick.price),
                            TickType::Last | TickType::DelayedLast => {
                                out.last = positive(tick.price)
                            }
                            _ => {}
                        }
                        match tick.size_tick_type {
                            TickType::BidSize | TickType::DelayedBidSize => {
                                out.bid_size = non_negative(tick.size)
                            }
                            TickType::AskSize | TickType::DelayedAskSize => {
                                out.ask_size = non_negative(tick.size)
                            }
                            TickType::ShortableShares => {
                                out.available_shares = non_negative(tick.size)
                            }
                            _ => {}
                        }
                    }
                    TickTypes::Generic(tick) if tick.tick_type == TickType::Shortable => {
                        out.shortable = non_negative(tick.value)
                    }
                    _ => {}
                }
                if (out.bid.is_some() || out.ask.is_some() || out.last.is_some())
                    && out.shortable.is_some()
                    && out.available_shares.is_some()
                {
                    break;
                }
            }
            Ok::<(), VenueError>(())
        };
        // A partial snapshot at the timeout is still useful and truthfully
        // carries `None` for fields IB did not send. No ticks at all is an
        // unreachable/data-permission failure.
        if let Ok(result) =
            tokio::time::timeout(std::time::Duration::from_secs(SNAPSHOT_TIMEOUT_S), read).await
        {
            result?;
        }
        subscription.cancel().await;
        if out.bid.is_none()
            && out.ask.is_none()
            && out.last.is_none()
            && out.shortable.is_none()
            && out.available_shares.is_none()
        {
            return Err(VenueError::Unreachable(
                "IB stock snapshot returned no price or shortability ticks".into(),
            ));
        }
        out.taken_at = OffsetDateTime::now_utc();
        Ok(out)
    }
}

fn positive(value: f64) -> Option<f64> {
    value.is_finite().then_some(value).filter(|v| *v > 0.0)
}

fn non_negative(value: f64) -> Option<f64> {
    value.is_finite().then_some(value).filter(|v| *v >= 0.0)
}

fn unreachable(error: impl std::fmt::Display) -> VenueError {
    VenueError::Unreachable(error.to_string())
}

mod date_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::Date;

    pub fn serialize<S: Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&date.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
        let value = String::deserialize(deserializer)?;
        let format = time::macros::format_description!("[year]-[month]-[day]");
        Date::parse(&value, format).map_err(serde::de::Error::custom)
    }
}

fn select_contract(
    query: &StockQuery,
    details: Vec<ContractDetails>,
) -> Result<ResolvedStock, String> {
    let mut matches = details
        .into_iter()
        .filter(|detail| detail.contract.security_type == SecurityType::Stock)
        .filter(|detail| {
            query
                .conid
                .is_none_or(|conid| detail.contract.contract_id == conid)
        })
        .filter(|detail| {
            query.isin.as_deref().is_none_or(|isin| {
                detail.sec_id_list.iter().any(|entry| {
                    entry.tag.eq_ignore_ascii_case("ISIN") && entry.value.eq_ignore_ascii_case(isin)
                })
            })
        })
        .filter(|detail| {
            query.symbol.as_deref().is_none_or(|symbol| {
                detail
                    .contract
                    .symbol
                    .to_string()
                    .eq_ignore_ascii_case(symbol)
                    || detail.contract.local_symbol.eq_ignore_ascii_case(symbol)
            })
        })
        .filter(|detail| {
            detail
                .contract
                .currency
                .to_string()
                .eq_ignore_ascii_case(&query.currency)
        })
        .filter(|detail| {
            query.primary_exchange.as_deref().is_none_or(|exchange| {
                detail
                    .contract
                    .primary_exchange
                    .to_string()
                    .eq_ignore_ascii_case(exchange)
            })
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(format!("no IB stock contract matches {query:?}"));
    }
    if matches.len() != 1 {
        let ids = matches
            .iter()
            .map(|detail| detail.contract.contract_id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "ambiguous IB stock query {query:?}; matching conids: {ids}"
        ));
    }
    let detail = matches.pop().expect("one match");
    let contract = detail.contract;
    let isin = detail
        .sec_id_list
        .iter()
        .find(|entry| entry.tag.eq_ignore_ascii_case("ISIN"))
        .map(|entry| entry.value.clone());
    Ok(ResolvedStock {
        conid: contract.contract_id,
        symbol: contract.symbol.to_string(),
        local_symbol: contract.local_symbol.clone(),
        primary_exchange: contract.primary_exchange.to_string(),
        currency: contract.currency.to_string(),
        isin,
        long_name: detail.long_name,
        stock_type: detail.stock_type,
        min_tick: detail.min_tick,
        contract,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ibapi::contracts::TagValue;

    fn detail(conid: i32, local_symbol: &str, exchange: &str) -> ContractDetails {
        ContractDetails {
            contract: Contract {
                contract_id: conid,
                symbol: "VOLV".into(),
                local_symbol: local_symbol.into(),
                primary_exchange: exchange.into(),
                currency: "SEK".into(),
                security_type: SecurityType::Stock,
                ..Default::default()
            },
            long_name: "Volvo AB".into(),
            min_tick: 0.1,
            ..Default::default()
        }
    }

    #[test]
    fn resolution_requires_identity() {
        let query = StockQuery {
            conid: None,
            isin: None,
            symbol: None,
            primary_exchange: None,
            currency: "SEK".into(),
        };
        assert!(query
            .validate()
            .unwrap_err()
            .contains("conid, ISIN, or symbol"));
    }

    #[test]
    fn conid_resolution_is_unambiguous() {
        let resolved = select_contract(
            &StockQuery::by_conid(917920),
            vec![
                detail(111, "VOLV A", "SFB"),
                detail(917920, "VOLV B", "SFB"),
            ],
        )
        .unwrap();
        assert_eq!(resolved.conid, 917920);
        assert_eq!(resolved.local_symbol, "VOLV B");
    }

    #[test]
    fn symbol_resolution_fails_on_multiple_share_classes() {
        let error = select_contract(
            &StockQuery {
                conid: None,
                isin: None,
                symbol: Some("VOLV".into()),
                primary_exchange: Some("SFB".into()),
                currency: "SEK".into(),
            },
            vec![
                detail(111, "VOLV A", "SFB"),
                detail(917920, "VOLV B", "SFB"),
            ],
        )
        .unwrap_err();
        assert!(error.contains("ambiguous"));
    }

    #[test]
    fn isin_resolution_selects_the_exact_share_class() {
        let mut a = detail(111, "VOLV A", "SFB");
        a.sec_id_list.push(TagValue {
            tag: "ISIN".into(),
            value: "SE0000115420".into(),
        });
        let mut b = detail(917920, "VOLV B", "SFB");
        b.sec_id_list.push(TagValue {
            tag: "ISIN".into(),
            value: "SE0000115446".into(),
        });
        let resolved =
            select_contract(&StockQuery::by_isin("SE0000115446", "SFB"), vec![a, b]).unwrap();
        assert_eq!(resolved.conid, 917920);
        assert_eq!(resolved.isin.as_deref(), Some("SE0000115446"));
    }

    #[test]
    fn invalid_snapshot_numbers_are_absent() {
        assert_eq!(positive(-1.0), None);
        assert_eq!(positive(f64::NAN), None);
        assert_eq!(non_negative(0.0), Some(0.0));
    }

    #[test]
    fn coverage_uses_date_extrema_not_response_order() {
        let bar = |date: Date| DailyBar {
            date,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: None,
            trades: None,
        };
        let bars = vec![
            bar(Date::from_calendar_date(2025, time::Month::January, 3).unwrap()),
            bar(Date::from_calendar_date(2025, time::Month::January, 1).unwrap()),
        ];
        assert_eq!(
            DailyCoverage::from_bars(&bars),
            DailyCoverage {
                observations: 2,
                first_date: Some("2025-01-01".into()),
                last_date: Some("2025-01-03".into()),
            }
        );
    }

    #[test]
    fn fee_history_preserves_decimal_annual_rates_and_rejects_negative_values() {
        let date = Date::from_calendar_date(2025, time::Month::January, 3).unwrap();
        let mut record = StockHistoryRecord {
            format_version: "ib-stock-daily-history-1".into(),
            retrieved_at: OffsetDateTime::UNIX_EPOCH,
            requested_years: 10,
            stock: StockIdentity {
                conid: 917920,
                symbol: "VOLV.B".into(),
                local_symbol: "VOLV B".into(),
                primary_exchange: "SFB".into(),
                currency: "SEK".into(),
                isin: Some("SE0000115446".into()),
                long_name: "Volvo AB".into(),
                stock_type: "COMMON".into(),
                min_tick: 0.1,
            },
            series: DailySeries::FeeRate,
            coverage: DailyCoverage {
                observations: 1,
                first_date: Some(date.to_string()),
                last_date: Some(date.to_string()),
            },
            bars: vec![DailyBar {
                date,
                open: 0.012,
                high: 0.012,
                low: 0.012,
                close: 0.012,
                volume: None,
                trades: None,
            }],
        };
        validate_history_record(&record).unwrap();
        assert_eq!(record.bars[0].close, 0.012);
        record.bars[0].close = -0.01;
        assert!(validate_history_record(&record).is_err());
    }
}
