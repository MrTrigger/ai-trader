"""Diff, deadband and the turnover budget.

The properties here are the ones that quietly cost money or quietly do less than
they claimed: a rebalance that pays spread to chase noise, an exit suppressed by
a cost filter, entries sized against capital an exit had not yet freed, and a
budget that truncates without saying so.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from planner import diff
from planner.config import Config, CostModel, RiskLimits

NAV = Decimal(100_000)
PRICES = {a: Decimal(100) for a in ("AAA", "BBB", "CCC", "DDD")}
ADV: dict[str, Decimal | None] = {a: Decimal(50_000_000) for a in PRICES}
VOL: dict[str, Decimal | None] = {a: Decimal("0.05") for a in PRICES}


def _config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=86_400,
        universe=list(PRICES),
        target_gross_exposure=Decimal("0.8"),
        constructor="equal_weight",
        min_dollar_volume=Decimal(0),
        min_history_bars=0,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("10"),  # effectively unlimited unless overridden
        limits=RiskLimits(
            max_gross_exposure=Decimal("1.0"),
            max_position=Decimal("0.5"),
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


def _compute(targets: dict, current: dict, **over) -> diff.DiffResult:
    return diff.compute(
        target_weights={k: Decimal(str(v)) for k, v in targets.items()},
        current_weights={k: Decimal(str(v)) for k, v in current.items()},
        prices=PRICES,
        adv=ADV,
        vol=VOL,
        nav=NAV,
        config=_config(**over),
    )


# --- deadband --------------------------------------------------------------


def test_small_drift_is_left_alone():
    """Paying a certain spread to correct noise is a guaranteed loss.

    20bps of drift on a 100k book is $200 - comfortably above the $25 notional
    floor, so this exercises the cost deadband rather than the minimum-size
    filter. Round-trip cost here is ~14bps and the multiple is 3x, so the
    threshold is ~42bps.
    """
    r = _compute({"AAA": "0.202"}, {"AAA": "0.2"})
    assert r.trades == []
    assert any("deadband" in s for s in r.skipped), r.skipped


def test_large_drift_trades():
    r = _compute({"AAA": "0.30"}, {"AAA": "0.20"})
    assert [t.asset for t in r.trades] == ["AAA"]
    assert r.trades[0].reason == "increase"


def test_exit_is_never_suppressed_by_the_deadband():
    """Getting out is not measured against its spread."""
    r = _compute({}, {"AAA": "0.0002"})
    assert [t.reason for t in r.trades] == ["exit"]


def test_tiny_entry_below_minimum_notional_is_skipped():
    r = _compute({"AAA": "0.0001"}, {})
    assert r.trades == []
    assert any("minimum" in s for s in r.skipped)


# --- sizing and direction --------------------------------------------------


def test_quantities_and_sides():
    r = _compute({"AAA": "0.10"}, {})
    t = r.trades[0]
    assert t.delta_notional == Decimal(10_000)
    assert t.qty == Decimal(100)
    assert r.orders[0].side == "buy"


def test_reducing_a_position_sells():
    r = _compute({"AAA": "0.10"}, {"AAA": "0.30"})
    assert r.orders[0].side == "sell"
    assert r.trades[0].reason == "reduce"


# --- ordering --------------------------------------------------------------


def test_exits_precede_entries():
    """Frees capital before entries size against it."""
    r = _compute({"BBB": "0.30"}, {"AAA": "0.30"})
    reasons = [t.reason for t in r.trades]
    assert reasons.index("exit") < reasons.index("entry")


def test_reduce_precedes_increase():
    r = _compute({"AAA": "0.10", "BBB": "0.40"}, {"AAA": "0.30", "BBB": "0.20"})
    reasons = [t.reason for t in r.trades]
    assert reasons.index("reduce") < reasons.index("increase")


# --- turnover budget -------------------------------------------------------


def test_budget_takes_largest_drift_first():
    """The trade closing the most distance buys the most convergence per unit spent."""
    r = _compute(
        {"AAA": "0.10", "BBB": "0.30", "CCC": "0.20"}, {}, turnover_budget=Decimal("0.50")
    )
    assert [t.asset for t in r.trades] == ["BBB", "CCC"]
    assert r.turnover_used == Decimal("0.50")


def test_dropped_trades_are_disclosed():
    """No silent caps. A plan that quietly did less reads later as one that failed."""
    r = _compute({"AAA": "0.30", "BBB": "0.30"}, {}, turnover_budget=Decimal("0.30"))
    assert len(r.trades) == 1
    assert len(r.dropped) == 1
    assert r.turnover_dropped == Decimal("0.30")
    assert "dropped" in r.dropped[0]


def test_budget_never_blocks_a_legal_destination():
    """A budget paces the transition; it does not veto the target."""
    r = _compute({"AAA": "0.25", "BBB": "0.25", "CCC": "0.25"}, {}, turnover_budget=Decimal("0.25"))
    assert len(r.trades) == 1, "one leg this run, the rest next run"
    assert len(r.dropped) == 2


def test_initial_build_from_flat_is_reachable():
    """The case that broke the veto design: 0% -> 75% invested IS 75% turnover."""
    r = _compute(
        {"AAA": "0.25", "BBB": "0.25", "CCC": "0.25"}, {}, turnover_budget=Decimal("0.75")
    )
    assert len(r.trades) == 3
    assert r.dropped == []


def test_exits_are_exempt_from_the_budget():
    """Pacing out of unwanted risk is the same exposure held longer, not a saving."""
    r = _compute(
        {}, {"AAA": "0.30", "BBB": "0.30", "CCC": "0.30"}, turnover_budget=Decimal("0.10")
    )
    assert len(r.trades) == 3
    assert all(t.reason == "exit" for t in r.trades)
    assert any("exits are exempt" in d for d in r.dropped)


def test_exits_consume_budget_before_entries():
    r = _compute(
        {"CCC": "0.40"}, {"AAA": "0.30"}, turnover_budget=Decimal("0.30")
    )
    assert [t.reason for t in r.trades] == ["exit"]
    assert any("CCC" in d for d in r.dropped)


def test_no_trades_when_already_on_target():
    r = _compute({"AAA": "0.25"}, {"AAA": "0.25"})
    assert r.trades == []
    assert r.turnover_used == Decimal(0)


def test_missing_price_is_skipped_not_guessed():
    with pytest.raises(KeyError):
        PRICES["ZZZ"]  # sanity: not priced
    r = _compute({"ZZZ": "0.10"}, {})
    assert r.trades == []
    assert any("no price" in s for s in r.skipped)
