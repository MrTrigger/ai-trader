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

/// Fold results produced by `bin/walk-forward.sh`: one model per fold, each
/// trained only on data strictly before its test block.
///
/// This exists because `validate::walk_forward` does something different and
/// weaker — it slices ONE finished backtest into folds, which measures how
/// stable a single fit was, not how a model does on data it never saw. For an
/// untrained baseline the two are the same thing. For a trained candidate they
/// are not remotely the same thing, and the difference is the whole question.
#[derive(Debug, serde::Serialize)]
pub struct RetrainedFolds {
    pub source: String,
    pub sharpes: Vec<f64>,
    pub returns: Vec<f64>,
    /// Rebalances per fold, so "was the sample adequate" is asked of the
    /// evidence that was actually used rather than of a separate replay.
    pub counts: Vec<usize>,
}

impl RetrainedFolds {
    /// Read `fold-*.json` from a walk-forward run directory.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("fold-") && n.ends_with(".json"))
            })
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!("{}: no fold-*.json", dir.display()));
        }
        let (mut sharpes, mut returns, mut counts) = (Vec::new(), Vec::new(), Vec::new());
        for f in &files {
            let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", f.display()))?;
            let m = &v["metrics"];
            sharpes.push(
                m["sharpe"]
                    .as_f64()
                    .ok_or_else(|| format!("{}: no metrics.sharpe", f.display()))?,
            );
            returns.push(
                m["total_return"]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .ok_or_else(|| format!("{}: no metrics.total_return", f.display()))?,
            );
            counts.push(m["n"].as_u64().unwrap_or_default() as usize);
        }
        Ok(Self {
            source: dir.display().to_string(),
            sharpes,
            returns,
            counts,
        })
    }

    pub fn mean_sharpe(&self) -> f64 {
        mean(&self.sharpes)
    }

    pub fn positive(&self) -> usize {
        self.sharpes.iter().filter(|s| **s > 0.0).count()
    }

    pub fn rebalances(&self) -> usize {
        self.counts.iter().sum()
    }

    /// Compounded across folds, which is what a book held through all of them
    /// would have earned.
    pub fn compounded(&self) -> f64 {
        self.returns.iter().fold(1.0, |acc, r| acc * (1.0 + r)) - 1.0
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// How much the candidate must beat the baseline by, in Sharpe, over
/// per-fold-retrained out-of-sample blocks.
///
/// Asserted in advance and not tuned afterwards, which is the only thing that
/// makes a margin mean anything. A candidate that merely edges past a dumb
/// baseline has not earned the complexity it costs.
pub const BASELINE_SHARPE_MARGIN: f64 = 0.4;

/// Where the honest folds live. Both come from `bin/walk-forward.sh`: one
/// model per fold, trained only on data strictly before its own test block.
/// The thing the candidate has to beat: a strategy with no model to leak.
#[derive(Debug)]
pub struct Baseline<'a> {
    pub signal: &'a str,
    pub constructor: &'a str,
}

#[derive(Debug, Default)]
pub struct Evidence<'a> {
    /// Required when the candidate is a trained model.
    pub retrained: Option<&'a Path>,
    /// The same folds replayed at twice the modelled slippage.
    pub stressed: Option<&'a Path>,
}

/// Slack for binary representation only. A margin of 1.4 - 1.0 is 0.39999...
/// in f64, and failing a gate on the sixteenth decimal place is an arithmetic
/// artefact, not a verdict about a strategy. Far too small to admit anything
/// that genuinely missed.
const MARGIN_EPSILON: f64 = 1e-9;

/// Does this margin clear the bar?
pub fn clears_margin(margin: f64) -> bool {
    margin >= BASELINE_SHARPE_MARGIN - MARGIN_EPSILON
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
    /// Present when the candidate was judged on properly retrained folds.
    pub retrained: Option<RetrainedFolds>,
    /// Those same folds at twice the modelled slippage.
    pub stressed_folds: Option<RetrainedFolds>,
}

pub fn run(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    root: &Path,
    initial_cash: Decimal,
    baseline: &Baseline<'_>,
    evidence: &Evidence<'_>,
) -> Result<GateResult, String> {
    let (baseline_signal, baseline_constructor) = (baseline.signal, baseline.constructor);
    let retrained = evidence.retrained.map(RetrainedFolds::load).transpose()?;
    let stressed_folds = evidence.stressed.map(RetrainedFolds::load).transpose()?;
    // Fail closed. Slicing one fit of a model trained across the whole window
    // and calling the pieces "out of sample" is how a gate passes something it
    // should have stopped; the number would be honest-looking and leaked.
    if cfg.signal == "ml_ranker" && retrained.is_none() {
        return Err(
            "the candidate is a trained model, so the gate needs folds from \
             bin/walk-forward.sh (--retrained DIR). validate::walk_forward slices one \
             finished backtest, which measures the stability of a single fit and would \
             score this model on data it was trained on."
                .into(),
        );
    }
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
    // The baseline is not trained, so slicing its single backtest into folds
    // IS walk-forward for it; there is nothing it could have memorised.
    let baseline_sharpe = mean(
        &baseline_walk
            .folds
            .iter()
            .map(|f| f.test.metrics.sharpe)
            .collect::<Vec<_>>(),
    );
    let criteria = vec![
        match &retrained {
            Some(f) => Criterion {
                name: "positive expectancy after costs, retrained per fold".into(),
                passed: f.compounded() > 0.0,
                detail: format!(
                    "{:+.1}% compounded across {} folds, {} rebalances",
                    f.compounded() * 100.0,
                    f.returns.len(),
                    f.rebalances()
                ),
            },
            None => Criterion {
                name: "positive expectancy after costs".into(),
                passed: candidate.metrics.total_return > Decimal::ZERO,
                detail: format!(
                    "{:+.2}% over {} rebalances, {:.1}bps of cost drag",
                    pct(candidate.metrics.total_return),
                    candidate.metrics.n,
                    candidate.metrics.cost_drag_bps.to_f64().unwrap_or_default()
                ),
            },
        },
        match (&retrained, &stressed_folds) {
            (Some(base), Some(stress)) => Criterion {
                name: "survives 2x slippage, retrained per fold".into(),
                passed: stress.compounded() > 0.0
                    && stress.positive() == stress.sharpes.len(),
                detail: format!(
                    "{:+.1}% compounded at 2x vs {:+.1}% at 1x; {}/{} folds still positive; \
                     mean Sharpe {:.2} vs {:.2}; folds from {}",
                    stress.compounded() * 100.0,
                    base.compounded() * 100.0,
                    stress.positive(),
                    stress.sharpes.len(),
                    stress.mean_sharpe(),
                    base.mean_sharpe(),
                    stress.source
                ),
            },
            // Refuse rather than report the whole-window replay, which for a
            // trained model is either leaked or - as the leakage guard makes
            // it - empty. "+0.00% at 2x over 0 rebalances" is not a pass and
            // must not be allowed to look like one.
            (Some(_), None) => Criterion {
                name: "survives 2x slippage, retrained per fold".into(),
                passed: false,
                detail: "not run: needs --retrained-2x DIR, a per-fold replay at twice \
                         the modelled slippage. The same fold models, so it is a replay \
                         and not a retrain."
                    .into(),
            },
            _ => Criterion {
                name: "survives 2x slippage".into(),
                passed: stressed.metrics.total_return > Decimal::ZERO,
                detail: format!(
                    "{:+.2}% at 2x (vs {:+.2}% at 1x)",
                    pct(stressed.metrics.total_return),
                    pct(candidate.metrics.total_return)
                ),
            },
        },
        match &retrained {
            // The real test: models that never saw their own test block,
            // against a baseline that needs no training, by a margin fixed in
            // advance.
            Some(folds) => {
                let margin = folds.mean_sharpe() - baseline_sharpe;
                Criterion {
                    name: format!(
                        "beats the baseline by {BASELINE_SHARPE_MARGIN:.1} Sharpe, retrained per fold"
                    ),
                    passed: clears_margin(margin)
                        && folds.positive() == folds.sharpes.len(),
                    detail: format!(
                        "{}/{} folds positive, mean Sharpe {:.2} vs baseline {:.2} \
                         (margin {:+.2}, needs {:+.2}); folds from {}",
                        folds.positive(),
                        folds.sharpes.len(),
                        folds.mean_sharpe(),
                        baseline_sharpe,
                        margin,
                        BASELINE_SHARPE_MARGIN,
                        folds.source
                    ),
                }
            }
            // Only reachable for an untrained candidate, where slicing one
            // backtest genuinely is what walk-forward means.
            None => Criterion {
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
        },
        match &retrained {
            Some(f) => Criterion {
                name: "sample adequate, or it says so".into(),
                passed: f.rebalances() >= backtest::INSUFFICIENT_SAMPLE_N,
                detail: format!(
                    "n={} rebalances across {} folds (floor {})",
                    f.rebalances(),
                    f.counts.len(),
                    backtest::INSUFFICIENT_SAMPLE_N
                ),
            },
            None => Criterion {
                name: "sample adequate, or it says so".into(),
                passed: !candidate.metrics.insufficient_sample,
                detail: format!(
                    "n={} rebalances (floor {})",
                    candidate.metrics.n,
                    backtest::INSUFFICIENT_SAMPLE_N
                ),
            },
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
        retrained,
        stressed_folds,
    })
}

#[cfg(test)]
mod margin_tests {
    use super::*;

    fn folds(sharpes: &[f64]) -> RetrainedFolds {
        RetrainedFolds {
            source: "test".into(),
            sharpes: sharpes.to_vec(),
            returns: sharpes.iter().map(|_| 0.1).collect(),
            counts: sharpes.iter().map(|_| 237).collect(),
        }
    }

    #[test]
    fn a_margin_of_exactly_four_tenths_passes_and_a_hair_under_does_not() {
        // Fixed in advance so the boundary is a decision, not a discovery.
        let baseline = 1.0;
        assert!(
            clears_margin(folds(&[1.4, 1.4]).mean_sharpe() - baseline),
            "exactly on the bar clears it; 1.4 - 1.0 is 0.39999... in f64 and \
             that is arithmetic, not a verdict"
        );
        assert!(!clears_margin(
            folds(&[1.39, 1.39]).mean_sharpe() - baseline
        ));
        assert!(!clears_margin(folds(&[1.2, 1.2]).mean_sharpe() - baseline));
    }

    #[test]
    fn one_negative_fold_fails_however_good_the_mean() {
        // A single fit that carried the whole record is the failure mode the
        // fold count exists to catch, so the mean cannot buy its way past it.
        let f = folds(&[6.0, 6.0, -0.1]);
        assert!(clears_margin(f.mean_sharpe() - 1.0));
        assert_ne!(f.positive(), f.sharpes.len());
    }

    #[test]
    fn the_frozen_config_folds_load_and_are_all_positive() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../var/research/wf-rank-2020");
        if !dir.exists() {
            return; // research artefacts are not required to build
        }
        let f = RetrainedFolds::load(&dir).expect("folds load");
        assert_eq!(f.sharpes.len(), 9);
        assert_eq!(f.positive(), 9, "9/9 is the recorded evidence");
        assert!(f.mean_sharpe() > 2.0, "got {}", f.mean_sharpe());
        assert!(
            f.rebalances() > crate::backtest::INSUFFICIENT_SAMPLE_N,
            "2139 rebalances across nine folds, not the 0 a whole-window replay \
             of one model produces once the leakage guard refuses every date"
        );
    }
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
