//! Bounded, read-only IB history coverage audit for a Stockholm universe.
//!
//! The input is the universe.json produced by equity-data. Only Main Market
//! Large, Mid, and Small Cap instruments are selected. Successful raw IB
//! series are archived beside audit.json, so a later research build does not
//! have to repeat the broker requests.
//!
//! Usage (with IB_GATEWAY_* and IB_PAPER_ACCOUNT exported):
//! cargo run -p ib --example stock_coverage_audit -- UNIVERSE_JSON OUTPUT_DIR [YEARS] [PER_BUCKET] [PAUSE_MS] [SERIES] [CACHE_DIRS]
//!
//! PER_BUCKET=0 selects the full universe. Start with the default sample of
//! three per bucket; IB historical-data pacing and permissions must be proven
//! before scaling the request count. SERIES is all, prices, or fee-rate.
//! CACHE_DIRS is an OS-separated list of earlier audit roots whose compatible
//! immutable archives may be copied into OUTPUT_DIR instead of re-requested.

use std::path::{Path, PathBuf};

use ib::stocks::{
    DailySeries, ResolvedStock, StockDataSource, StockHistoryRecord, StockIdentity, StockQuery,
};
use ib::GatewayConfig;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

const PRIMARY_EXCHANGE: &str = "SFB";
const MAX_UNPACED_HISTORICAL_REQUESTS: usize = 50;
const FULL_AUDIT_MIN_PAUSE_MS: u64 = 10_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UniverseBucket {
    LargeCap,
    MidCap,
    SmallCap,
    FirstNorthPremier,
    FirstNorth,
}

impl UniverseBucket {
    fn is_main_market(self) -> bool {
        matches!(self, Self::LargeCap | Self::MidCap | Self::SmallCap)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InputInstrument {
    orderbook_id: String,
    isin: String,
    symbol: String,
    name: String,
    currency: String,
    sector: String,
    bucket: UniverseBucket,
    yahoo_symbol: String,
}

#[derive(Debug, Serialize)]
struct SeriesAudit {
    series: DailySeries,
    archive: Option<String>,
    reused_archive: bool,
    reused_from: Option<String>,
    observations: usize,
    first_date: Option<String>,
    last_date: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct InstrumentAudit {
    instrument: InputInstrument,
    ib_stock: Option<StockIdentity>,
    resolution_error: Option<String>,
    series: Vec<SeriesAudit>,
}

#[derive(Debug, Serialize)]
struct AuditTotals {
    instruments_requested: usize,
    instruments_resolved: usize,
    resolution_failures: usize,
    trades_with_data: usize,
    adjusted_last_with_data: usize,
    fee_rate_with_data: usize,
}

#[derive(Debug, Serialize)]
struct AuditReport {
    format_version: String,
    generated_at: String,
    gateway_mode: String,
    primary_exchange: String,
    requested_years: i32,
    per_bucket_limit: usize,
    pause_ms: u64,
    requested_series: Vec<DailySeries>,
    cache_roots: Vec<String>,
    universe_policy: String,
    note: String,
    totals: AuditTotals,
    instruments: Vec<InstrumentAudit>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let universe_path = PathBuf::from(args.next().ok_or(
        "usage: stock_coverage_audit UNIVERSE_JSON OUTPUT_DIR [YEARS] [PER_BUCKET] [PAUSE_MS] [SERIES] [CACHE_DIRS]",
    )?);
    let output_root = PathBuf::from(args.next().ok_or(
        "usage: stock_coverage_audit UNIVERSE_JSON OUTPUT_DIR [YEARS] [PER_BUCKET] [PAUSE_MS] [SERIES] [CACHE_DIRS]",
    )?);
    let years: i32 = args.next().unwrap_or_else(|| "10".into()).parse()?;
    let per_bucket: usize = args.next().unwrap_or_else(|| "3".into()).parse()?;
    let pause_ms: u64 = args.next().unwrap_or_else(|| "1000".into()).parse()?;
    let requested_series = series_selection(&args.next().unwrap_or_else(|| "all".into()))?;
    let cache_roots = args
        .next()
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    if !(1..=20).contains(&years) {
        return Err(format!("YEARS must be in 1..=20, got {years}").into());
    }
    if args.next().is_some() {
        return Err("too many arguments".into());
    }

    let universe: Vec<InputInstrument> = serde_json::from_slice(&std::fs::read(&universe_path)?)?;
    let selected = select_sample(universe, per_bucket);
    validate_pacing(selected.len(), requested_series.len(), pause_ms)?;
    std::fs::create_dir_all(output_root.join("series"))?;

    let client_id = std::env::var("IB_STOCK_AUDIT_CLIENT_ID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(19);
    let source = StockDataSource::connect(GatewayConfig::from_env(false, client_id)?).await?;

    let mut instruments = Vec::with_capacity(selected.len());
    for instrument in selected {
        eprintln!(
            "auditing {} {} ({:?})",
            instrument.isin, instrument.symbol, instrument.bucket
        );
        match source
            .resolve(&StockQuery::by_isin(&instrument.isin, PRIMARY_EXCHANGE))
            .await
        {
            Ok(stock) => {
                let mut series = Vec::with_capacity(requested_series.len());
                for kind in requested_series.iter().copied() {
                    series.push(
                        archive_series(
                            &source,
                            &stock,
                            kind,
                            years,
                            &output_root,
                            &cache_roots,
                            pause_ms,
                        )
                        .await,
                    );
                }
                instruments.push(InstrumentAudit {
                    instrument,
                    ib_stock: Some(StockIdentity::from(&stock)),
                    resolution_error: None,
                    series,
                });
            }
            Err(error) => instruments.push(InstrumentAudit {
                instrument,
                ib_stock: None,
                resolution_error: Some(error.to_string()),
                series: Vec::new(),
            }),
        }
    }

    let totals = totals(&instruments);
    let report = AuditReport {
        format_version: "ib-stockholm-main-history-audit-1".into(),
        generated_at: OffsetDateTime::now_utc().to_string(),
        gateway_mode: if source.is_paper() { "paper" } else { "live" }.into(),
        primary_exchange: PRIMARY_EXCHANGE.into(),
        requested_years: years,
        per_bucket_limit: per_bucket,
        pause_ms,
        requested_series,
        cache_roots: cache_roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        universe_policy: "NASDAQ_STOCKHOLM_MAIN_LARGE_MID_SMALL".into(),
        note: "FEE_RATE is historical borrow cost, not historical share availability; current availability still requires a live snapshot".into(),
        totals,
        instruments,
    };
    write_json(&output_root.join("audit.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report.totals)?);
    Ok(())
}

fn series_selection(value: &str) -> Result<Vec<DailySeries>, String> {
    match value {
        "all" => Ok(vec![
            DailySeries::Trades,
            DailySeries::AdjustedLast,
            DailySeries::FeeRate,
        ]),
        "prices" => Ok(vec![DailySeries::Trades, DailySeries::AdjustedLast]),
        "fee-rate" => Ok(vec![DailySeries::FeeRate]),
        _ => Err(format!(
            "SERIES must be all, prices, or fee-rate, got {value:?}"
        )),
    }
}

fn validate_pacing(instruments: usize, series: usize, pause_ms: u64) -> Result<(), String> {
    let requests = instruments.saturating_mul(series);
    if requests > MAX_UNPACED_HISTORICAL_REQUESTS && pause_ms < FULL_AUDIT_MIN_PAUSE_MS {
        return Err(format!(
            "{requests} historical requests require PAUSE_MS >= \
             {FULL_AUDIT_MIN_PAUSE_MS}; use a small stratified sample for a faster entitlement probe"
        ));
    }
    Ok(())
}

fn select_sample(mut universe: Vec<InputInstrument>, per_bucket: usize) -> Vec<InputInstrument> {
    universe.retain(|instrument| instrument.bucket.is_main_market());
    universe.sort_by(|left, right| {
        left.bucket
            .cmp(&right.bucket)
            .then_with(|| left.symbol.cmp(&right.symbol))
            .then_with(|| left.isin.cmp(&right.isin))
    });
    let mut grouped = std::collections::BTreeMap::<_, Vec<_>>::new();
    for instrument in universe {
        grouped
            .entry(instrument.bucket)
            .or_default()
            .push(instrument);
    }
    let mut selected = Vec::new();
    for bucket in [
        UniverseBucket::LargeCap,
        UniverseBucket::MidCap,
        UniverseBucket::SmallCap,
    ] {
        let Some(group) = grouped.remove(&bucket) else {
            continue;
        };
        let count = if per_bucket == 0 {
            group.len()
        } else {
            per_bucket.min(group.len())
        };
        if count == group.len() {
            selected.extend(group);
        } else if count == 1 {
            selected.push(group[0].clone());
        } else {
            for sample_index in 0..count {
                let index = sample_index * (group.len() - 1) / (count - 1);
                selected.push(group[index].clone());
            }
        }
    }
    selected
}

async fn archive_series(
    source: &StockDataSource,
    stock: &ResolvedStock,
    series: DailySeries,
    years: i32,
    output_root: &Path,
    cache_roots: &[PathBuf],
    pause_ms: u64,
) -> SeriesAudit {
    let relative = archive_path(stock.conid, series);
    let absolute = output_root.join(&relative);
    if absolute.exists() {
        match read_json::<StockHistoryRecord>(&absolute) {
            Ok(record) if reusable(&record, stock, series, years) => {
                return completed_series(series, relative, record, true, Some(absolute));
            }
            Ok(_) => eprintln!(
                "ignoring incompatible archive {} and requesting it again",
                absolute.display()
            ),
            Err(error) => eprintln!(
                "ignoring unreadable archive {} ({error}) and requesting it again",
                absolute.display()
            ),
        }
    }
    for cache_root in cache_roots {
        let cached = cache_root.join(&relative);
        if !cached.exists() {
            continue;
        }
        match read_json::<StockHistoryRecord>(&cached) {
            Ok(record) if reusable(&record, stock, series, years) => {
                return match persist_series(output_root, &record) {
                    Ok(path) => {
                        completed_series(series, PathBuf::from(path), record, true, Some(cached))
                    }
                    Err(error) => failed_series(series, error),
                };
            }
            Ok(_) => eprintln!("ignoring incompatible cache archive {}", cached.display()),
            Err(error) => eprintln!(
                "ignoring unreadable cache archive {} ({error})",
                cached.display()
            ),
        }
    }
    let result = source.history_record(stock, series, years).await;
    if pause_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
    }
    match result {
        Ok(record) => match persist_series(output_root, &record) {
            Ok(path) => completed_series(series, PathBuf::from(path), record, false, None),
            Err(error) => failed_series(series, error),
        },
        Err(error) => failed_series(series, error.to_string()),
    }
}

fn reusable(
    record: &StockHistoryRecord,
    stock: &ResolvedStock,
    series: DailySeries,
    years: i32,
) -> bool {
    record.format_version == "ib-stock-daily-history-1"
        && record.stock.conid == stock.conid
        && record.stock.isin == stock.isin
        && record.series == series
        && record.requested_years >= years
}

fn completed_series(
    series: DailySeries,
    relative: PathBuf,
    record: StockHistoryRecord,
    reused_archive: bool,
    reused_from: Option<PathBuf>,
) -> SeriesAudit {
    SeriesAudit {
        series,
        archive: Some(relative.to_string_lossy().into_owned()),
        reused_archive,
        reused_from: reused_from.map(|path| path.to_string_lossy().into_owned()),
        observations: record.coverage.observations,
        first_date: record.coverage.first_date,
        last_date: record.coverage.last_date,
        error: None,
    }
}

fn persist_series(output_root: &Path, record: &StockHistoryRecord) -> Result<String, String> {
    let relative = archive_path(record.stock.conid, record.series);
    write_json(&output_root.join(&relative), record)?;
    Ok(relative.to_string_lossy().into_owned())
}

fn archive_path(conid: i32, series: DailySeries) -> PathBuf {
    let name = format!(
        "{conid}-{}.json",
        match series {
            DailySeries::Trades => "trades",
            DailySeries::AdjustedLast => "adjusted-last",
            DailySeries::FeeRate => "fee-rate",
        }
    );
    PathBuf::from("series").join(name)
}

fn failed_series(series: DailySeries, error: impl Into<String>) -> SeriesAudit {
    SeriesAudit {
        series,
        archive: None,
        reused_archive: false,
        reused_from: None,
        observations: 0,
        first_date: None,
        last_date: None,
        error: Some(error.into()),
    }
}

fn totals(instruments: &[InstrumentAudit]) -> AuditTotals {
    let with_data = |series| {
        instruments
            .iter()
            .filter(|instrument| {
                instrument
                    .series
                    .iter()
                    .any(|result| result.series == series && result.observations > 0)
            })
            .count()
    };
    let instruments_resolved = instruments
        .iter()
        .filter(|instrument| instrument.ib_stock.is_some())
        .count();
    AuditTotals {
        instruments_requested: instruments.len(),
        instruments_resolved,
        resolution_failures: instruments.len() - instruments_resolved,
        trades_with_data: with_data(DailySeries::Trades),
        adjusted_last_with_data: with_data(DailySeries::AdjustedLast),
        fee_rate_with_data: with_data(DailySeries::FeeRate),
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("{} -> {}: {error}", temporary.display(), path.display()))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument(symbol: &str, bucket: UniverseBucket) -> InputInstrument {
        InputInstrument {
            orderbook_id: symbol.into(),
            isin: format!("SE-{symbol}"),
            symbol: symbol.into(),
            name: symbol.into(),
            currency: "SEK".into(),
            sector: "Test".into(),
            bucket,
            yahoo_symbol: format!("{symbol}.ST"),
        }
    }

    #[test]
    fn sample_is_balanced_and_excludes_first_north() {
        let sample = select_sample(
            vec![
                instrument("L2", UniverseBucket::LargeCap),
                instrument("F1", UniverseBucket::FirstNorth),
                instrument("S1", UniverseBucket::SmallCap),
                instrument("L1", UniverseBucket::LargeCap),
                instrument("M1", UniverseBucket::MidCap),
            ],
            1,
        );
        assert_eq!(sample.len(), 3);
        assert_eq!(sample[0].symbol, "L1");
        assert_eq!(sample[1].symbol, "M1");
        assert_eq!(sample[2].symbol, "S1");
    }

    #[test]
    fn sample_spans_each_bucket_instead_of_taking_a_prefix() {
        let sample = select_sample(
            (0..10)
                .map(|index| instrument(&format!("L{index}"), UniverseBucket::LargeCap))
                .collect(),
            3,
        );
        assert_eq!(
            sample
                .iter()
                .map(|instrument| instrument.symbol.as_str())
                .collect::<Vec<_>>(),
            ["L0", "L4", "L9"]
        );
    }

    #[test]
    fn full_audit_requires_conservative_pacing() {
        assert!(validate_pacing(51, 1, 1_000).is_err());
        assert!(validate_pacing(51, 1, FULL_AUDIT_MIN_PAUSE_MS).is_ok());
        assert!(validate_pacing(3, 3, 0).is_ok());
    }

    #[test]
    fn series_mode_limits_requests() {
        assert_eq!(
            series_selection("fee-rate").unwrap(),
            [DailySeries::FeeRate]
        );
        assert_eq!(series_selection("prices").unwrap().len(), 2);
        assert!(series_selection("other").is_err());
    }
}
