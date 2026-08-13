import math

import pytest

from summarize_stockholm import active_return_tstat, sharpe_standard_error, summarize


def make_fold(period_returns, benchmark_period_returns, *, cadence_sessions=20):
    """A minimal synthetic Rust fold report `summarize()` can stitch.

    Every field `summarize()` reads unconditionally (not via `dict.get` with a
    default) is present; everything else is omitted so the defaults exercise
    too.
    """
    steps = [
        {"period_return": bot, "benchmark_period_return": benchmark}
        for bot, benchmark in zip(period_returns, benchmark_period_returns)
    ]
    bucket = {
        "observations": 0,
        "mean_prediction": 0.0,
        "mean_realised_return": 0.0,
        "directional_accuracy": 0.0,
    }
    return {
        "model_family": "lightgbm",
        "feature_set_version": "fixture",
        "reward": "absolute_return",
        "objective": "l1",
        "cadence_sessions": cadence_sessions,
        "survivorship_status": "POINT_IN_TIME",
        "start": "2024-01-02",
        "end": "2024-02-01",
        "steps": steps,
        "metrics": {
            "total_return": sum(period_returns),
            "sharpe": 0.0,
            "max_drawdown": 0.0,
            "mean_gross": 1.0,
            "mean_net": 1.0,
            "long_pnl": 0.0,
            "short_pnl": 0.0,
            "cost_drag": 0.0,
            "long_positions": 0,
            "short_positions": 0,
            "periods": len(period_returns),
        },
        "diagnostics": {
            "observations": 1,
            "decision_dates": 1,
            "mean_rank_ic": 0.05,
            "positive_rank_ic_dates": 1,
            "directional_accuracy": 0.5,
            "mean_absolute_error": 0.01,
            "buckets": [dict(bucket) for _ in range(10)],
        },
        "benchmark": {
            "symbol": "OMXSGI",
            "name": "OMX Stockholm Gross Index",
            "return_type": "gross_total_return",
            "currency": "SEK",
            "source": "fixture",
            "total_return": sum(benchmark_period_returns),
            "sharpe": 0.0,
            "portfolio_minus_benchmark_total_return": sum(period_returns)
            - sum(benchmark_period_returns),
        },
        "disclosures": ["fixture disclosure"],
    }


def two_fold_summary(**kwargs):
    folds = [
        make_fold([0.02, 0.01], [0.01, 0.015]),
        make_fold([0.03, -0.01], [0.005, -0.02]),
    ]
    defaults = {"min_sharpe": 2.0, "risk_free_annual": 0.02, "target_sharpe_floor": 1.0}
    defaults.update(kwargs)
    return summarize(folds, **defaults)


def test_active_tstat_matches_a_hand_computed_value_across_two_folds():
    # Concatenated: returns = [0.02, 0.01, 0.03, -0.01],
    # benchmark = [0.01, 0.015, 0.005, -0.02].
    # active = [0.01, -0.005, 0.025, 0.01]; mean 0.01, sample (N-1-divisor)
    # std, SE = std/sqrt(4); t = mean/SE.
    report = two_fold_summary()
    active = [0.01, -0.005, 0.025, 0.01]
    mean_active = sum(active) / 4
    sample_variance = sum((a - mean_active) ** 2 for a in active) / 3
    expected = mean_active / (math.sqrt(sample_variance) / math.sqrt(4))
    assert report["active_tstat"] == pytest.approx(expected, abs=1e-9)
    assert report["active_tstat"] == pytest.approx(1.632993161855452, abs=1e-9)


def test_sharpe_and_sharpe_se_reflect_the_risk_free_rate():
    # periods_per_year = 252/20 = 12.6; per-period rf = 1.02**(1/12.6) - 1.
    report = two_fold_summary(risk_free_annual=0.02)
    assert report["risk_free_annual"] == 0.02
    assert report["sharpe"] == pytest.approx(2.622510538602889, abs=1e-9)
    assert report["sharpe_se"] == pytest.approx(2.0024223307373004, abs=1e-9)
    assert report["benchmark"]["risk_free_annual"] == 0.02
    assert report["benchmark"]["sharpe_se"] > 0.0


def test_both_thresholds_appear_and_passed_uses_the_new_formula():
    report = two_fold_summary(risk_free_annual=0.02, target_sharpe_floor=1.0)
    assert report["target_sharpe"] == 2.0
    assert report["target_sharpe_floor"] == 1.0
    assert report["active_tstat_threshold"] == 2.0
    expected_passed = (
        report["active_tstat"] >= report["active_tstat_threshold"]
        and report["sharpe"] - 1.64 * report["sharpe_se"] >= report["target_sharpe_floor"]
    )
    assert report["passed"] == expected_passed


def test_a_high_floor_fails_the_gate_even_with_a_strong_active_tstat():
    report = two_fold_summary(target_sharpe_floor=100.0)
    assert report["passed"] is False


def test_sharpe_standard_error_matches_the_closed_form():
    # SR_periodic = 0.5, N = 9: SE = sqrt((1 + 0.25/2) / 9) * sqrt(252).
    value = sharpe_standard_error(0.05, 0.1, 9, 252.0)
    expected = math.sqrt((1.0 + 0.5**2 / 2.0) / 9.0) * math.sqrt(252.0)
    assert value == pytest.approx(expected, abs=1e-9)


def test_active_tstat_helper_is_none_when_series_lengths_differ():
    import numpy as np

    assert active_return_tstat(np.array([0.01, 0.02]), np.array([0.01])) is None
