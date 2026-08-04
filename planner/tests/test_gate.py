"""The Phase 1 gate (design spec §9).

The gate's job is to be capable of saying no, so these tests are mostly about
its refusals: that a losing strategy fails, that one which dies under 2×
slippage fails, that a thin sample is reported as thin rather than passed over.

A gate that can be argued with is not a gate, so `GateResult.passed` is
all-of-four and there is deliberately no override.
"""

from __future__ import annotations

import math
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl
import pytest

from planner import backtest, gate, state, store, universe
from planner.config import Config, CostModel, RiskLimits

UTC = timezone.utc
DAY = 86_400
END = datetime(2026, 8, 1, tzinfo=UTC)
START = END - timedelta(days=120)
ASSETS = tuple(f"A{i:02d}" for i in range(20))
HISTORY = 400


def _bars(drift: float = 0.0008) -> pl.DataFrame:
    """Twenty assets with staggered, deterministic trends.

    Staggered so a cross-sectional ranking has something to rank: each asset
    leads at a different phase, which is what momentum is supposed to exploit.
    """
    rows = []
    for i, asset in enumerate(ASSETS):
        close = 100.0 * (i + 1)
        for d in range(HISTORY):
            ts = END - timedelta(seconds=DAY * (HISTORY - 1 - d))
            step = 0.02 * math.sin(i * 0.9 + d * 0.08) + drift
            nxt = close * math.exp(step)
            rows.append(
                {
                    "asset": asset,
                    "ts_utc": ts,
                    "interval_s": DAY,
                    "open": close,
                    "high": max(close, nxt),
                    "low": min(close, nxt),
                    "close": nxt,
                    "volume": 1e5,
                    "quote_volume": 5e8,
                    "trades": 10_000,
                }
            )
            close = nxt
    return pl.DataFrame(rows).with_columns(
        pl.col("ts_utc").dt.replace_time_zone("UTC"), pl.col("interval_s").cast(pl.Int32)
    )


def config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=DAY,
        universe=list(ASSETS),
        target_gross_exposure=Decimal("0.75"),
        constructor="conviction_tilt",
        signal="xs_momentum",
        max_holdings=5,
        min_cross_section=10,
        rebalance_every=7,
        min_dollar_volume=Decimal(1_000_000),
        min_volatility=Decimal("0.10"),
        min_history_bars=90,
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
    return Config(**(base | over))


@pytest.fixture
def env(tmp_path: Path) -> Path:
    store.write(_bars(), root=tmp_path, source="synthetic")
    day = START
    while day <= END:
        universe.record(
            universe.from_config(list(ASSETS)), as_of=day, source="test", root=tmp_path
        )
        day += timedelta(days=7)
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=START),
        tmp_path / "book.json",
    )
    return tmp_path


def run(env: Path, **over) -> gate.GateResult:
    return gate.run(
        config=config(**over),
        start=START,
        end=END,
        data_root=env,
        initial_cash=Decimal(100_000),
    )


def criterion(result: gate.GateResult, fragment: str) -> gate.Criterion:
    return next(c for c in result.criteria if fragment in c.name)


# --- shape -----------------------------------------------------------------


def test_the_gate_evaluates_all_four_criteria(env):
    result = run(env)
    names = [c.name for c in result.criteria]
    assert len(names) == 4
    assert any("expectancy" in n for n in names)
    assert any("2x slippage" in n for n in names)
    assert any("walk-forward" in n for n in names)
    assert any("sample" in n for n in names)


def test_passing_requires_all_four(env):
    result = run(env)
    assert result.passed == all(c.passed for c in result.criteria)


def test_the_candidate_is_compared_against_a_named_baseline(env):
    result = run(env)
    assert "xs_momentum" in result.candidate
    # The baseline holds the same NUMBER of names, so the comparison isolates
    # which names the ranking picked rather than how many it held.
    assert "liquidity_top" in result.baseline
    assert result.baseline_metrics is not None
    assert result.baseline_metrics.n > 0, "a baseline that never traded is not one"


def test_every_criterion_carries_the_numbers_behind_it(env):
    result = run(env)
    assert all(c.detail for c in result.criteria)
    assert "%" in criterion(result, "expectancy").detail


# --- refusals --------------------------------------------------------------


def test_a_losing_strategy_fails_the_expectancy_criterion(tmp_path):
    store.write(_bars(drift=-0.004), root=tmp_path, source="synthetic")
    day = START
    while day <= END:
        universe.record(
            universe.from_config(list(ASSETS)), as_of=day, source="test", root=tmp_path
        )
        day += timedelta(days=7)
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=START),
        tmp_path / "book.json",
    )

    result = run(tmp_path)
    assert not criterion(result, "expectancy").passed
    assert not result.passed


def test_a_thin_sample_fails_the_sample_criterion(env):
    result = gate.run(
        config=config(),
        start=END - timedelta(days=60),
        end=END,
        data_root=env,
        initial_cash=Decimal(100_000),
    )
    assert result.candidate_metrics.n < backtest.INSUFFICIENT_SAMPLE_N
    assert not criterion(result, "sample").passed
    assert any("insufficient sample" in d for d in result.disclosures)


def test_the_slippage_criterion_uses_the_stressed_run(env):
    result = run(env)
    assert result.stressed_metrics is not None
    assert result.stressed_metrics.total_return <= result.candidate_metrics.total_return


# --- honesty ---------------------------------------------------------------


def test_disclosures_come_before_the_verdict(env):
    text = gate.format_result(run(env))
    assert text.index("DISCLOSURES") < text.index("PHASE 1 GATE")


def test_the_report_names_what_the_gate_does_not_measure(env):
    # §7.5: portfolio P&L is ~30x less informative per unit of calendar time
    # than the information coefficient, and the gate does not compute the IC.
    text = gate.format_result(run(env))
    assert "information coefficient" in text


def test_a_failed_gate_says_that_is_the_system_working(tmp_path):
    store.write(_bars(drift=-0.004), root=tmp_path, source="synthetic")
    day = START
    while day <= END:
        universe.record(
            universe.from_config(list(ASSETS)), as_of=day, source="test", root=tmp_path
        )
        day += timedelta(days=7)
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=START),
        tmp_path / "book.json",
    )
    text = gate.format_result(run(tmp_path))
    assert "NOT PASSED" in text
    assert "not a softer gate" in text


def test_the_uncalibrated_cost_model_is_disclosed(env):
    assert any("uncalibrated" in d for d in run(env).disclosures)


# --- beating the baseline --------------------------------------------------


def test_beating_the_baseline_needs_both_consistency_and_a_better_result():
    from planner import validate

    empty = backtest.metrics([], interval_s=DAY)

    def walk(returns: list[str]) -> validate.WalkForward:
        folds = []
        for i, r in enumerate(returns):
            metrics = backtest.Metrics(
                n=10,
                total_return=Decimal(r),
                cagr=0.0,
                volatility=0.1,
                sharpe=0.0,
                max_drawdown=Decimal(0),
                turnover_per_rebalance=Decimal(0),
                cost_drag_bps=Decimal(0),
                rejected=0,
            )
            window = validate.Window(f"f{i}", (), metrics)
            folds.append(validate.Holdout(train=window, test=window))
        return validate.WalkForward(folds=tuple(folds))

    strong = walk(["0.10", "0.08", "0.05", "0.06"])
    weak = walk(["0.01", "0.01", "0.01", "0.01"])
    inconsistent = walk(["0.40", "-0.05", "-0.05", "-0.05"])

    assert gate._beats(strong, weak)
    assert not gate._beats(weak, strong), "consistent but worse is not beating it"
    assert not gate._beats(inconsistent, weak), "one fold carrying the result is not beating it"
