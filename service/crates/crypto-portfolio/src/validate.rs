//! Time-ordered validation over one continuous portfolio replay.

use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
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
    let prepared = backtest::prepare(first, root, first.signal == "ml_ranker")?;
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
