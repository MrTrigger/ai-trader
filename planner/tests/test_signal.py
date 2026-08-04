"""Signals (step 3).

A signal says what it thinks, never what to do. The tests below are mostly about
the boundary that keeps it that way, and about the two places `xs_momentum` is
allowed to decline: too few rankable assets, and too little history to rank on.

Declining matters more than it sounds. "Top 10 of 12" is a ranking; "top 10 of
11" is holding almost everything and calling it a signal, and the difference
between those two is where a strategy quietly becomes a long-only index with
extra turnover.
"""

from __future__ import annotations

from decimal import Decimal

import polars as pl
import pytest

from planner import signal
from planner.config import Config, CostModel, RiskLimits
from planner.signal import MOMENTUM_COLUMN


def config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=86_400,
        universe=[],
        target_gross_exposure=Decimal("0.75"),
        constructor="equal_weight",
        min_dollar_volume=Decimal(1_000_000),
        min_history_bars=60,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("1.0"),
        max_holdings=3,
        min_cross_section=5,
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


def cross(momentum: dict[str, float | None], **over) -> pl.DataFrame:
    """A point-in-time cross-section with everything eligibility needs."""
    assets = list(momentum)
    return pl.DataFrame(
        {
            "asset": assets,
            MOMENTUM_COLUMN: [momentum[a] for a in assets],
            "bars_available": over.get("bars_available", [200] * len(assets)),
            "adv_quote": over.get("adv_quote", [5e8] * len(assets)),
            "vol_30": over.get("vol_30", [0.5] * len(assets)),
        }
    )


def six(**over) -> pl.DataFrame:
    return cross(
        {"AAA": 0.10, "BBB": 0.50, "CCC": 0.30, "DDD": -0.10, "EEE": 0.40, "FFF": 0.20},
        **over,
    )


def held(result) -> list[str]:
    return [s.asset for s in result.signals]


# --- cross-sectional momentum ----------------------------------------------


def test_it_holds_the_strongest_names():
    result = signal.get("xs_momentum").generate(six(), config=config())
    assert sorted(held(result)) == ["BBB", "CCC", "EEE"]


def test_it_holds_exactly_max_holdings():
    result = signal.get("xs_momentum").generate(six(), config=config(max_holdings=2))
    assert len(result.signals) == 2
    assert sorted(held(result)) == ["BBB", "EEE"]


def test_everything_it_declined_is_noted_with_its_score():
    result = signal.get("xs_momentum").generate(six(), config=config())
    assert any("AAA" in n and "outside the top 3" in n for n in result.notes)


def test_conviction_is_the_score_that_drove_the_selection():
    result = signal.get("xs_momentum").generate(six(), config=config())
    by_asset = {s.asset: s.conviction for s in result.signals}
    # BBB has the strongest momentum, so it must not rank below its peers.
    assert by_asset["BBB"] >= by_asset["CCC"]
    assert all(c > 0 for c in by_asset.values())


def test_every_signal_is_long():
    # Long-only is not a preference: shorting spot needs margin, and §9.2 puts
    # leverage above 1x out of scope before Phase 3.
    result = signal.get("xs_momentum").generate(six(), config=config())
    assert {s.direction for s in result.signals} == {"long"}


def test_volatility_is_carried_so_a_constructor_can_size_on_risk():
    result = signal.get("xs_momentum").generate(six(), config=config())
    assert all(s.volatility is not None for s in result.signals)


def test_a_negative_momentum_name_can_still_be_held_if_it_is_top_ranked():
    # Cross-sectional, not absolute: the claim is about relative strength.
    # Whether that is the right claim is what the gate decides, but the signal
    # should not quietly become a trend filter.
    result = signal.get("xs_momentum").generate(
        cross({"A": -0.5, "B": -0.4, "C": -0.3, "D": -0.2, "E": -0.1, "F": -0.05}),
        config=config(max_holdings=2),
    )
    assert sorted(held(result)) == ["E", "F"]


# --- where it declines -----------------------------------------------------


def test_a_cross_section_too_small_to_rank_produces_no_position():
    """Holding the 'top 3 of 4' is holding four assets and calling it a signal."""
    result = signal.get("xs_momentum").generate(
        cross({"A": 0.5, "B": 0.4, "C": 0.3, "D": 0.2}), config=config(min_cross_section=5)
    )
    assert result.signals == []
    assert any("too small to rank" in n for n in result.notes)
    assert any(w.kind == "degenerate_feature" for w in result.warnings)


def test_an_asset_without_enough_history_is_excluded_and_said_so():
    frame = six(bars_available=[200, 200, 10, 200, 200, 200])
    result = signal.get("xs_momentum").generate(frame, config=config())
    assert "CCC" not in held(result)
    assert any("10 bars, needs 60" in n for n in result.notes)


def test_an_illiquid_asset_is_excluded_and_said_so():
    frame = six(adv_quote=[5e8, 100.0, 5e8, 5e8, 5e8, 5e8])
    result = signal.get("xs_momentum").generate(frame, config=config())
    assert "BBB" not in held(result), "the strongest name, but untradeable at size"
    assert any("below" in n for n in result.notes)


def test_an_asset_with_no_liquidity_estimate_is_excluded():
    frame = six(adv_quote=[5e8, None, 5e8, 5e8, 5e8, 5e8])
    result = signal.get("xs_momentum").generate(frame, config=config())
    assert "BBB" not in held(result)
    assert any("no liquidity estimate" in n for n in result.notes)


def test_a_missing_momentum_measurement_does_not_win_a_slot():
    frame = cross({"A": None, "B": 0.5, "C": 0.4, "D": 0.3, "E": 0.2, "F": 0.1})
    result = signal.get("xs_momentum").generate(frame, config=config(max_holdings=2))
    assert "A" not in held(result), "a null must not outrank a measurement"
    assert any(w.kind == "degenerate_feature" for w in result.warnings)


def test_nothing_eligible_is_a_flat_target_not_an_error():
    frame = six(bars_available=[1] * 6)
    result = signal.get("xs_momentum").generate(frame, config=config())
    assert result.signals == []


def test_a_feature_frame_without_the_momentum_column_is_refused():
    frame = pl.DataFrame(
        {"asset": ["A"], "bars_available": [200], "adv_quote": [5e8], "vol_30": [0.5]}
    )
    with pytest.raises(ValueError, match=MOMENTUM_COLUMN):
        signal.get("xs_momentum").generate(frame, config=config())


# --- determinism -----------------------------------------------------------


def test_the_same_cross_section_gives_the_same_signals():
    a = signal.get("xs_momentum").generate(six(), config=config())
    b = signal.get("xs_momentum").generate(six(), config=config())
    assert [(s.asset, s.conviction) for s in a.signals] == [
        (s.asset, s.conviction) for s in b.signals
    ]


def test_a_tie_is_broken_by_asset_name_not_by_row_order():
    tied = cross({"BBB": 0.5, "AAA": 0.5, "CCC": 0.1, "DDD": 0.1, "EEE": 0.1, "FFF": 0.1})
    result = signal.get("xs_momentum").generate(tied, config=config(max_holdings=2))
    assert sorted(held(result)) == ["AAA", "BBB"]


# --- the placeholder, kept as the null hypothesis ---------------------------


def test_the_placeholder_holds_everything_eligible():
    result = signal.get("placeholder_equal_long").generate(six(), config=config())
    assert len(result.signals) == 6
    assert {s.conviction for s in result.signals} == {Decimal(1)}


def test_the_placeholder_still_claims_no_edge():
    result = signal.get("placeholder_equal_long").generate(six(), config=config())
    assert any("claims no edge" in w.message for w in result.warnings)


def test_an_unknown_signal_is_refused():
    with pytest.raises(ValueError, match="unknown signal"):
        signal.get("wishful_thinking")


# --- the baseline a ranking has to beat ------------------------------------


def test_the_liquidity_baseline_holds_the_most_liquid_names():
    frame = six(adv_quote=[1e8, 9e8, 3e8, 8e8, 2e8, 7e8])
    result = signal.get("liquidity_top").generate(frame, config=config(max_holdings=2))
    assert sorted(held(result)) == ["BBB", "DDD"]


def test_the_baseline_holds_the_same_number_of_names_as_the_candidate():
    """Otherwise the comparison confounds *which* names with *how many*.

    And against a max_position_count limit, a baseline holding everything is
    not merely incomparable — it is illegal, every plan is rejected, and the
    baseline silently becomes a flat book that any strategy beats.
    """
    cfg = config(max_holdings=3)
    baseline = signal.get("liquidity_top").generate(six(), config=cfg)
    candidate = signal.get("xs_momentum").generate(six(), config=cfg)
    assert len(baseline.signals) == len(candidate.signals) == 3


def test_the_baseline_ignores_momentum_entirely():
    strong = cross({"A": 0.9, "B": 0.1, "C": 0.1, "D": 0.1, "E": 0.1, "F": 0.1})
    weak = cross({"A": 0.1, "B": 0.9, "C": 0.1, "D": 0.1, "E": 0.1, "F": 0.1})
    a = signal.get("liquidity_top").generate(strong, config=config())
    b = signal.get("liquidity_top").generate(weak, config=config())
    assert held(a) == held(b)


def test_the_baseline_says_it_claims_no_edge():
    result = signal.get("liquidity_top").generate(six(), config=config())
    assert any("exists to be beaten" in w.message for w in result.warnings)


def test_the_baseline_applies_the_same_eligibility_screens():
    frame = six(adv_quote=[5e8, 100.0, 5e8, 5e8, 5e8, 5e8])
    result = signal.get("liquidity_top").generate(frame, config=config())
    assert "BBB" not in held(result)
