//! Phase 1 portfolio gate, evaluated from Rust backtests and Rust validation.

use std::path::Path;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::backtest::{self, Metrics};
use crate::config::Config;
use crate::validate::{self, Holdout, WalkForward};

#[derive(Debug, serde::Serialize)]
pub struct Criterion {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, serde::Serialize)]
pub struct GateResult {
    pub candidate: String,
    pub baseline: String,
    pub passed: bool,
    pub criteria: Vec<Criterion>,
    pub disclosures: Vec<String>,
    pub candidate_metrics: Metrics,
    pub baseline_metrics: Metrics,
    pub stressed_metrics: Metrics,
    pub walk: WalkForward,
    pub holdout: Holdout,
}

pub fn run(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    baseline_signal: &str,
    baseline_constructor: &str,
) -> Result<GateResult, String> {
    let prepared = backtest::prepare(
        cfg,
        root,
        cfg.signal == "ml_ranker" || baseline_signal == "ml_ranker",
        features_crypto::FundingWindow::Trailing,
    )?;
    let candidate =
        backtest::replay_prepared(cfg, start, end, root, initial_cash, Decimal::ONE, &prepared)?;
    let stressed = backtest::replay_prepared(
        cfg,
        start,
        end,
        root,
        initial_cash,
        Decimal::from(2),
        &prepared,
    )?;
    let mut baseline_cfg = cfg.clone();
    baseline_cfg.signal = baseline_signal.into();
    baseline_cfg.constructor = baseline_constructor.into();
    let baseline = backtest::replay_prepared(
        &baseline_cfg,
        start,
        end,
        root,
        initial_cash,
        Decimal::ONE,
        &prepared,
    )?;
    let walk = validate::walk_forward(&candidate, 4, 0.6)?;
    let baseline_walk = validate::walk_forward(&baseline, 4, 0.6)?;
    let holdout = validate::holdout(&candidate, 0.7)?;
    let oos_candidate = oos(&walk);
    let oos_baseline = oos(&baseline_walk);
    let criteria = vec![
        Criterion {
            name: "positive expectancy after costs".into(),
            passed: candidate.metrics.total_return > Decimal::ZERO,
            detail: format!(
                "{:+.2}% over {} rebalances, {:.1}bps of cost drag",
                pct(candidate.metrics.total_return),
                candidate.metrics.n,
                candidate.metrics.cost_drag_bps.to_f64().unwrap_or_default()
            ),
        },
        Criterion {
            name: "survives 2x slippage".into(),
            passed: stressed.metrics.total_return > Decimal::ZERO,
            detail: format!(
                "{:+.2}% at 2x (vs {:+.2}% at 1x)",
                pct(stressed.metrics.total_return),
                pct(candidate.metrics.total_return)
            ),
        },
        Criterion {
            name: "walk-forward beats the baseline".into(),
            passed: walk.consistent && oos_candidate > oos_baseline,
            detail: format!(
                "{}/{} candidate folds positive vs {}/{} baseline; out-of-sample {oos_candidate:+.2}% vs {oos_baseline:+.2}%",
                walk.positive_folds,
                walk.folds.len(),
                baseline_walk.positive_folds,
                baseline_walk.folds.len()
            ),
        },
        Criterion {
            name: "sample adequate, or it says so".into(),
            passed: !candidate.metrics.insufficient_sample,
            detail: format!(
                "n={} rebalances (floor {})",
                candidate.metrics.n,
                backtest::INSUFFICIENT_SAMPLE_N
            ),
        },
    ];
    let passed = !criteria.is_empty() && criteria.iter().all(|criterion| criterion.passed);
    let mut disclosures = candidate.disclosures.clone();
    if walk.folds.is_empty() {
        disclosures.push("walk-forward produced no folds: too few rebalances to split; the out-of-sample criterion is reporting nothing, not passing".into());
    }
    if !holdout.consistent {
        disclosures.push(format!(
            "holdout: train {:+.2}%, test {:+.2}%; the sign did not hold out of sample",
            pct(holdout.train.metrics.total_return),
            pct(holdout.test.metrics.total_return)
        ));
    }
    disclosures.push("this gate measures the portfolio, not the signal; information coefficient must be evaluated separately".into());
    Ok(GateResult {
        candidate: format!("{} + {}", cfg.signal, cfg.constructor),
        baseline: format!("{baseline_signal} + {baseline_constructor}"),
        passed,
        criteria,
        disclosures,
        candidate_metrics: candidate.metrics,
        baseline_metrics: baseline.metrics,
        stressed_metrics: stressed.metrics,
        walk,
        holdout,
    })
}

fn oos(walk: &WalkForward) -> f64 {
    if walk.folds.is_empty() {
        0.0
    } else {
        walk.folds
            .iter()
            .map(|fold| fold.test.metrics.total_return.to_f64().unwrap_or_default())
            .sum::<f64>()
            / walk.folds.len() as f64
            * 100.0
    }
}

fn pct(value: Decimal) -> f64 {
    value.to_f64().unwrap_or_default() * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_walk_forward_cannot_beat_anything() {
        let walk = WalkForward {
            folds: Vec::new(),
            positive_folds: 0,
            consistent: false,
        };
        assert_eq!(oos(&walk), 0.0);
        assert!(!walk.consistent);
    }
}
