//! Empirical block resampling of saved daily walk-forward returns.
//!
//! Each block begins on a uniformly sampled observed day, continues in order,
//! and wraps within that day's fold. Blocks never splice different folds.
//! Wrapping creates one artificial end/start adjacency; block joins break
//! dependence. Results condition on the observed regimes and charged costs.
//! Paths are scenarios, not additional evidence that a strategy has alpha.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeDelta, Timelike, Utc};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(untagged)]
enum Number {
    Text(String),
    Float(f64),
}

impl Number {
    fn positive(&self, label: &str) -> Result<f64, String> {
        let value = match self {
            Self::Text(value) => value.parse::<f64>().map_err(|e| format!("{label}: {e}"))?,
            Self::Float(value) => *value,
        };
        if value.is_finite() && value > 0.0 {
            Ok(value)
        } else {
            Err(format!("{label} must be finite and positive"))
        }
    }
}

#[derive(Deserialize)]
struct SavedStep {
    as_of: DateTime<Utc>,
    nav: Number,
}

#[derive(Deserialize)]
struct SavedFold {
    initial_cash: Option<Number>,
    steps: Vec<SavedStep>,
}

#[derive(Serialize)]
struct FoldSource {
    file: String,
    sha256: String,
    initial_cash: f64,
    initial_cash_source: &'static str,
    observations: usize,
    sharpe: Option<f64>,
}

struct Series {
    dates: Vec<DateTime<Utc>>,
    returns: Vec<f64>,
    bounds: Vec<(usize, usize)>,
    sources: Vec<FoldSource>,
}

#[derive(Debug)]
struct FoldReturns {
    dates: Vec<DateTime<Utc>>,
    returns: Vec<f64>,
    initial_cash: f64,
    initial_cash_source: &'static str,
}

fn fold_returns(fold: SavedFold, fallback: Option<f64>) -> Result<FoldReturns, String> {
    let (initial, source) = match fold.initial_cash {
        Some(value) => (value.positive("initial_cash")?, "metadata"),
        None => (
            fallback.ok_or(
                "missing initial_cash metadata; supply --initial-cash explicitly for legacy folds",
            )?,
            "cli",
        ),
    };
    if !initial.is_finite() || initial <= 0.0 {
        return Err("initial cash must be finite and positive".into());
    }
    if fold.steps.len() < 2 {
        return Err("each fold needs at least two daily steps".into());
    }
    let mut dates = Vec::with_capacity(fold.steps.len());
    let mut returns = Vec::with_capacity(fold.steps.len());
    let mut previous = initial;
    for step in fold.steps {
        if step.as_of.time().num_seconds_from_midnight() != 0
            || step.as_of.timestamp_subsec_nanos() != 0
        {
            return Err(format!(
                "{} is not UTC midnight; only daily replays are supported",
                step.as_of
            ));
        }
        if let Some(date) = dates.last() {
            if step.as_of - *date != TimeDelta::days(1) {
                return Err(format!(
                    "non-daily, missing, duplicate or out-of-order step at {}",
                    step.as_of
                ));
            }
        }
        let nav = step.nav.positive("NAV")?;
        let change = nav / previous - 1.0;
        if !change.is_finite() || change <= -1.0 {
            return Err(format!("unrepresentable return at {}", step.as_of));
        }
        dates.push(step.as_of);
        returns.push(change);
        previous = nav;
    }
    Ok(FoldReturns {
        dates,
        returns,
        initial_cash: initial,
        initial_cash_source: source,
    })
}

fn load(
    dir: &Path,
    fallback: Option<f64>,
    annual_periods: f64,
    last_folds: Option<usize>,
) -> Result<Series, String> {
    use sha2::{Digest, Sha256};
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|name| name.starts_with("fold-") && name.ends_with(".json"))
        {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(format!("{}: no fold-*.json files", dir.display()));
    }
    let mut folds = Vec::new();
    for path in files {
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fold: SavedFold =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let FoldReturns {
            dates,
            returns,
            initial_cash,
            initial_cash_source,
        } = fold_returns(fold, fallback).map_err(|e| format!("{}: {e}", path.display()))?;
        let source = FoldSource {
            file: path.display().to_string(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            initial_cash,
            initial_cash_source,
            observations: dates.len(),
            sharpe: sharpe(&returns, annual_periods),
        };
        folds.push((dates, returns, source));
    }
    folds.sort_by_key(|(dates, _, _)| dates[0]);
    if let Some(count) = last_folds {
        if count == 0 || count > folds.len() {
            return Err(format!(
                "--last-folds must be between 1 and {}",
                folds.len()
            ));
        }
        folds.drain(..folds.len() - count);
    }
    let mut series = Series {
        dates: vec![],
        returns: vec![],
        bounds: vec![],
        sources: vec![],
    };
    for (dates, returns, source) in folds {
        if let Some(previous) = series.dates.last() {
            if dates[0] - *previous != TimeDelta::days(1) {
                return Err(format!(
                    "folds overlap or have a gap between {previous} and {}",
                    dates[0]
                ));
            }
        }
        let start = series.returns.len();
        series.dates.extend(dates);
        series.returns.extend(returns);
        series.bounds.push((start, series.returns.len()));
        series.sources.push(source);
    }
    Ok(series)
}

pub fn daily_returns(
    dir: &Path,
    initial_cash: Option<f64>,
) -> Result<Vec<(DateTime<Utc>, f64)>, String> {
    let series = load(dir, initial_cash, 365.0, None)?;
    Ok(series.dates.into_iter().zip(series.returns).collect())
}

#[derive(Serialize)]
pub struct ObservedMetrics {
    pub sharpe: Option<f64>,
    pub total_return: f64,
    pub max_drawdown: f64,
}

pub fn observed_metrics(dir: &Path) -> Result<ObservedMetrics, String> {
    let series = load(dir, None, 365.0, None)?;
    let (total_return, max_drawdown) = path_metrics(series.returns.iter().copied())?;
    Ok(ObservedMetrics {
        sharpe: if series.returns.len() < 2 {
            None
        } else {
            sharpe(&series.returns, 365.0)
        },
        total_return,
        max_drawdown,
    })
}

fn crop(
    mut series: Series,
    window: Option<(DateTime<Utc>, DateTime<Utc>)>,
    annual_periods: f64,
) -> Result<Series, String> {
    let Some((start, end)) = window else {
        return Ok(series);
    };
    if start > end || start < series.dates[0] || end > *series.dates.last().unwrap() {
        return Err("requested Monte Carlo window is outside the complete source dates".into());
    }
    let first = series.dates.partition_point(|date| *date < start);
    let last = series.dates.partition_point(|date| *date <= end);
    let mut bounds = Vec::new();
    let mut sources = Vec::new();
    for ((lo, hi), mut source) in series.bounds.into_iter().zip(series.sources) {
        let lo = lo.max(first);
        let hi = hi.min(last);
        if lo < hi {
            source.observations = hi - lo;
            source.sharpe = sharpe(&series.returns[lo..hi], annual_periods);
            bounds.push((lo - first, hi - first));
            sources.push(source);
        }
    }
    series.dates = series.dates[first..last].to_vec();
    series.returns = series.returns[first..last].to_vec();
    series.bounds = bounds;
    series.sources = sources;
    Ok(series)
}

#[derive(Serialize)]
struct Quantiles {
    p025: f64,
    p05: f64,
    p50: f64,
    p95: f64,
    p975: f64,
}

fn quantiles(values: &mut [f64]) -> Quantiles {
    values.sort_by(f64::total_cmp);
    let at = |p: f64| {
        let index = p * (values.len() - 1) as f64;
        let lo = index.floor() as usize;
        let hi = index.ceil() as usize;
        values[lo] + (values[hi] - values[lo]) * index.fract()
    };
    Quantiles {
        p025: at(0.025),
        p05: at(0.05),
        p50: at(0.5),
        p95: at(0.95),
        p975: at(0.975),
    }
}

fn sharpe(returns: &[f64], annual_periods: f64) -> Option<f64> {
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (returns.len() - 1) as f64;
    let value = mean / variance.sqrt() * annual_periods.sqrt();
    (variance > 0.0 && value.is_finite()).then_some(value)
}

fn path_metrics(returns: impl IntoIterator<Item = f64>) -> Result<(f64, f64), String> {
    let mut wealth: f64 = 1.0;
    let mut peak: f64 = 1.0;
    let mut drawdown: f64 = 0.0;
    for change in returns {
        wealth *= 1.0 + change;
        if !wealth.is_finite() || wealth <= 0.0 {
            return Err(
                "simulated wealth overflowed or underflowed; shorten horizon or inspect returns"
                    .into(),
            );
        }
        peak = peak.max(wealth);
        drawdown = drawdown.max(1.0 - wealth / peak);
    }
    Ok((wealth - 1.0, drawdown))
}

// SplitMix64 has a fully specified stream, including seed zero.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
    fn index(&mut self, n: usize) -> usize {
        let bound = n as u64;
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let value = self.next();
            if value >= threshold {
                return (value % bound) as usize;
            }
        }
    }
}

fn sample_indices(series: &Series, block: usize, length: usize, rng: &mut Rng) -> Vec<usize> {
    let mut indices = Vec::with_capacity(length);
    while indices.len() < length {
        let first = rng.index(series.returns.len());
        let &(start, end) = series
            .bounds
            .iter()
            .find(|&&(start, end)| first >= start && first < end)
            .unwrap();
        for offset in 0..block.min(length - indices.len()) {
            indices.push(start + (first - start + offset) % (end - start));
        }
    }
    indices
}

#[derive(Serialize)]
struct PathDistribution {
    cumulative_return: Quantiles,
    max_drawdown: Quantiles,
    probability_loss: f64,
    probability_drawdown_over_25_percent: f64,
}

#[derive(Default)]
struct Paths {
    returns: Vec<f64>,
    drawdowns: Vec<f64>,
}
impl Paths {
    fn push(&mut self, returns: impl IntoIterator<Item = f64>) -> Result<(), String> {
        let (change, drawdown) = path_metrics(returns)?;
        self.returns.push(change);
        self.drawdowns.push(drawdown);
        Ok(())
    }
    fn finish(mut self) -> PathDistribution {
        let n = self.returns.len() as f64;
        PathDistribution {
            probability_loss: self.returns.iter().filter(|&&v| v < 0.0).count() as f64 / n,
            probability_drawdown_over_25_percent: self
                .drawdowns
                .iter()
                .filter(|&&v| v > 0.25)
                .count() as f64
                / n,
            cumulative_return: quantiles(&mut self.returns),
            max_drawdown: quantiles(&mut self.drawdowns),
        }
    }
}

#[derive(Serialize)]
struct SharpeDistribution {
    defined_iterations: usize,
    undefined_iterations: usize,
    percentile_interval: Option<Quantiles>,
}
fn sharpe_distribution(mut values: Vec<f64>, iterations: usize) -> SharpeDistribution {
    SharpeDistribution {
        defined_iterations: values.len(),
        undefined_iterations: iterations - values.len(),
        percentile_interval: (!values.is_empty()).then(|| quantiles(&mut values)),
    }
}

#[derive(Serialize)]
struct PairedReport {
    sources: Vec<FoldSource>,
    original_daily_return_correlation: Option<f64>,
    original_benchmark_cumulative_return: f64,
    original_benchmark_max_drawdown: f64,
    original_daily_rebalanced_50_50_blend_cumulative_return: f64,
    original_daily_rebalanced_50_50_blend_max_drawdown: f64,
    original_benchmark_pooled_sharpe: Option<f64>,
    original_strategy_minus_benchmark_sharpe: Option<f64>,
    strategy_minus_benchmark_sharpe_bootstrap: SharpeDistribution,
    benchmark_horizon: PathDistribution,
    original_daily_rebalanced_50_50_blend_sharpe: Option<f64>,
    daily_rebalanced_50_50_blend_sharpe_bootstrap: SharpeDistribution,
    daily_rebalanced_50_50_blend_horizon: PathDistribution,
}

fn correlation(left: &[f64], right: &[f64]) -> Option<f64> {
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let a = mean(left);
    let b = mean(right);
    let mut cross = 0.0;
    let mut aa = 0.0;
    let mut bb = 0.0;
    for (x, y) in left.iter().zip(right) {
        cross += (x - a) * (y - b);
        aa += (x - a).powi(2);
        bb += (y - b).powi(2);
    }
    (aa > 0.0 && bb > 0.0).then(|| (cross / (aa * bb).sqrt()).clamp(-1.0, 1.0))
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    method: &'static str,
    seed: u64,
    iterations: usize,
    block_days: usize,
    horizon_days: usize,
    annual_periods: f64,
    observations: usize,
    first_date: DateTime<Utc>,
    last_date: DateTime<Utc>,
    original_pooled_sharpe: Option<f64>,
    original_mean_fold_sharpe: Option<f64>,
    original_cumulative_return: f64,
    original_max_drawdown: f64,
    sources: Vec<FoldSource>,
    whole_sample_sharpe_bootstrap: SharpeDistribution,
    horizon_scenarios: PathDistribution,
    paired: Option<PairedReport>,
    disclosures: Vec<&'static str>,
}

struct Options {
    folds: PathBuf,
    out: PathBuf,
    benchmark: Option<PathBuf>,
    iterations: usize,
    seed: u64,
    block: usize,
    horizon: usize,
    annual_periods: f64,
    initial_cash: Option<f64>,
    last_folds: Option<usize>,
    window: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let allowed = [
            "--folds",
            "--out",
            "--benchmark",
            "--iterations",
            "--seed",
            "--block",
            "--horizon-days",
            "--annual-periods",
            "--initial-cash",
            "--last-folds",
            "--start",
            "--end",
        ];
        let mut values = BTreeMap::new();
        for pair in args.chunks(2) {
            if pair.len() != 2 || !allowed.contains(&pair[0].as_str()) || pair[1].starts_with("--")
            {
                return Err(format!("unknown option or missing value: {}", pair[0]));
            }
            if values.insert(pair[0].as_str(), pair[1].as_str()).is_some() {
                return Err(format!("duplicate option {}", pair[0]));
            }
        }
        let required = |name| {
            values
                .get(name)
                .map(PathBuf::from)
                .ok_or_else(|| format!("missing {name}"))
        };
        let integer = |name, default| {
            values
                .get(name)
                .unwrap_or(&default)
                .parse::<usize>()
                .map_err(|e| format!("{name}: {e}"))
        };
        let number = |name, default| {
            values
                .get(name)
                .unwrap_or(&default)
                .parse::<f64>()
                .map_err(|e| format!("{name}: {e}"))
        };
        let last_folds = values
            .get("--last-folds")
            .map(|value| {
                value
                    .parse::<usize>()
                    .map_err(|e| format!("--last-folds: {e}"))
            })
            .transpose()?;
        if last_folds == Some(0) {
            return Err("--last-folds must be positive".into());
        }
        let options = Self {
            window: match (values.get("--start"), values.get("--end")) {
                (None, None) => None,
                (Some(start), Some(end)) => {
                    let parse = |value: &str| {
                        format!("{value}T00:00:00Z")
                            .parse::<DateTime<Utc>>()
                            .map_err(|e| format!("bad date {value}: {e}"))
                    };
                    Some((parse(start)?, parse(end)?))
                }
                _ => return Err("--start and --end must be supplied together".into()),
            },
            last_folds,
            folds: required("--folds")?,
            out: required("--out")?,
            benchmark: values.get("--benchmark").map(PathBuf::from),
            iterations: integer("--iterations", "10000")?,
            seed: values
                .get("--seed")
                .unwrap_or(&"42")
                .parse()
                .map_err(|e| format!("--seed: {e}"))?,
            block: integer("--block", "20")?,
            horizon: integer("--horizon-days", "365")?,
            annual_periods: number("--annual-periods", "365")?,
            initial_cash: values
                .get("--initial-cash")
                .map(|value| {
                    value
                        .parse::<f64>()
                        .map_err(|e| format!("--initial-cash: {e}"))
                })
                .transpose()?,
        };
        if !(100..=1_000_000).contains(&options.iterations) {
            return Err("--iterations must be between 100 and 1000000".into());
        }
        if options.block < 2 {
            return Err("--block must be at least 2 days to retain serial dependence".into());
        }
        if !(1..=36_500).contains(&options.horizon) {
            return Err("--horizon-days must be between 1 and 36500".into());
        }
        if !options.annual_periods.is_finite() || options.annual_periods <= 0.0 {
            return Err("--annual-periods must be finite and positive".into());
        }
        if options
            .initial_cash
            .is_some_and(|v| !v.is_finite() || v <= 0.0)
        {
            return Err("--initial-cash must be finite and positive".into());
        }
        Ok(options)
    }
}

fn run(options: &Options) -> Result<Report, String> {
    let series = crop(
        load(
            &options.folds,
            options.initial_cash,
            options.annual_periods,
            options.last_folds,
        )?,
        options.window,
        options.annual_periods,
    )?;
    if series.returns.len() < 60 {
        return Err("at least 60 daily observations are required".into());
    }
    if series
        .bounds
        .iter()
        .any(|&(start, end)| options.block > (end - start) / 2)
    {
        return Err("--block must fit at least twice in every fold".into());
    }
    let benchmark = options
        .benchmark
        .as_ref()
        .map(|dir| {
            crop(
                load(
                    dir,
                    options.initial_cash,
                    options.annual_periods,
                    options.last_folds,
                )?,
                options.window,
                options.annual_periods,
            )
        })
        .transpose()?;
    if benchmark
        .as_ref()
        .is_some_and(|other| other.dates != series.dates || other.bounds != series.bounds)
    {
        return Err("benchmark must have identical daily timestamps and fold boundaries; no silent inner join".into());
    }
    let mut rng = Rng(options.seed);
    let mut paths = Paths::default();
    let mut benchmark_paths = Paths::default();
    let mut blend_paths = Paths::default();
    let mut sharpes = Vec::new();
    let mut deltas = Vec::new();
    let mut blend_sharpes = Vec::new();
    for _ in 0..options.iterations {
        let indices = sample_indices(&series, options.block, options.horizon, &mut rng);
        paths.push(indices.iter().map(|&i| series.returns[i]))?;
        if let Some(other) = &benchmark {
            benchmark_paths.push(indices.iter().map(|&i| other.returns[i]))?;
            blend_paths.push(
                indices
                    .iter()
                    .map(|&i| 0.5 * series.returns[i] + 0.5 * other.returns[i]),
            )?;
        }
        let indices = sample_indices(&series, options.block, series.returns.len(), &mut rng);
        let sampled: Vec<_> = indices.iter().map(|&i| series.returns[i]).collect();
        let strategy_sharpe = sharpe(&sampled, options.annual_periods);
        if let Some(value) = strategy_sharpe {
            sharpes.push(value);
        }
        if let Some(other) = &benchmark {
            let sampled_other: Vec<_> = indices.iter().map(|&i| other.returns[i]).collect();
            if let (Some(a), Some(b)) = (
                strategy_sharpe,
                sharpe(&sampled_other, options.annual_periods),
            ) {
                deltas.push(a - b);
            }
            let blend: Vec<_> = sampled
                .iter()
                .zip(&sampled_other)
                .map(|(a, b)| 0.5 * a + 0.5 * b)
                .collect();
            if let Some(value) = sharpe(&blend, options.annual_periods) {
                blend_sharpes.push(value);
            }
        }
    }
    let original_pooled_sharpe = sharpe(&series.returns, options.annual_periods);
    let fold_sharpes: Option<Vec<f64>> = series.sources.iter().map(|fold| fold.sharpe).collect();
    let original_mean_fold_sharpe = fold_sharpes.map(|v| v.iter().sum::<f64>() / v.len() as f64);
    let (original_cumulative_return, original_max_drawdown) =
        path_metrics(series.returns.iter().copied())?;
    let paired = benchmark
        .map(|other| -> Result<PairedReport, String> {
            let original_benchmark_pooled_sharpe = sharpe(&other.returns, options.annual_periods);
            let blend: Vec<_> = series
                .returns
                .iter()
                .zip(&other.returns)
                .map(|(a, b)| 0.5 * a + 0.5 * b)
                .collect();
            let benchmark_path = path_metrics(other.returns.iter().copied())?;
            let blend_path = path_metrics(blend.iter().copied())?;
            Ok(PairedReport {
                original_daily_return_correlation: correlation(&series.returns, &other.returns),
                original_benchmark_cumulative_return: benchmark_path.0,
                original_benchmark_max_drawdown: benchmark_path.1,
                original_daily_rebalanced_50_50_blend_cumulative_return: blend_path.0,
                original_daily_rebalanced_50_50_blend_max_drawdown: blend_path.1,
                sources: other.sources,
                original_benchmark_pooled_sharpe,
                original_strategy_minus_benchmark_sharpe: original_pooled_sharpe
                    .zip(original_benchmark_pooled_sharpe)
                    .map(|(a, b)| a - b),
                strategy_minus_benchmark_sharpe_bootstrap: sharpe_distribution(
                    deltas,
                    options.iterations,
                ),
                benchmark_horizon: benchmark_paths.finish(),
                original_daily_rebalanced_50_50_blend_sharpe: sharpe(
                    &blend,
                    options.annual_periods,
                ),
                daily_rebalanced_50_50_blend_sharpe_bootstrap: sharpe_distribution(
                    blend_sharpes,
                    options.iterations,
                ),
                daily_rebalanced_50_50_blend_horizon: blend_paths.finish(),
            })
        })
        .transpose()?;
    Ok(Report {
        schema_version: 1, method: "within-fold circular fixed-length block bootstrap; SplitMix64",
        seed: options.seed, iterations: options.iterations, block_days: options.block,
        horizon_days: options.horizon, annual_periods: options.annual_periods,
        observations: series.returns.len(), first_date: series.dates[0], last_date: *series.dates.last().unwrap(),
        original_pooled_sharpe, original_mean_fold_sharpe, original_cumulative_return, original_max_drawdown,
        sources: series.sources,
        whole_sample_sharpe_bootstrap: sharpe_distribution(sharpes, options.iterations),
        horizon_scenarios: paths.finish(), paired,
        disclosures: vec![
            "Returns include each fold's first NAV / initial cash - 1, including initial fills and fees. Each fold restarts at initial cash. Liquidation and transition costs absent from saved NAVs remain absent.",
            "Source days are sampled uniformly. Consecutive days within blocks retain local dependence. Blocks wrap within a fold and never cross folds; wraparound and block joins introduce artificial adjacencies. Check multiple block lengths.",
            "Horizon quantiles describe empirical scenarios conditional on observed regimes and recorded execution costs, not calibrated forecasts. Unseen regimes, missing costs and signal decay are absent.",
            "Whole-sample Sharpe percentile intervals resample N observed days separately from horizon paths. Sharpe uses sample standard deviation, zero risk-free rate and the stated annual periods. Undefined zero-variance draws are counted and omitted from quantiles.",
            "The original pooled Sharpe differs from the unweighted mean of recomputed fold Sharpes. First-day cost inclusion can change both relative to legacy reports.",
            "Simulations do not validate alpha, repair look-ahead or reused holdouts, or create independent observations. No correction for strategy selection or multiple testing is applied.",
            "The optional 50/50 blend rebalances daily at fixed weights, with no additional allocation transaction costs. Benchmark and strategy use identical sampled indices; no weights are optimized.",
            "An explicit start/end window slices already-computed daily returns without resetting NAV; source initial cash still describes the original fold.",
        ],
    })
}

/// Write a deterministic empirical scenario report from saved daily fold JSON.
pub fn command(args: &[String]) -> Result<(), String> {
    let options = Options::parse(args)?;
    let report = run(&options)?;
    let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&options.out, bytes).map_err(|e| format!("{}: {e}", options.out.display()))?;
    eprintln!(
        "{} observed days, {} scenarios -> {}",
        report.observations,
        report.iterations,
        options.out.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "crypto-monte-carlo-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, file: &str, start: &str, changes: &[f64]) {
            let date: DateTime<Utc> = start.parse().unwrap();
            let mut nav = 100_000.0;
            let steps: Vec<_> = changes.iter().enumerate().map(|(i, change)| {
                nav *= 1.0 + change;
                serde_json::json!({"as_of": date + TimeDelta::days(i as i64), "nav": nav.to_string()})
            }).collect();
            std::fs::write(
                self.0.join(file),
                serde_json::to_vec(&serde_json::json!({"initial_cash": "100000", "steps": steps}))
                    .unwrap(),
            )
            .unwrap();
        }
        fn options(&self) -> Options {
            Options {
                folds: self.0.clone(),
                out: self.0.join("report.json"),
                benchmark: None,
                iterations: 200,
                seed: 42,
                block: 5,
                horizon: 365,
                annual_periods: 365.0,
                initial_cash: None,
                last_folds: None,
                window: None,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn correlation_distinguishes_a_hedge_from_identical_returns() {
        assert_eq!(correlation(&[1.0, -1.0, 2.0], &[1.0, -1.0, 2.0]), Some(1.0));
        assert_eq!(
            correlation(&[1.0, -1.0, 2.0], &[-1.0, 1.0, -2.0]),
            Some(-1.0)
        );
        assert_eq!(correlation(&[0.0, 0.0], &[1.0, -1.0]), None);
    }

    #[test]
    fn cropping_preserves_the_original_return_at_the_new_start() {
        let fixture = Fixture::new();
        fixture.write(
            "fold-1.json",
            "2025-01-01T00:00:00Z",
            &[0.1, -0.2, 0.3, -0.1],
        );
        let series = load(&fixture.0, None, 365.0, None).unwrap();
        let selected = crop(
            series,
            Some((
                "2025-01-02T00:00:00Z".parse().unwrap(),
                "2025-01-03T00:00:00Z".parse().unwrap(),
            )),
            365.0,
        )
        .unwrap();
        assert!((selected.returns[0] + 0.2).abs() < 1e-12);
        assert_eq!(selected.bounds, vec![(0, 2)]);
        assert_eq!(selected.sources[0].observations, 2);
    }

    #[test]
    fn first_step_cost_and_known_drawdown_are_retained() {
        let fixture = Fixture::new();
        fixture.write(
            "fold-1.json",
            "2025-01-01T00:00:00Z",
            &[-0.1, 0.2, -0.25, 0.1],
        );
        let series = load(&fixture.0, None, 365.0, None).unwrap();
        assert_eq!(series.returns.len(), 4);
        assert!((series.returns[0] + 0.1).abs() < 1e-12);
        let (change, drawdown) = path_metrics(series.returns).unwrap();
        assert!((change - (0.9 * 1.2 * 0.75 * 1.1 - 1.0)).abs() < 1e-12);
        assert!((drawdown - 0.25).abs() < 1e-12);
    }

    #[test]
    fn legacy_cash_is_explicit_and_metadata_wins() {
        let legacy = || {
            serde_json::from_value(serde_json::json!({"steps": [
                {"as_of": "2025-01-01T00:00:00Z", "nav": "99000"},
                {"as_of": "2025-01-02T00:00:00Z", "nav": "100000"}
            ]}))
            .unwrap()
        };
        assert!(fold_returns(legacy(), None)
            .unwrap_err()
            .contains("initial_cash"));
        let FoldReturns {
            returns,
            initial_cash_source: source,
            ..
        } = fold_returns(legacy(), Some(100_000.0)).unwrap();
        assert!((returns[0] + 0.01).abs() < 1e-12);
        assert_eq!(source, "cli");
        let fixture = Fixture::new();
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &[0.0, 0.1]);
        let series = load(&fixture.0, Some(2.0), 365.0, None).unwrap();
        assert_eq!(series.returns[0], 0.0);
    }

    #[test]
    fn blocks_preserve_clusters_and_stay_in_source_fold() {
        let fixture = Fixture::new();
        let changes: Vec<_> = (0..80).map(|i| if i < 40 { -0.02 } else { 0.02 }).collect();
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &changes);
        fixture.write("fold-2.json", "2025-03-22T00:00:00Z", &changes);
        let series = load(&fixture.0, None, 365.0, None).unwrap();
        let indices = sample_indices(&series, 10, 10_000, &mut Rng(42));
        for block in indices.chunks(10) {
            let start = block[0] / 80 * 80;
            for (offset, &index) in block.iter().enumerate() {
                assert_eq!(index, start + (block[0] - start + offset) % 80);
            }
        }
        let same_sign = indices
            .windows(2)
            .filter(|pair| series.returns[pair[0]].signum() == series.returns[pair[1]].signum())
            .count();
        assert!(same_sign as f64 / (indices.len() - 1) as f64 > 0.90);
    }

    #[test]
    fn deterministic_report_and_identical_paired_delta() {
        let fixture = Fixture::new();
        let changes: Vec<_> = (0..90).map(|i| (i % 9) as f64 * 0.003 - 0.01).collect();
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &changes);
        let mut options = fixture.options();
        options.benchmark = Some(fixture.0.clone());
        let first = serde_json::to_value(run(&options).unwrap()).unwrap();
        let second = serde_json::to_value(run(&options).unwrap()).unwrap();
        assert_eq!(first, second);
        let paired = &first["paired"];
        assert_eq!(paired["original_strategy_minus_benchmark_sharpe"], 0.0);
        for key in ["p025", "p05", "p50", "p95", "p975"] {
            assert_eq!(
                paired["strategy_minus_benchmark_sharpe_bootstrap"]["percentile_interval"][key],
                0.0
            );
        }
        assert_eq!(first["horizon_scenarios"], paired["benchmark_horizon"]);
        assert_eq!(
            first["horizon_scenarios"],
            paired["daily_rebalanced_50_50_blend_horizon"]
        );
        options.seed = 43;
        assert_ne!(
            first["horizon_scenarios"],
            serde_json::to_value(run(&options).unwrap()).unwrap()["horizon_scenarios"]
        );
    }

    #[test]
    fn last_folds_selects_by_date_and_retains_first_cost() {
        let fixture = Fixture::new();
        fixture.write("fold-2.json", "2025-01-01T00:00:00Z", &[0.02; 60]);
        fixture.write("fold-10.json", "2025-03-02T00:00:00Z", &[-0.01; 60]);
        let series = load(&fixture.0, None, 365.0, Some(1)).unwrap();
        assert_eq!(series.returns.len(), 60);
        assert!(series.sources[0].file.ends_with("fold-10.json"));
        assert!((series.returns[0] + 0.01).abs() < 1e-12);
        assert!(load(&fixture.0, None, 365.0, Some(3)).is_err());
    }

    #[test]
    fn malformed_dates_nav_and_cash_fail() {
        for (first, second, nav) in [
            ("2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z", "100"),
            ("2025-01-01T00:00:00Z", "2025-01-03T00:00:00Z", "100"),
            ("2025-01-01T00:00:00Z", "2025-01-01T12:00:00Z", "100"),
            ("2025-01-01T00:00:00Z", "2025-01-02T00:00:00Z", "NaN"),
            ("2025-01-01T00:00:00Z", "2025-01-02T00:00:00Z", "0"),
            ("2025-01-01T00:00:00Z", "2025-01-02T00:00:00Z", "-1"),
        ] {
            let fold: SavedFold =
                serde_json::from_value(serde_json::json!({"initial_cash": "100", "steps": [
                    {"as_of": first, "nav": "100"}, {"as_of": second, "nav": nav}
                ]}))
                .unwrap();
            assert!(fold_returns(fold, None).is_err());
        }
    }

    #[test]
    fn overlapping_and_gapped_folds_fail() {
        let fixture = Fixture::new();
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &[0.0; 60]);
        for start in ["2025-02-01T00:00:00Z", "2025-03-03T00:00:00Z"] {
            fixture.write("fold-2.json", start, &[0.0; 60]);
            assert!(load(&fixture.0, None, 365.0, None)
                .err()
                .unwrap()
                .contains("overlap or have a gap"));
        }
    }

    #[test]
    fn tiny_samples_invalid_blocks_and_unaligned_benchmark_fail() {
        let fixture = Fixture::new();
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &[0.01; 20]);
        assert!(run(&fixture.options()).err().unwrap().contains("60 daily"));
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &[0.01; 60]);
        let mut options = fixture.options();
        options.block = 31;
        assert!(run(&options).err().unwrap().contains("twice"));
        options.block = 5;
        let other = Fixture::new();
        other.write("fold-1.json", "2025-01-02T00:00:00Z", &[0.01; 60]);
        options.benchmark = Some(other.0.clone());
        assert!(run(&options).err().unwrap().contains("identical"));
        for (flag, value) in [
            ("--iterations", "0"),
            ("--block", "1"),
            ("--annual-periods", "NaN"),
            ("--horizon-days", "0"),
            ("--initial-cash", "-1"),
        ] {
            let args = ["--folds", "x", "--out", "y", flag, value].map(String::from);
            assert!(Options::parse(&args).is_err());
        }
    }

    #[test]
    fn command_writes_byte_identical_json_and_known_rng_stream() {
        let fixture = Fixture::new();
        let changes: Vec<_> = (0..60)
            .map(|i| if i % 2 == 0 { -0.01 } else { 0.02 })
            .collect();
        fixture.write("fold-1.json", "2025-01-01T00:00:00Z", &changes);
        let out = fixture.0.join("output.json");
        let args = vec![
            "--folds".into(),
            fixture.0.display().to_string(),
            "--out".into(),
            out.display().to_string(),
            "--iterations".into(),
            "100".into(),
        ];
        command(&args).unwrap();
        let first = std::fs::read(&out).unwrap();
        command(&args).unwrap();
        assert_eq!(first, std::fs::read(&out).unwrap());
        assert_eq!(Rng(0).next(), 0xe220a8397b1dcdaf);
        let mut values = [0.0, 1.0, 2.0];
        assert_eq!(quantiles(&mut values).p50, 1.0);
        assert_eq!(sharpe(&[0.0; 60], 365.0), None);
    }
}
