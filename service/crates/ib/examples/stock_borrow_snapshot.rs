//! Immutable current borrow/quote snapshot for the Stockholm Main Market.
//!
//! IB does not expose historical lendable quantity through the TWS API. This
//! collector starts a truthful prospective series instead: every observation
//! carries its own capture timestamp and preserves missing broker fields.
//!
//! Usage (with IB_GATEWAY_* and IB_PAPER_ACCOUNT exported):
//! cargo run -p ib --example stock_borrow_snapshot -- UNIVERSE_JSON OUTPUT_ROOT [CONCURRENCY]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ib::stocks::{StockDataSource, StockIdentity, StockQuery, StockSnapshot};
use ib::GatewayConfig;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const PRIMARY_EXCHANGE: &str = "SFB";

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
struct Observation {
    instrument: InputInstrument,
    ib_stock: Option<StockIdentity>,
    snapshot: Option<StockSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotArchive {
    format_version: String,
    generated_at: String,
    gateway_mode: String,
    universe_policy: String,
    primary_exchange: String,
    requested: usize,
    resolved: usize,
    snapshots: usize,
    with_shortable_tier: usize,
    with_available_shares: usize,
    limitations: Vec<String>,
    observations: Vec<Observation>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let universe_path = PathBuf::from(
        args.next()
            .ok_or("usage: stock_borrow_snapshot UNIVERSE_JSON OUTPUT_ROOT [CONCURRENCY]")?,
    );
    let output_root = PathBuf::from(
        args.next()
            .ok_or("usage: stock_borrow_snapshot UNIVERSE_JSON OUTPUT_ROOT [CONCURRENCY]")?,
    );
    let concurrency: usize = args.next().unwrap_or_else(|| "8".into()).parse()?;
    if !(1..=32).contains(&concurrency) {
        return Err("CONCURRENCY must be in 1..=32".into());
    }
    if args.next().is_some() {
        return Err("too many arguments".into());
    }

    let mut universe: Vec<InputInstrument> =
        serde_json::from_slice(&std::fs::read(universe_path)?)?;
    universe.retain(|instrument| instrument.bucket.is_main_market());
    universe.sort_by(|left, right| {
        left.bucket
            .cmp(&right.bucket)
            .then_with(|| left.symbol.cmp(&right.symbol))
            .then_with(|| left.isin.cmp(&right.isin))
    });
    let source = Arc::new(StockDataSource::connect(GatewayConfig::from_env(false, 20)?).await?);
    let paper = source.is_paper();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks: JoinSet<Result<Observation, String>> = JoinSet::new();
    for instrument in universe {
        let source = Arc::clone(&source);
        let semaphore = Arc::clone(&semaphore);
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|error| error.to_string())?;
            eprintln!("snapshotting {} {}", instrument.isin, instrument.symbol);
            let stock = source
                .resolve(&StockQuery::by_isin(&instrument.isin, PRIMARY_EXCHANGE))
                .await;
            match stock {
                Ok(stock) => match source.snapshot(&stock).await {
                    Ok(snapshot) => Ok(Observation {
                        instrument,
                        ib_stock: Some(StockIdentity::from(&stock)),
                        snapshot: Some(snapshot),
                        error: None,
                    }),
                    Err(error) => Ok(Observation {
                        instrument,
                        ib_stock: Some(StockIdentity::from(&stock)),
                        snapshot: None,
                        error: Some(error.to_string()),
                    }),
                },
                Err(error) => Ok(Observation {
                    instrument,
                    ib_stock: None,
                    snapshot: None,
                    error: Some(error.to_string()),
                }),
            }
        });
    }
    let mut observations = Vec::new();
    while let Some(result) = tasks.join_next().await {
        observations.push(result.map_err(|error| error.to_string())??);
    }
    observations.sort_by(|left, right| {
        left.instrument
            .bucket
            .cmp(&right.instrument.bucket)
            .then_with(|| left.instrument.symbol.cmp(&right.instrument.symbol))
            .then_with(|| left.instrument.isin.cmp(&right.instrument.isin))
    });
    let now = OffsetDateTime::now_utc();
    let archive = SnapshotArchive {
        format_version: "ib-stockholm-main-borrow-snapshot-1".into(),
        generated_at: now.to_string(),
        gateway_mode: if paper { "paper" } else { "live" }.into(),
        universe_policy: "NASDAQ_STOCKHOLM_MAIN_LARGE_MID_SMALL".into(),
        primary_exchange: PRIMARY_EXCHANGE.into(),
        requested: observations.len(),
        resolved: observations
            .iter()
            .filter(|observation| observation.ib_stock.is_some())
            .count(),
        snapshots: observations
            .iter()
            .filter(|observation| observation.snapshot.is_some())
            .count(),
        with_shortable_tier: observations
            .iter()
            .filter(|observation| {
                observation
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.shortable.is_some())
            })
            .count(),
        with_available_shares: observations
            .iter()
            .filter(|observation| {
                observation
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.available_shares.is_some())
            })
            .count(),
        limitations: vec![
            "Each record is a current account-visible IB snapshot, not historical availability".into(),
            "A positive quantity is not a future locate guarantee; live pre-trade validation remains authoritative".into(),
            "The universe is the supplied current snapshot and therefore cannot reconstruct earlier membership".into(),
        ],
        observations,
    };
    let snapshot_dir = output_root.join("snapshots");
    std::fs::create_dir_all(&snapshot_dir)?;
    let snapshot_path = snapshot_dir.join(format!("{}.json", now.unix_timestamp()));
    write_json(&snapshot_path, &archive)?;
    write_json(&output_root.join("latest.json"), &archive)?;
    println!(
        "wrote {}: {}/{} snapshots, {} quantities",
        snapshot_path.display(),
        archive.snapshots,
        archive.requested,
        archive.with_available_shares
    );
    Ok(())
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
