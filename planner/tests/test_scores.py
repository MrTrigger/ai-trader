"""Cross-sectional scoring (design spec §5.2, §9.1).

The framework's job is to turn per-asset measurements into a comparable
cross-section without ever letting "we could not measure this" look like "we
measured this and it was average". Most of what follows is about that second
half, because it is the half that quietly corrupts a backtest.
"""

from __future__ import annotations

from decimal import Decimal

import polars as pl
import pytest

from planner import scores
from planner.scores import Factor, SubFactor

MOMENTUM = Factor(
    name="momentum",
    sub_factors=(SubFactor("ret_30"), SubFactor("ret_90")),
    weight=Decimal(2),
)
LOW_VOL = Factor(
    name="low_vol",
    sub_factors=(SubFactor("vol_30", higher_is_better=False),),
    weight=Decimal(1),
)


def cross(**columns) -> pl.DataFrame:
    """One row per asset, keyed by the `asset` column."""
    return pl.DataFrame({"asset": list(columns.pop("assets")), **columns})


def five(**columns) -> pl.DataFrame:
    return cross(assets=["A", "B", "C", "D", "E"], **columns)


def composite_of(result) -> dict[str, float]:
    return result.composite()


# --- ranking ---------------------------------------------------------------


def test_percentile_rank_orders_the_cross_section():
    result = scores.score(
        five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    pct = dict(zip(result.frame["asset"], result.frame["pct_ret_30"]))
    assert pct["A"] < pct["B"] < pct["C"] < pct["D"] < pct["E"]
    # Midpoint form: five assets land on 10/30/50/70/90, never on 0 or 100.
    assert pct["A"] == pytest.approx(10.0)
    assert pct["E"] == pytest.approx(90.0)


def test_the_best_asset_is_not_scored_as_the_most_extreme_possible():
    result = scores.score(
        five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    assert max(result.frame["pct_ret_30"]) < 100.0


def test_direction_is_declared_on_the_sub_factor_not_by_negating_the_feature():
    # vol_30 means volatility here and everywhere; low_vol just reads it backwards.
    result = scores.score(
        five(vol_30=[0.9, 0.7, 0.5, 0.3, 0.1]),
        factors=(LOW_VOL,),
    )
    pct = dict(zip(result.frame["asset"], result.frame["pct_vol_30"]))
    assert pct["A"] < pct["E"], "the least volatile asset scores highest"


def test_ties_score_identically_and_do_not_depend_on_row_order():
    result = scores.score(
        five(ret_30=[0.3, 0.1, 0.3, 0.2, 0.3]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    pct = dict(zip(result.frame["asset"], result.frame["pct_ret_30"]))
    assert pct["A"] == pct["C"] == pct["E"]


# --- blending --------------------------------------------------------------


def test_sub_factors_are_equal_weighted_within_their_parent():
    result = scores.score(
        five(ret_30=[0.5, 0.4, 0.3, 0.2, 0.1], ret_90=[0.1, 0.2, 0.3, 0.4, 0.5]),
        factors=(MOMENTUM,),
    )
    # The two sub-factors are exact opposites, so every parent score is the mean.
    assert all(v == pytest.approx(50.0) for v in result.frame["factor_momentum"])


def test_the_composite_is_a_weighted_blend_of_parents():
    frame = five(
        ret_30=[0.1, 0.2, 0.3, 0.4, 0.5],
        ret_90=[0.1, 0.2, 0.3, 0.4, 0.5],
        vol_30=[0.1, 0.2, 0.3, 0.4, 0.5],
    )
    result = scores.score(frame, factors=(MOMENTUM, LOW_VOL))

    row = result.frame.filter(pl.col("asset") == "E")
    momentum, low_vol = row["factor_momentum"][0], row["factor_low_vol"][0]
    assert momentum == pytest.approx(90.0)
    assert low_vol == pytest.approx(10.0), "E is both the strongest and the most volatile"
    # Weights 2 and 1 -> two thirds momentum.
    assert row["composite"][0] == pytest.approx((2 * momentum + 1 * low_vol) / 3)


def test_composite_weights_are_normalised_so_absolute_values_do_not_matter():
    frame = five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5], ret_90=[0.1, 0.2, 0.3, 0.4, 0.5],
                 vol_30=[0.5, 0.4, 0.3, 0.2, 0.1])
    doubled = scores.score(
        frame,
        factors=(
            Factor("momentum", MOMENTUM.sub_factors, Decimal(4)),
            Factor("low_vol", LOW_VOL.sub_factors, Decimal(2)),
        ),
    )
    plain = scores.score(frame, factors=(MOMENTUM, LOW_VOL))
    assert composite_of(doubled) == pytest.approx(composite_of(plain))


def test_a_zero_weight_factor_is_computed_but_does_not_move_the_composite():
    frame = five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5], ret_90=[0.1, 0.2, 0.3, 0.4, 0.5],
                 vol_30=[0.5, 0.4, 0.3, 0.2, 0.1])
    with_zero = scores.score(
        frame, factors=(MOMENTUM, Factor("low_vol", LOW_VOL.sub_factors, Decimal(0)))
    )
    momentum_only = scores.score(frame, factors=(MOMENTUM,))
    assert composite_of(with_zero) == pytest.approx(composite_of(momentum_only))
    assert "factor_low_vol" in with_zero.frame.columns, "still reported, just not weighted"


# --- degeneracy: the honesty rules -----------------------------------------


def test_a_missing_measurement_scores_neutral_and_says_so():
    result = scores.score(
        five(ret_30=[0.1, 0.2, None, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    pct = dict(zip(result.frame["asset"], result.frame["pct_ret_30"]))
    assert pct["C"] == scores.NEUTRAL
    assert result.flags_for("C") == ["mom/ret_30:no_measurement"]
    assert any("C" in d and "ret_30" in d for d in result.disclosures)


def test_an_asset_with_a_measurement_carries_no_flag():
    result = scores.score(
        five(ret_30=[0.1, 0.2, None, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    assert result.flags_for("A") == []


def test_a_missing_measurement_does_not_shift_the_other_ranks():
    # The neutral asset must not be ranked as though it measured zero, which
    # would push every real measurement up the scale.
    with_null = scores.score(
        five(ret_30=[0.1, 0.2, None, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    pct = dict(zip(with_null.frame["asset"], with_null.frame["pct_ret_30"]))
    # Four ranked assets -> 12.5 / 37.5 / 62.5 / 87.5, and C sits out at 50.
    assert pct["A"] == pytest.approx(12.5)
    assert pct["E"] == pytest.approx(87.5)


def test_a_group_too_small_to_rank_within_scores_neutral_and_says_so():
    # Three assets in three sectors is three rankings of one, and a ranking of
    # one is a foregone conclusion rather than a measurement.
    result = scores.score(
        cross(assets=["A", "B", "C"], ret_30=[0.1, 0.2, 0.3]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
        groups={"A": "l1", "B": "l2", "C": "defi"},
    )
    assert all(v == scores.NEUTRAL for v in result.frame["pct_ret_30"])
    assert len(result.disclosures) == 3
    assert all("scored neutral" in d for d in result.disclosures)


def test_a_group_at_the_threshold_is_ranked_normally():
    result = scores.score(
        five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
        groups={a: "one" for a in "ABCDE"},
        min_group_size=5,
    )
    assert len(set(result.frame["pct_ret_30"])) == 5
    assert not result.disclosures


def test_ranking_is_within_the_group_not_across_the_universe():
    # B is the worst of its own group despite beating everything in the other.
    result = scores.score(
        cross(
            assets=["A", "B", "C", "D", "E", "F"],
            ret_30=[0.9, 0.8, 0.7, 0.3, 0.2, 0.1],
        ),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
        groups={"A": "big", "B": "big", "C": "big", "D": "small", "E": "small", "F": "small"},
        min_group_size=3,
    )
    pct = dict(zip(result.frame["asset"], result.frame["pct_ret_30"]))
    assert pct["C"] == pct["F"], "worst in group scores the same in either group"
    assert pct["A"] == pct["D"]


def test_ungrouped_assets_are_all_one_cross_section():
    result = scores.score(
        five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    assert set(result.frame["group_key"]) == {scores.UNGROUPED}


def test_both_kinds_of_degeneracy_are_distinguishable_in_the_flags():
    result = scores.score(
        cross(assets=["A", "B"], ret_30=[0.1, None]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    assert result.flags_for("A") == ["mom/ret_30:small_group"]
    assert sorted(result.flags_for("B")) == [
        "mom/ret_30:no_measurement",
        "mom/ret_30:small_group",
    ]


# --- provenance and refusals -----------------------------------------------


def test_the_scoring_version_travels_with_the_scores():
    result = scores.score(
        five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
        factors=(Factor("mom", (SubFactor("ret_30"),)),),
    )
    assert result.scoring_version == scores.SCORING_VERSION
    assert set(result.frame["scoring_version"]) == {scores.SCORING_VERSION}


def test_a_factor_wanting_a_column_the_frame_lacks_is_refused():
    with pytest.raises(ValueError, match="does not have"):
        scores.score(
            five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
            factors=(Factor("mom", (SubFactor("nonexistent"),)),),
        )


def test_a_factor_with_no_sub_factors_is_refused():
    with pytest.raises(ValueError, match="no sub-factors"):
        Factor("empty", ())


def test_weights_summing_to_zero_are_refused():
    with pytest.raises(ValueError, match="sum to zero"):
        scores.score(
            five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5]),
            factors=(Factor("mom", (SubFactor("ret_30"),), Decimal(0)),),
        )


def test_scoring_nothing_is_refused():
    with pytest.raises(ValueError, match="at least one factor"):
        scores.score(five(ret_30=[1, 2, 3, 4, 5]), factors=())


def test_an_empty_cross_section_scores_to_nothing_rather_than_raising():
    result = scores.score(pl.DataFrame({"asset": []}), factors=(MOMENTUM,))
    assert result.frame.is_empty()
    assert result.disclosures == ["no assets to score"]


# --- determinism -----------------------------------------------------------


def test_scoring_the_same_cross_section_twice_gives_the_same_answer():
    frame = five(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5], ret_90=[0.5, 0.1, 0.3, 0.2, 0.4],
                 vol_30=[0.2, 0.9, 0.4, 0.1, 0.7])
    a = scores.score(frame, factors=(MOMENTUM, LOW_VOL))
    b = scores.score(frame, factors=(MOMENTUM, LOW_VOL))
    assert a.frame.equals(b.frame)


def test_row_order_does_not_change_any_score():
    columns = dict(ret_30=[0.1, 0.2, 0.3, 0.4, 0.5], vol_30=[0.5, 0.4, 0.3, 0.2, 0.1])
    forward = scores.score(five(**columns), factors=(LOW_VOL,))
    shuffled = scores.score(
        five(**columns).sort("asset", descending=True), factors=(LOW_VOL,)
    )
    assert composite_of(forward) == pytest.approx(composite_of(shuffled))
