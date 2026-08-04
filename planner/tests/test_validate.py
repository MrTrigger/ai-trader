"""Validation: holdout, walk-forward, plateaus (design spec §2.2, §9).

These are the instruments the Phase 1 gate is read on, so a bug here does not
produce a wrong number — it produces a *confident* wrong number, which is worse.
The tests are correspondingly about the shape of the answer rather than its
value: that splits run forward in time, that a plateau of one is called a peak,
that an inadequate sample says so.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest

from planner import backtest, validate

UTC = timezone.utc
DAY = 86_400
START = datetime(2026, 1, 1, tzinfo=UTC)


def step(index: int, nav: str, *, status: str = "accepted") -> backtest.Step:
    return backtest.Step(
        as_of=START + timedelta(days=index),
        nav=Decimal(nav),
        cash=Decimal(0),
        gross_exposure=Decimal("0.75"),
        status=status,
        fills=[],
        plan_id=f"plan-{index}",
        warnings=[],
    )


def result(navs: list[str]) -> backtest.Result:
    steps = [step(i, n) for i, n in enumerate(navs)]
    return backtest.Result(steps=steps, metrics=backtest.metrics(steps, interval_s=DAY))


def rising(n: int = 40) -> backtest.Result:
    return result([str(100_000 + i * 500) for i in range(n)])


def falling(n: int = 40) -> backtest.Result:
    return result([str(100_000 - i * 500) for i in range(n)])


# --- holdout ---------------------------------------------------------------


def test_holdout_splits_forward_in_time():
    """Never shuffled: a shuffled split leaks the test window's neighbours into
    training through overlapping feature windows."""
    h = validate.holdout(rising(40), interval_s=DAY)
    assert h.train.dates[-1] < h.test.dates[0]
    assert len(h.train.dates) == 28
    assert len(h.test.dates) == 12


def test_holdout_respects_the_train_fraction():
    h = validate.holdout(rising(40), interval_s=DAY, train_fraction=0.5)
    assert len(h.train.dates) == 20
    assert len(h.test.dates) == 20


def test_a_consistently_rising_book_holds_up_out_of_sample():
    assert validate.holdout(rising(), interval_s=DAY).consistent


def test_a_falling_book_does_not_hold_up():
    assert not validate.holdout(falling(), interval_s=DAY).consistent


def test_a_book_that_only_rises_in_sample_does_not_hold_up():
    # The case the split exists to catch: great until the moment it is asked to
    # perform on data it was not chosen on.
    navs = [str(100_000 + i * 500) for i in range(28)] + [
        str(114_000 - i * 800) for i in range(12)
    ]
    assert not validate.holdout(result(navs), interval_s=DAY).consistent


def test_an_invalid_train_fraction_is_refused():
    with pytest.raises(ValueError, match="must be in"):
        validate.holdout(rising(), interval_s=DAY, train_fraction=1.0)


# --- walk-forward ----------------------------------------------------------


def test_walk_forward_produces_the_requested_folds():
    wf = validate.walk_forward(rising(80), interval_s=DAY, folds=4)
    assert len(wf.folds) == 4


def test_every_fold_tests_on_dates_later_than_it_trained_on():
    wf = validate.walk_forward(rising(80), interval_s=DAY, folds=4)
    for fold in wf.folds:
        assert fold.train.dates[-1] < fold.test.dates[0]


def test_fold_test_windows_do_not_overlap():
    # Overlapping test windows would count the same observation more than once
    # and make `positive_folds` read as more evidence than it is.
    wf = validate.walk_forward(rising(80), interval_s=DAY, folds=4)
    seen: set = set()
    for fold in wf.folds:
        assert not seen & set(fold.test.dates)
        seen |= set(fold.test.dates)


def test_a_rising_book_is_consistent_across_folds():
    wf = validate.walk_forward(rising(80), interval_s=DAY, folds=4)
    assert wf.consistent
    assert wf.positive_folds == 4


def test_a_falling_book_is_not_consistent():
    wf = validate.walk_forward(falling(80), interval_s=DAY, folds=4)
    assert not wf.consistent
    assert wf.positive_folds == 0


def test_too_few_steps_produces_no_folds_rather_than_fake_ones():
    wf = validate.walk_forward(rising(4), interval_s=DAY, folds=4)
    assert wf.folds == ()
    assert not wf.consistent


def test_zero_folds_is_refused():
    with pytest.raises(ValueError, match="must be positive"):
        validate.walk_forward(rising(), interval_s=DAY, folds=0)


# --- plateaus --------------------------------------------------------------


def points(flags: list[bool], values: list[object] | None = None):
    values = values or list(range(len(flags)))
    empty = backtest.metrics([], interval_s=DAY)
    return [validate.SweepPoint(v, empty, f) for v, f in zip(values, flags)]


def test_the_widest_contiguous_run_wins():
    p = validate.find_plateau(
        points([True, False, True, True, True, False, True]), axis="x"
    )
    assert p.values == (2, 3, 4)
    assert p.width == 3


def test_the_centre_is_reported_not_the_edge():
    p = validate.find_plateau(points([False, True, True, True, True, True]), axis="x")
    assert p.centre == 3, "centre of 1..5"


def test_a_run_of_one_is_named_a_peak():
    """A setting that works at exactly one value and fails on either side of it
    is an artefact of this particular history."""
    p = validate.find_plateau(points([False, True, False]), axis="lookback")
    assert p.is_a_peak
    assert "PEAK" in str(p)


def test_a_wide_run_is_not_a_peak():
    p = validate.find_plateau(points([True, True, True]), axis="x")
    assert not p.is_a_peak
    assert "PEAK" not in str(p)


def test_nothing_holding_up_is_an_empty_plateau():
    p = validate.find_plateau(points([False, False, False]), axis="x")
    assert p.values == ()
    assert p.centre is None
    assert "nothing held up" in str(p)


def test_a_run_that_reaches_the_end_of_the_range_is_still_found():
    p = validate.find_plateau(points([False, True, True, True]), axis="x")
    assert p.values == (1, 2, 3)


def test_the_whole_range_holding_up_is_one_plateau():
    p = validate.find_plateau(points([True] * 5), axis="x")
    assert p.width == 5
    assert p.centre == 2


def test_a_plateau_carries_the_actual_swept_values():
    p = validate.find_plateau(
        points([False, True, True, True, False], values=[5, 10, 20, 30, 60]), axis="days"
    )
    assert p.values == (10, 20, 30)
    assert p.centre == 20


# --- the axes --------------------------------------------------------------


def test_each_axis_changes_exactly_one_thing():
    from planner.config import Config, CostModel, RiskLimits

    base = Config(
        quote_currency="USD",
        interval_s=DAY,
        universe=[],
        target_gross_exposure=Decimal("0.75"),
        constructor="equal_weight",
        min_dollar_volume=Decimal(1),
        min_history_bars=60,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("1.0"),
        limits=RiskLimits(
            max_gross_exposure=Decimal("1.0"),
            max_position=Decimal("0.30"),
            max_position_count=20,
            min_position_notional=Decimal(25),
        ),
        costs=CostModel(
            commission_bps=Decimal(5),
            spread_bps=Decimal(2),
            impact_coefficient=Decimal("0.10"),
            adv_lookback_days=20,
        ),
    )

    assert validate.with_holdings(base, 5).max_holdings == 5
    assert validate.with_turnover_budget(base, "0.25").turnover_budget == Decimal("0.25")
    assert validate.with_constructor(base, "inverse_vol").constructor == "inverse_vol"
    assert validate.with_rebalance_every(base, 7).rebalance_every == 7

    # And nothing else moved.
    tweaked = validate.with_holdings(base, 5)
    assert tweaked.constructor == base.constructor
    assert tweaked.turnover_budget == base.turnover_budget
    assert tweaked.interval_s == base.interval_s, (
        "rebalance frequency must not change the bar interval, or a sweep "
        "measures the features and the cadence at once"
    )
