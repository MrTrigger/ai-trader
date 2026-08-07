//! Reproducible research evidence built from one declared window.

use std::path::Path;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::backtest::{self, Metrics};
use crate::config::Config;
use crate::{ic, store, universe, validate};

#[derive(Debug, serde::Serialize)]
pub struct DataBlock {
    pub assets: usize,
    pub bars: usize,
    pub first_bar: Option<DateTime<Utc>>,
    pub last_bar: Option<DateTime<Utc>>,
    pub stale_assets: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct UniversePoint {
    pub date: chrono::NaiveDate,
    pub eligible: usize,
    pub considered: usize,
    pub dead_or_halted: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct NavPoint {
    pub date: chrono::NaiveDate,
    #[serde(with = "rust_decimal::serde::str")]
    pub value: Decimal,
}

#[derive(Debug, serde::Serialize)]
pub struct FoldPoint {
    pub start: Option<chrono::NaiveDate>,
    pub end: Option<chrono::NaiveDate>,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_return: Decimal,
    pub sharpe: f64,
    pub n: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct RunBlock {
    pub label: String,
    pub signal: String,
    pub constructor: String,
    pub metrics: Metrics,
    pub stressed: Metrics,
    pub nav: Vec<NavPoint>,
    pub folds: Vec<FoldPoint>,
    pub disclosures: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct Record {
    pub schema_version: &'static str,
    pub generated_at: DateTime<Utc>,
    pub window: [chrono::NaiveDate; 2],
    pub disclosures: Vec<String>,
    pub data: DataBlock,
    pub universe: Vec<UniversePoint>,
    pub runs: Vec<RunBlock>,
    pub ic: Vec<ic::IcResult>,
}

fn data_block(bars: &[features_crypto::Bar]) -> DataBlock {
    let mut last_by_asset = std::collections::BTreeMap::new();
    for bar in bars {
        last_by_asset
            .entry(bar.asset.as_str())
            .and_modify(|old: &mut DateTime<Utc>| *old = (*old).max(bar.ts_utc))
            .or_insert(bar.ts_utc);
    }
    let first_bar = bars.iter().map(|bar| bar.ts_utc).min();
    let last_bar = bars.iter().map(|bar| bar.ts_utc).max();
    let stale_assets = last_bar.map_or(0, |newest| {
        last_by_asset
            .values()
            .filter(|last| **last < newest - chrono::Duration::days(30))
            .count()
    });
    DataBlock {
        assets: last_by_asset.len(),
        bars: bars.len(),
        first_bar,
        last_bar,
        stale_assets,
    }
}

fn universe_block(root: &Path, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<UniversePoint> {
    let mut out = Vec::new();
    let mut as_of = start;
    while as_of <= end {
        if let Ok(members) = universe::load(root, as_of) {
            out.push(UniversePoint {
                date: as_of.date_naive(),
                eligible: members.iter().filter(|member| member.eligible).count(),
                considered: members.len(),
                dead_or_halted: members
                    .iter()
                    .filter(|member| {
                        member.reason.contains("delisted") || member.reason.contains("halted")
                    })
                    .count(),
            });
        }
        as_of += chrono::Duration::days(7);
    }
    out
}

pub fn build(
    config: &Config,
    root: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    initial_cash: Decimal,
) -> Result<Record, String> {
    if start > end {
        return Err("research start is after end".into());
    }
    let bars = store::read(root, config.interval_s as i32)?;
    if bars.is_empty() {
        return Err("daily store is empty".into());
    }
    let prepared = backtest::prepare(config, root, false)?;
    let specifications = [
        ("momentum", "xs_momentum", "conviction_tilt"),
        ("gc_breakout", "gc_breakout", "conviction_tilt"),
        ("baseline", "liquidity_top", "equal_weight"),
    ];
    let mut runs = Vec::new();
    for (label, signal, constructor) in specifications {
        let mut cfg = config.clone();
        cfg.signal = signal.into();
        cfg.constructor = constructor.into();
        let result = backtest::replay_prepared(
            &cfg,
            start,
            end,
            root,
            initial_cash,
            Decimal::ONE,
            &prepared,
        )?;
        let stressed = backtest::replay_prepared(
            &cfg,
            start,
            end,
            root,
            initial_cash,
            Decimal::from(2),
            &prepared,
        )?;
        let walk = validate::walk_forward(&result, 4, 0.6)?;
        runs.push(RunBlock {
            label: label.into(),
            signal: signal.into(),
            constructor: constructor.into(),
            metrics: result.metrics,
            stressed: stressed.metrics,
            nav: result
                .steps
                .iter()
                .map(|step| NavPoint {
                    date: step.as_of.date_naive(),
                    value: step.nav,
                })
                .collect(),
            folds: walk
                .folds
                .into_iter()
                .map(|fold| FoldPoint {
                    start: fold.test.dates.first().copied(),
                    end: fold.test.dates.last().copied(),
                    total_return: fold.test.metrics.total_return,
                    sharpe: fold.test.metrics.sharpe,
                    n: fold.test.metrics.n,
                })
                .collect(),
            disclosures: result.disclosures,
        });
    }
    let ic = ic::measure(config, start, end, root, "ret_30_skip_7", &[7, 14, 30])?;
    Ok(Record {
        schema_version: "rust-research-1",
        generated_at: Utc::now(),
        window: [start.date_naive(), end.date_naive()],
        disclosures: vec![
            "all computed sections use the declared window and current Rust decision/feature code"
                .into(),
            "candidate runs are evidence about this history, not evidence of a durable edge".into(),
            "Python is not involved in feature calculation, scoring, replay, validation, or rendering"
                .into(),
        ],
        data: data_block(&bars),
        universe: universe_block(root, start, end),
        runs,
        ic,
    })
}
