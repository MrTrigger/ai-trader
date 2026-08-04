"""The information coefficient (design spec §7.5).

This is a measurement instrument, so a bug here does not produce a wrong answer
— it produces a *confident* wrong answer about whether a signal has content, and
everything downstream is decided on it.

The overlap correction gets the most attention because it is the one that
already bit: sampling a 30-day forward return every 7 days reuses each window
4.3×, and the uncorrected t-stat read −3.55 where the honest one reads −1.72.
One of those says "established", the other says "suggestive".
"""

from __future__ import annotations

import math
from datetime import datetime, timedelta, timezone

import pytest

from planner import ic

UTC = timezone.utc
START = datetime(2026, 1, 1, tzinfo=UTC)


def result(ics: list[float], *, horizon: int = 7, step: int = 7) -> ic.ICResult:
    return ic.ICResult(
        horizon_days=horizon,
        step_days=step,
        periods=[
            ic.PeriodIC(as_of=START + timedelta(days=i * step), n_assets=30, ic=v)
            for i, v in enumerate(ics)
        ],
    )


# --- rank correlation ------------------------------------------------------


def test_a_perfect_ordering_scores_one():
    assert ic.spearman([1, 2, 3, 4, 5], [10, 20, 30, 40, 50]) == pytest.approx(1.0)


def test_a_reversed_ordering_scores_minus_one():
    assert ic.spearman([1, 2, 3, 4, 5], [50, 40, 30, 20, 10]) == pytest.approx(-1.0)


def test_it_measures_order_not_magnitude():
    """A signal that ranks correctly and scales wrongly is still a signal."""
    linear = ic.spearman([1, 2, 3, 4], [1, 2, 3, 4])
    convex = ic.spearman([1, 2, 3, 4], [1, 100, 10_000, 1_000_000])
    assert linear == convex == pytest.approx(1.0)


def test_ties_are_averaged():
    # Two identical scores must not be ordered by their position in the list.
    assert ic.spearman([1, 1, 2], [5, 5, 9]) == pytest.approx(1.0)
    assert ic.spearman([1, 1, 2], [9, 5, 5]) == ic.spearman([1, 1, 2], [5, 9, 5])


def test_no_dispersion_on_either_side_is_not_a_correlation():
    assert ic.spearman([1, 1, 1], [1, 2, 3]) is None
    assert ic.spearman([1, 2, 3], [7, 7, 7]) is None


def test_a_single_point_is_not_a_correlation():
    assert ic.spearman([1.0], [2.0]) is None


# --- aggregation -----------------------------------------------------------


def test_mean_and_hit_rate():
    r = result([0.1, -0.1, 0.2, 0.0])
    assert r.mean_ic == pytest.approx(0.05)
    assert r.hit_rate == pytest.approx(0.5), "zero is not positive"
    assert r.n_periods == 4
    assert r.n_observations == 120


def test_the_t_stat_uses_periods_not_assets():
    """Assets inside one cross-section are heavily correlated.

    Counting each as independent would inflate the t-stat by ~sqrt(n_assets) —
    about 6x here — and turn noise into a result.
    """
    r = result([0.13, -0.07] * 50)
    naive_by_assets = r.mean_ic / (r.std_ic / math.sqrt(r.n_observations))
    assert abs(r.t_stat) < abs(naive_by_assets) / 5


def test_a_constant_ic_series_does_not_divide_by_zero():
    assert result([0.05] * 10).t_stat == 0.0


# --- the overlap correction ------------------------------------------------


def test_non_overlapping_windows_use_the_full_period_count():
    r = result([0.02] * 100, horizon=7, step=7)
    assert r.overlap == 1.0
    assert r.effective_n == 100


def test_overlapping_windows_deflate_the_effective_sample():
    r = result([0.02] * 100, horizon=30, step=7)
    assert r.overlap == pytest.approx(30 / 7)
    assert r.effective_n == pytest.approx(100 * 7 / 30)


def test_overlap_shrinks_the_t_stat():
    """The correction that already changed a verdict.

    Uncorrected, the 30d IC read t = -3.55 and would have been reported as
    established. Corrected, it reads -1.72.
    """
    ics = [0.02, -0.01, 0.03, 0.01, 0.02, -0.02] * 20
    clean = result(ics, horizon=7, step=7)
    overlapped = result(ics, horizon=30, step=7)
    assert abs(overlapped.t_stat) < abs(clean.t_stat)
    assert abs(overlapped.t_stat) == pytest.approx(
        abs(clean.t_stat) / math.sqrt(30 / 7), rel=1e-6
    )


def test_a_horizon_shorter_than_the_step_does_not_inflate():
    # Gaps between windows, not overlaps. Never treat that as extra evidence.
    r = result([0.02] * 50, horizon=3, step=7)
    assert r.overlap == 1.0
    assert r.effective_n == 50


def test_significance_is_read_off_the_corrected_statistic():
    """The exact shape of the mistake this correction prevents.

    Mean IC 0.03 with std 0.10 over 100 weekly periods reads t = 2.99 and looks
    established. Sampled at a 30-day horizon those windows overlap 4.3x, the
    effective sample is 23, and the honest t is 1.44 - which is not a finding.
    """
    ics = [0.13, -0.07] * 50
    clean = result(ics, horizon=7, step=7)
    overlapped = result(ics, horizon=30, step=7)

    assert clean.t_stat == pytest.approx(2.99, abs=0.05)
    assert clean.distinguishable_from_zero
    assert overlapped.t_stat == pytest.approx(1.44, abs=0.05)
    assert not overlapped.distinguishable_from_zero


# --- empty and degenerate --------------------------------------------------


def test_no_periods_reports_nothing_rather_than_zero_confidence():
    r = result([])
    assert r.n_periods == 0
    assert r.mean_ic == 0.0
    assert r.t_stat == 0.0
    assert not r.distinguishable_from_zero


def test_one_period_is_not_a_measurement():
    r = result([0.5])
    assert r.std_ic == 0.0
    assert r.t_stat == 0.0
    assert not r.distinguishable_from_zero


# --- reporting -------------------------------------------------------------


def test_disclosures_come_before_the_numbers():
    text = ic.format_results([result([0.01] * 10)], score_column="x")
    assert text.index("DISCLOSURES") < text.index("mean IC")


def test_the_report_says_a_positive_ic_is_not_a_profitable_strategy():
    text = ic.format_results([result([0.01] * 10)], score_column="x")
    assert "does not mean a strategy built on it makes money" in text


def test_the_report_shows_the_effective_sample_next_to_the_raw_one():
    text = ic.format_results([result([0.01] * 100, horizon=30, step=7)], score_column="x")
    assert "eff n" in text
    assert " 100" in text and " 23" in text, "raw 100 periods, ~23 effective"
