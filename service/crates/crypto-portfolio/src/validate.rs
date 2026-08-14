//! Time-ordered validation over one continuous portfolio replay.

use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::backtest::{self, BacktestResult, Metrics, Step};
use crate::config::Config;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Window {
    pub name: String,
    pub dates: Vec<NaiveDate>,
    pub metrics: Metrics,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Holdout {
    pub train: Window,
    pub test: Window,
    pub consistent: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WalkForward {
    pub folds: Vec<Holdout>,
    pub positive_folds: usize,
    pub consistent: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepPoint {
    pub value: String,
    pub metrics: Metrics,
    pub holds_up: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Plateau {
    pub axis: String,
    pub values: Vec<String>,
    pub centre: Option<String>,
    pub width: usize,
    pub is_peak: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Sweep {
    pub axis: String,
    pub points: Vec<SweepPoint>,
    pub plateau: Plateau,
    pub disclosures: Vec<String>,
}

fn window(name: &str, steps: &[Step]) -> Window {
    Window {
        name: name.into(),
        dates: steps.iter().map(|step| step.as_of.date_naive()).collect(),
        metrics: backtest::metrics(steps),
    }
}

pub fn holdout(result: &BacktestResult, train_fraction: f64) -> Result<Holdout, String> {
    if !(0.0..1.0).contains(&train_fraction) || train_fraction == 0.0 {
        return Err(format!(
            "train_fraction must be in (0, 1), got {train_fraction}"
        ));
    }
    let mut steps: Vec<_> = result.steps.iter().collect();
    steps.sort_by_key(|step| step.as_of);
    let cut = (steps.len() as f64 * train_fraction) as usize;
    let owned: Vec<_> = steps.into_iter().cloned().collect();
    let train = window("train", &owned[..cut]);
    let test = window("test", &owned[cut..]);
    let consistent = train.metrics.total_return > rust_decimal::Decimal::ZERO
        && test.metrics.total_return > rust_decimal::Decimal::ZERO;
    Ok(Holdout {
        train,
        test,
        consistent,
    })
}

pub fn walk_forward(
    result: &BacktestResult,
    folds: usize,
    train_fraction: f64,
) -> Result<WalkForward, String> {
    if folds == 0 {
        return Err("folds must be positive".into());
    }
    if !(0.0..1.0).contains(&train_fraction) || train_fraction == 0.0 {
        return Err(format!(
            "train_fraction must be in (0, 1), got {train_fraction}"
        ));
    }
    let mut steps = result.steps.clone();
    steps.sort_by_key(|step| step.as_of);
    if steps.len() < folds * 2 {
        return Ok(WalkForward {
            folds: Vec::new(),
            positive_folds: 0,
            consistent: false,
        });
    }
    let block = steps.len() / folds;
    let train_size = ((block as f64 * train_fraction) as usize).max(1);
    let mut out = Vec::new();
    for index in 0..folds {
        let start = index * block;
        let end = if index < folds - 1 {
            start + block
        } else {
            steps.len()
        };
        let chunk = &steps[start..end];
        if chunk.len() < 2 {
            continue;
        }
        let train = window(&format!("fold{index}-train"), &chunk[..train_size]);
        let test = window(&format!("fold{index}-test"), &chunk[train_size..]);
        let consistent = train.metrics.total_return > rust_decimal::Decimal::ZERO
            && test.metrics.total_return > rust_decimal::Decimal::ZERO;
        out.push(Holdout {
            train,
            test,
            consistent,
        });
    }
    let positive_folds = out
        .iter()
        .filter(|fold| fold.test.metrics.total_return > rust_decimal::Decimal::ZERO)
        .count();
    let consistent = !out.is_empty() && positive_folds * 2 > out.len();
    Ok(WalkForward {
        folds: out,
        positive_folds,
        consistent,
    })
}

pub fn find_plateau(axis: &str, points: &[SweepPoint]) -> Plateau {
    let mut best = (0, 0);
    let mut run_start = None;
    for index in 0..=points.len() {
        let holds = points.get(index).is_some_and(|point| point.holds_up);
        if holds && run_start.is_none() {
            run_start = Some(index);
        } else if !holds {
            if let Some(start) = run_start.take() {
                if index - start > best.1 - best.0 {
                    best = (start, index);
                }
            }
        }
    }
    let values = points[best.0..best.1]
        .iter()
        .map(|point| point.value.clone())
        .collect::<Vec<_>>();
    let centre = values.get(values.len() / 2).cloned();
    Plateau {
        axis: axis.into(),
        width: values.len(),
        is_peak: values.len() == 1,
        values,
        centre,
    }
}

/// Replay one ordered axis. The caller supplies already-mutated configs so the
/// API cannot quietly turn this into a multidimensional optimiser.
pub fn sweep_axis(
    axis: &str,
    configs: Vec<(String, Config)>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
) -> Result<Sweep, String> {
    let (_, first) = configs.first().ok_or("sweep needs at least one value")?;
    if configs.iter().any(|(_, cfg)| cfg.signal != first.signal) {
        return Err("a validation sweep may not change the signal axis".into());
    }
    let prepared = backtest::prepare(
        first,
        root,
        first.signal == "ml_ranker",
        features_crypto::FundingWindow::Trailing,
    )?;
    let mut points = Vec::new();
    for (value, config) in configs {
        let result = backtest::replay_prepared(
            &config,
            start,
            end,
            root,
            initial_cash,
            Decimal::ONE,
            &prepared,
        )?;
        points.push(SweepPoint {
            value,
            holds_up: result.metrics.total_return > Decimal::ZERO,
            metrics: result.metrics,
        });
    }
    let plateau = find_plateau(axis, &points);
    let mut disclosures = Vec::new();
    if plateau.is_peak {
        disclosures.push(format!(
            "{axis}: the widest run that held up is one value wide; that is a peak, not a plateau"
        ));
    }
    if plateau.values.is_empty() {
        disclosures.push(format!(
            "{axis}: no value held up anywhere on the swept range"
        ));
    }
    if points.iter().any(|point| point.metrics.insufficient_sample) {
        disclosures.push(format!(
            "{axis}: at least one point has an inadequate sample; its result establishes nothing"
        ));
    }
    Ok(Sweep {
        axis: axis.into(),
        points,
        plateau,
        disclosures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use plan::Status;
    use rust_decimal::Decimal;

    fn result(count: usize, rising: bool) -> BacktestResult {
        let start = "2025-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let steps = (0..count)
            .map(|index| Step {
                as_of: start + Duration::days(index as i64),
                nav: Decimal::from(if rising {
                    100_000 + index as i64 * 100
                } else {
                    100_000 - index as i64 * 100
                }),
                cash: Decimal::ZERO,
                gross_exposure: Decimal::ZERO,
                status: Status::Accepted,
                fills: Vec::new(),
                plan_id: uuid::Uuid::nil(),
                warnings: Vec::new(),
            })
            .collect::<Vec<_>>();
        BacktestResult {
            metrics: backtest::metrics(&steps),
            steps,
            disclosures: Vec::new(),
            slippage_multiple: Decimal::ONE,
        }
    }

    #[test]
    fn holdout_is_forward_and_unshuffled() {
        let out = holdout(&result(40, true), 0.7).unwrap();
        assert_eq!(out.train.dates.len(), 28);
        assert_eq!(out.test.dates.len(), 12);
        assert!(out.train.dates.last() < out.test.dates.first());
        assert!(out.consistent);
    }

    #[test]
    fn walk_forward_test_windows_do_not_overlap() {
        let out = walk_forward(&result(80, true), 4, 0.6).unwrap();
        assert_eq!(out.folds.len(), 4);
        assert_eq!(out.positive_folds, 4);
        let mut seen = std::collections::BTreeSet::new();
        for fold in out.folds {
            for date in fold.test.dates {
                assert!(seen.insert(date));
            }
        }
    }

    #[test]
    fn thin_history_does_not_invent_folds() {
        let out = walk_forward(&result(4, true), 4, 0.6).unwrap();
        assert!(out.folds.is_empty());
        assert!(!out.consistent);
    }

    #[test]
    fn widest_contiguous_run_wins_and_returns_its_centre() {
        let metrics = result(2, true).metrics;
        let points = [false, true, true, true, false, true]
            .into_iter()
            .enumerate()
            .map(|(index, holds_up)| SweepPoint {
                value: index.to_string(),
                metrics: metrics.clone(),
                holds_up,
            })
            .collect::<Vec<_>>();
        let out = find_plateau("holdings", &points);
        assert_eq!(out.values, ["1", "2", "3"]);
        assert_eq!(out.centre.as_deref(), Some("2"));
        assert!(!out.is_peak);
    }
}

// --- block bootstrap ---------------------------------------------------------

/// Deterministic PRNG, so a confidence interval is reproducible.
///
/// A bootstrap whose answer moves between runs is a bad instrument for
/// settling arguments, and "run it again and see" is how a marginal result
/// becomes whichever one the reader wanted. xorshift64* is more than enough
/// for resampling indices and costs no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// What a comparison between two configurations is actually worth.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BootstrapDelta {
    pub metric: String,
    pub observed: f64,
    /// Percentiles of the resampled delta, in the caller's units.
    pub p05: f64,
    pub p50: f64,
    pub p95: f64,
    /// Share of resamples where the variant beat the baseline. Not a p-value,
    /// and reported as what it is.
    pub share_positive: f64,
    pub block: usize,
    pub iterations: usize,
    pub steps: usize,
}

impl BootstrapDelta {
    /// Does the interval exclude "no difference"?
    pub fn conclusive(&self) -> bool {
        (self.p05 > 0.0) || (self.p95 < 0.0)
    }
}

/// Sharpe of a return series, annualised on the caller's periods-per-year.
fn sharpe_of(returns: &[f64], periods: f64) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    if var <= 0.0 {
        return 0.0;
    }
    mean / var.sqrt() * periods.sqrt()
}

/// Confidence interval on the difference between two runs of the same dates.
///
/// Nine walk-forward folds give nine observations, and a standard error built
/// on nine points is a weak instrument for a decision about real money — the
/// turnover-cap comparison came out at +0.25 Sharpe with a two-standard-error
/// band of ±0.57, which is to say it straddled zero while looking decisive in
/// its compounded form.
///
/// Resampling CONTIGUOUS blocks rather than individual days is the point:
/// daily returns are autocorrelated and a strategy's drawdowns are runs, not
/// scattered bad days. Sampling days independently would break exactly the
/// structure that makes a drawdown a drawdown, and would report a tighter
/// interval than the evidence supports.
///
/// Paired by date. Both runs must cover the same steps, because the question
/// is what the CHANGE did, not what two different periods happened to return.
pub fn bootstrap_delta(
    baseline: &[(DateTime<Utc>, f64)],
    variant: &[(DateTime<Utc>, f64)],
    block: usize,
    iterations: usize,
    periods_per_year: f64,
    seed: u64,
) -> Result<BootstrapDelta, String> {
    if baseline.len() != variant.len() {
        return Err(format!(
            "paired bootstrap needs the same steps in both runs: {} vs {}",
            baseline.len(),
            variant.len()
        ));
    }
    for (a, b) in baseline.iter().zip(variant) {
        if a.0 != b.0 {
            return Err(format!("steps do not line up: {} vs {}", a.0, b.0));
        }
    }
    let n = baseline.len();
    if n < block * 2 || block == 0 {
        return Err(format!(
            "{n} steps cannot be block-bootstrapped at block {block}"
        ));
    }
    let (base_r, var_r): (Vec<f64>, Vec<f64>) =
        baseline.iter().zip(variant).map(|(a, b)| (a.1, b.1)).unzip();
    let observed = sharpe_of(&var_r, periods_per_year) - sharpe_of(&base_r, periods_per_year);

    let blocks = n.div_ceil(block);
    let mut rng = Rng::new(seed);
    let mut deltas = Vec::with_capacity(iterations);
    let (mut rb, mut rv) = (Vec::with_capacity(n + block), Vec::with_capacity(n + block));
    for _ in 0..iterations {
        rb.clear();
        rv.clear();
        for _ in 0..blocks {
            // Wrapping, so every step has an equal chance of appearing rather
            // than the tail being under-sampled for want of room after it.
            let start = rng.below(n);
            for k in 0..block {
                let i = (start + k) % n;
                rb.push(base_r[i]);
                rv.push(var_r[i]);
            }
        }
        rb.truncate(n);
        rv.truncate(n);
        deltas.push(sharpe_of(&rv, periods_per_year) - sharpe_of(&rb, periods_per_year));
    }
    let positive = deltas.iter().filter(|d| **d > 0.0).count() as f64 / iterations as f64;
    deltas.sort_by(f64::total_cmp);
    let at = |q: f64| deltas[((deltas.len() - 1) as f64 * q).round() as usize];
    Ok(BootstrapDelta {
        metric: "sharpe".into(),
        observed,
        p05: at(0.05),
        p50: at(0.50),
        p95: at(0.95),
        share_positive: positive,
        block,
        iterations,
        steps: n,
    })
}

/// Per-step returns from a run's NAV path, oldest first.
pub fn step_returns(result: &BacktestResult) -> Vec<(DateTime<Utc>, f64)> {
    let mut steps: Vec<&Step> = result.steps.iter().collect();
    steps.sort_by_key(|s| s.as_of);
    let mut out = Vec::new();
    for pair in steps.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let (Some(prev), Some(next)) = (a.nav.to_f64(), b.nav.to_f64()) else {
            continue;
        };
        if prev > 0.0 {
            out.push((b.as_of, next / prev - 1.0));
        }
    }
    out
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;

    fn series(vals: &[f64]) -> Vec<(DateTime<Utc>, f64)> {
        vals.iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    DateTime::from_timestamp(1_600_000_000 + i as i64 * 86_400, 0).unwrap(),
                    *v,
                )
            })
            .collect()
    }

    #[test]
    fn an_identical_variant_has_no_measurable_edge() {
        let a = series(&(0..300).map(|i| ((i % 7) as f64 - 3.0) / 100.0).collect::<Vec<_>>());
        let r = bootstrap_delta(&a, &a, 20, 400, 365.0, 42).unwrap();
        assert_eq!(r.observed, 0.0);
        assert_eq!(r.p05, 0.0);
        assert_eq!(r.p95, 0.0);
        assert!(!r.conclusive(), "no difference cannot be a conclusive one");
    }

    #[test]
    fn a_variant_that_is_better_every_single_day_is_conclusive() {
        let base: Vec<f64> = (0..300).map(|i| ((i % 11) as f64 - 5.0) / 100.0).collect();
        let up: Vec<f64> = base.iter().map(|r| r + 0.004).collect();
        let r = bootstrap_delta(&series(&base), &series(&up), 20, 400, 365.0, 7).unwrap();
        assert!(r.observed > 0.0);
        assert!(r.p05 > 0.0, "p05 {} should exclude zero", r.p05);
        assert!(r.conclusive());
        assert!(r.share_positive > 0.99);
    }

    #[test]
    fn it_is_reproducible_and_refuses_mismatched_runs() {
        let a = series(&(0..300).map(|i| ((i % 5) as f64 - 2.0) / 100.0).collect::<Vec<_>>());
        let b = series(&(0..300).map(|i| ((i % 5) as f64 - 1.9) / 100.0).collect::<Vec<_>>());
        let one = bootstrap_delta(&a, &b, 20, 200, 365.0, 99).unwrap();
        let two = bootstrap_delta(&a, &b, 20, 200, 365.0, 99).unwrap();
        assert_eq!(one.p05, two.p05, "same seed, same interval");
        assert_eq!(one.p95, two.p95);

        let short = series(&[0.01, 0.02, 0.03]);
        assert!(bootstrap_delta(&a, &short, 20, 100, 365.0, 1).is_err());
    }

    /// Why blocks at all: resampling days independently breaks the runs that
    /// make a drawdown a drawdown, and reports a tighter interval than the
    /// evidence supports.
    ///
    /// Note this is a claim about block 1 versus a block spanning the
    /// correlation length, NOT that width grows with block length in general.
    /// It does not: a block equal to a periodic series' period samples one
    /// whole cycle every time and collapses the variance instead.
    #[test]
    fn independent_resampling_understates_the_interval() {
        // Persistent but not periodic: an AR(1) walk, so no block length can
        // align with a cycle and flatter itself.
        let mut state = 0.0_f64;
        let mut seed = 12345_u64;
        let base: Vec<f64> = (0..400)
            .map(|_| {
                seed ^= seed >> 12;
                seed ^= seed << 25;
                seed ^= seed >> 27;
                let shock = ((seed >> 11) as f64 / (1u64 << 53) as f64) - 0.5;
                state = 0.94 * state + 0.06 * shock;
                state / 20.0
            })
            .collect();
        let up: Vec<f64> = base.iter().map(|r| r + 0.0004).collect();
        let (a, b) = (series(&base), series(&up));
        let independent = bootstrap_delta(&a, &b, 1, 600, 365.0, 3).unwrap();
        let blocked = bootstrap_delta(&a, &b, 25, 600, 365.0, 3).unwrap();
        assert!(
            blocked.p95 - blocked.p05 > independent.p95 - independent.p05,
            "block 25 width {} must exceed independent width {} — otherwise the \
             bootstrap is just pretending the days are independent",
            blocked.p95 - blocked.p05,
            independent.p95 - independent.p05
        );
    }
}
