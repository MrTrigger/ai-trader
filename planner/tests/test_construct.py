"""Portfolio constructors (step 5, design spec §4.5).

Constructors are swappable, but the property that matters is that they are
*comparable* — the harness scores them against each other on the same signals,
so the choice is settled by out-of-sample evidence rather than by argument. Any
constructor that cannot beat `equal_weight` is deleted, however elegant.

The shared discipline every one of them is held to here: **a capped position's
leftover is never redistributed.** Spending it on the remaining names converts a
diversification constraint into a concentration one, which is the opposite of
what the cap was for.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from planner import construct
from planner.config import Config, CostModel, RiskLimits
from planner.construct import Signal

ALL = ("equal_weight", "conviction_tilt", "inverse_vol")


def config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=86_400,
        universe=[],
        target_gross_exposure=Decimal("0.90"),
        constructor="equal_weight",
        min_dollar_volume=Decimal(1_000_000),
        min_history_bars=60,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("1.0"),
        limits=RiskLimits(
            max_gross_exposure=Decimal("1.0"),
            max_position=Decimal("0.50"),
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


def signals(spec: dict[str, tuple[str, str | None]]) -> list[Signal]:
    """`{asset: (conviction, volatility|None)}`."""
    return [
        Signal(
            asset=asset,
            direction="long",
            conviction=Decimal(conviction),
            volatility=None if vol is None else Decimal(vol),
        )
        for asset, (conviction, vol) in spec.items()
    ]


THREE = signals({"A": ("90", "0.4"), "B": ("60", "0.8"), "C": ("30", "1.2")})


def gross(construction) -> Decimal:
    return sum((abs(w) for w in construction.weights.values()), Decimal(0))


# --- shared discipline -----------------------------------------------------


@pytest.mark.parametrize("name", ALL)
def test_no_signals_is_a_flat_target_not_an_error(name):
    built = construct.get(name).construct([], config=config())
    assert built.weights == {}
    assert any("flat" in n for n in built.notes)


@pytest.mark.parametrize("name", ALL)
def test_the_book_reaches_the_gross_target_when_nothing_binds(name):
    built = construct.get(name).construct(THREE, config=config())
    assert gross(built) == pytest.approx(Decimal("0.90"), abs=Decimal("0.0001"))


@pytest.mark.parametrize("name", ALL)
def test_a_capped_positions_leftover_is_not_redistributed(name):
    # Cap at 0.20 with a 0.90 target: three names can reach at most 0.60, and
    # the book must be left under-invested rather than concentrated.
    built = construct.get(name).construct(
        THREE, config=config(limits=config().limits.__class__(
            max_gross_exposure=Decimal("1.0"),
            max_position=Decimal("0.20"),
            max_position_count=20,
            min_position_notional=Decimal(25),
        ))
    )
    assert all(abs(w) <= Decimal("0.20") for w in built.weights.values())
    assert gross(built) <= Decimal("0.60")
    assert any("under-invested" in n for n in built.notes)


@pytest.mark.parametrize("name", ALL)
def test_the_constructor_records_itself_and_no_fallback_fired(name):
    built = construct.get(name).construct(THREE, config=config())
    assert built.constructor == name
    assert not built.fell_back


@pytest.mark.parametrize("name", ALL)
def test_a_short_signal_produces_a_negative_weight(name):
    built = construct.get(name).construct(
        [Signal(asset="A", direction="short", conviction=Decimal(50), volatility=Decimal("0.5"))],
        config=config(),
    )
    assert built.weights["A"] < 0


# --- equal_weight, the baseline --------------------------------------------


def test_equal_weight_ignores_conviction_entirely():
    built = construct.get("equal_weight").construct(THREE, config=config())
    assert len(set(built.weights.values())) == 1


# --- conviction_tilt -------------------------------------------------------


def test_conviction_tilt_leans_toward_the_stronger_signal():
    built = construct.get("conviction_tilt").construct(THREE, config=config())
    assert built.weights["A"] > built.weights["B"] > built.weights["C"]


def test_conviction_tilt_is_proportional_to_conviction():
    built = construct.get("conviction_tilt").construct(THREE, config=config())
    # 90 : 60 : 30 -> 3 : 2 : 1
    assert built.weights["A"] / built.weights["C"] == pytest.approx(Decimal(3), abs=1e-9)


def test_conviction_tilt_is_flat_when_every_conviction_is_zero():
    # Falling back to equal weight would invent an opinion the signal declined
    # to have.
    flat = signals({"A": ("0", "0.4"), "B": ("0", "0.8")})
    built = construct.get("conviction_tilt").construct(flat, config=config())
    assert built.weights == {}
    assert any("sum to zero" in n for n in built.notes)


# --- inverse_vol -----------------------------------------------------------


def test_inverse_vol_gives_the_calmest_asset_the_largest_weight():
    built = construct.get("inverse_vol").construct(THREE, config=config())
    assert built.weights["A"] > built.weights["B"] > built.weights["C"]


def test_inverse_vol_equalises_risk_contribution_not_capital():
    built = construct.get("inverse_vol").construct(THREE, config=config())
    contributions = [built.weights[a] * s.volatility for a, s in
                     zip(["A", "B", "C"], THREE)]
    assert contributions[0] == pytest.approx(contributions[1], abs=Decimal("0.0001"))
    assert contributions[1] == pytest.approx(contributions[2], abs=Decimal("0.0001"))


def test_inverse_vol_ignores_conviction():
    # It sizes on risk, not on rank. Whether that beats conviction_tilt is what
    # the harness is for.
    a = construct.get("inverse_vol").construct(THREE, config=config())
    reranked = signals({"A": ("10", "0.4"), "B": ("50", "0.8"), "C": ("99", "1.2")})
    b = construct.get("inverse_vol").construct(reranked, config=config())
    assert a.weights == b.weights


def test_inverse_vol_drops_an_asset_with_no_volatility_estimate():
    """Substituting an assumed number would size a position on a guess, and the
    whole reason to size on risk is that the risk number is real."""
    mixed = signals({"A": ("90", "0.4"), "B": ("60", None)})
    built = construct.get("inverse_vol").construct(mixed, config=config())
    assert "B" not in built.weights
    assert any("no volatility estimate" in n and "B" in n for n in built.notes)


def test_inverse_vol_drops_a_zero_volatility_asset():
    # A zero-vol asset would take the entire book on a division by zero.
    mixed = signals({"A": ("90", "0.4"), "B": ("60", "0")})
    built = construct.get("inverse_vol").construct(mixed, config=config())
    assert "B" not in built.weights
    # A alone would want the whole 0.90 target, and max_position caps it at
    # 0.50 — dropping an asset must not become a route to concentration.
    assert built.weights["A"] == Decimal("0.50")
    assert any("under-invested" in n for n in built.notes)


def test_inverse_vol_with_nothing_sizeable_is_flat():
    built = construct.get("inverse_vol").construct(
        signals({"A": ("90", None)}), config=config()
    )
    assert built.weights == {}


# --- the registry ----------------------------------------------------------


def test_every_constructor_is_reachable_by_name():
    for name in ALL:
        assert construct.get(name).name == name


def test_an_unknown_constructor_is_refused():
    with pytest.raises(ValueError, match="unknown constructor"):
        construct.get("magic")
