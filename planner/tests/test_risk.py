"""The risk gate (design spec §6).

The two limits added here are the ones §6 argues hardest for, and both exist to
catch the same failure: a book that looks diversified and is not. A cluster
limit stops twenty correlated names from adding up to one big position; a beta
limit stops a long alt book from being a leveraged BTC trade that the
attribution will later credit as alpha.

Both are only as good as what they can see, so a running theme below is that
what the gate *cannot* vouch for gets disclosed rather than absorbed.
"""

from __future__ import annotations

from decimal import Decimal

import pytest

from planner import risk
from planner.config import RiskLimits, clusters_from_dict

NAV = Decimal(100_000)


def limits(**overrides) -> RiskLimits:
    base = dict(
        max_gross_exposure=Decimal("1.00"),
        max_position=Decimal("0.30"),
        max_position_count=20,
        min_position_notional=Decimal(25),
    )
    base.update(overrides)
    return RiskLimits(**base)


def evaluate(weights, **kwargs):
    return risk.evaluate(
        target_weights={a: Decimal(w) for a, w in weights.items()},
        current_weights={},
        nav=NAV,
        **kwargs,
    )


def check(evaluation, name):
    return next(c for c in evaluation.report.checks if c.name == name)


# --- cluster exposure ------------------------------------------------------

CLUSTERS = {"BTC": "btc", "ETH": "eth", "SOL": "alt_l1", "AVAX": "alt_l1", "NEAR": "alt_l1"}


def test_a_cluster_within_its_limit_passes():
    ev = evaluate(
        {"BTC": "0.25", "SOL": "0.20", "AVAX": "0.15"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    c = check(ev, "max_cluster_exposure")
    assert c.value == Decimal("0.35"), "SOL + AVAX are one group"
    assert c.passed
    assert ev.passed


def test_correlated_names_are_summed_and_can_reject_the_plan():
    # Individually each is well under max_position. Together they are one bet,
    # which is the entire point of the limit.
    ev = evaluate(
        {"SOL": "0.20", "AVAX": "0.20", "NEAR": "0.15", "BTC": "0.10"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    c = check(ev, "max_cluster_exposure")
    assert c.value == Decimal("0.55")
    assert not c.passed
    assert not ev.passed
    assert "max_cluster_exposure" in ev.report.rejected_reason


def test_shorts_count_toward_cluster_gross():
    # Gross, not net: a long and a short in the same correlated group are two
    # positions to be wrong about, not zero.
    ev = evaluate(
        {"SOL": "0.30", "AVAX": "-0.30"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    assert check(ev, "max_cluster_exposure").value == Decimal("0.60")


def test_an_unclassified_asset_is_disclosed_by_name():
    ev = evaluate(
        {"BTC": "0.20", "WIF": "0.30"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    assert check(ev, "max_cluster_exposure").passed
    assert any("WIF" in d for d in ev.disclosures), (
        "an asset the grouping does not name escapes this limit, and a limit "
        "that quietly stopped applying is worse than one declared off"
    )


def test_unclassified_assets_are_not_lumped_into_one_fictitious_cluster():
    # Two unrelated unclassified names must not fail the check for being
    # unrelated to each other.
    ev = evaluate(
        {"WIF": "0.30", "PEPE": "0.30"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    assert check(ev, "max_cluster_exposure").value == Decimal("0.30")
    assert check(ev, "max_cluster_exposure").passed


def test_zero_weights_do_not_create_a_cluster():
    ev = evaluate(
        {"SOL": "0.30", "AVAX": "0"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    assert check(ev, "max_cluster_exposure").value == Decimal("0.30")
    assert not ev.disclosures, "a zero weight is not a position to disclose"


def test_the_cluster_limit_is_absent_when_not_configured():
    ev = evaluate({"SOL": "0.90"}, limits=limits(), clusters=CLUSTERS)
    assert not any(c.name == "max_cluster_exposure" for c in ev.report.checks)
    assert "max_cluster_exposure" in limits().unenforced()


# --- benchmark beta --------------------------------------------------------


def test_portfolio_beta_is_the_weighted_sum():
    ev = evaluate(
        {"BTC": "0.40", "SOL": "0.40"},
        limits=limits(max_benchmark_beta=Decimal("1.00")),
        betas={"BTC": Decimal("1.0"), "SOL": Decimal("1.5")},
    )
    c = check(ev, "max_benchmark_beta")
    assert c.value == Decimal("1.00")
    assert c.passed


def test_a_high_beta_alt_book_is_rejected():
    # The failure §6.2 names: a long book of alts with no beta constraint is a
    # leveraged BTC position wearing a diversification costume.
    ev = evaluate(
        {"SOL": "0.30", "AVAX": "0.30", "NEAR": "0.30"},
        limits=limits(max_benchmark_beta=Decimal("1.00")),
        betas={"SOL": Decimal("1.4"), "AVAX": Decimal("1.5"), "NEAR": Decimal("1.6")},
    )
    c = check(ev, "max_benchmark_beta")
    assert c.value == Decimal("1.35")
    assert not c.passed
    assert not ev.passed


def test_a_short_can_offset_beta_and_the_measure_is_signed():
    # Unlike cluster gross, beta nets: that is what hedging means, and a book
    # that is genuinely market-neutral should read as one.
    ev = evaluate(
        {"SOL": "0.50", "BTC": "-0.50"},
        limits=limits(max_benchmark_beta=Decimal("0.30")),
        betas={"SOL": Decimal("1.2"), "BTC": Decimal("1.0")},
    )
    assert check(ev, "max_benchmark_beta").value == Decimal("0.10")


def test_a_net_short_book_is_measured_on_absolute_beta():
    ev = evaluate(
        {"BTC": "-0.80"},
        limits=limits(max_benchmark_beta=Decimal("0.50")),
        betas={"BTC": Decimal("1.0")},
    )
    c = check(ev, "max_benchmark_beta")
    assert c.value == Decimal("0.80"), "-0.8 beta is as much exposure as +0.8"
    assert not c.passed


def test_an_unestimable_beta_is_assumed_conservatively_and_disclosed():
    ev = evaluate(
        {"NEWCOIN": "0.90"},
        limits=limits(max_benchmark_beta=Decimal("0.50")),
        betas={"NEWCOIN": None},
    )
    c = check(ev, "max_benchmark_beta")
    assert c.value == Decimal("0.90"), "assumed 1.0, so the limit binds harder"
    assert not c.passed
    assert any("NEWCOIN" in d for d in ev.disclosures)


def test_assuming_a_beta_can_only_tighten_the_limit():
    # The direction is the whole argument for the default. A book that cannot
    # be measured must not pass by virtue of being unmeasurable.
    known = evaluate(
        {"A": "0.50", "B": "0.50"},
        limits=limits(max_benchmark_beta=Decimal("2.00")),
        betas={"A": Decimal("0.2"), "B": Decimal("0.2")},
    )
    unknown = evaluate(
        {"A": "0.50", "B": "0.50"},
        limits=limits(max_benchmark_beta=Decimal("2.00")),
        betas={"A": None, "B": None},
    )
    assert unknown.report.checks[-1].value > known.report.checks[-1].value


def test_a_missing_asset_in_the_beta_map_is_treated_as_unestimable():
    # Absent and null must behave identically, or a lookup miss becomes a
    # silent zero-beta position.
    ev = evaluate(
        {"GHOST": "0.90"},
        limits=limits(max_benchmark_beta=Decimal("0.50")),
        betas={},
    )
    assert check(ev, "max_benchmark_beta").value == Decimal("0.90")
    assert any("GHOST" in d for d in ev.disclosures)


# --- the grouping itself ---------------------------------------------------


def test_clusters_invert_from_groups_to_assets():
    assert clusters_from_dict({"alt_l1": ["SOL", "avax"]}) == {
        "SOL": "alt_l1",
        "AVAX": "alt_l1",
    }


def test_an_asset_in_two_clusters_is_refused_at_load_time():
    with pytest.raises(ValueError, match="two clusters"):
        clusters_from_dict({"alt_l1": ["SOL"], "defi": ["SOL"]})


def test_the_same_asset_twice_in_one_cluster_is_fine():
    assert clusters_from_dict({"alt_l1": ["SOL", "SOL"]}) == {"SOL": "alt_l1"}


# --- the gate as a whole ---------------------------------------------------


def test_one_breach_rejects_the_whole_plan_not_the_offending_leg():
    ev = evaluate(
        {"SOL": "0.30", "AVAX": "0.30", "BTC": "0.05"},
        limits=limits(max_cluster_exposure=Decimal("0.40")),
        clusters=CLUSTERS,
    )
    assert not ev.passed
    # Every check still ran and is reported; rejection is whole, and the record
    # shows what else would have passed.
    assert len(ev.report.checks) == 4
    assert check(ev, "max_gross_exposure").passed
