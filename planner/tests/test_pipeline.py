"""The decision path, end to end, on synthetic bars.

Synthetic rather than recorded market data so the Phase 0 gate is enforceable in
CI without shipping a data set — and so the gate tests determinism of the *code*
rather than immutability of a fixture.

The gate: the same decision computed twice is one plan, not two that agree.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl
import pytest

from planner import pipeline, plan as P, state, store, universe
from planner.config import Config, CostModel, RiskLimits

DAY = 86_400
AS_OF = datetime(2026, 8, 1, tzinfo=timezone.utc)
ASSETS = ("AAA", "BBB", "CCC")


def _bars(n: int = 120) -> pl.DataFrame:
    """A contiguous series per asset: each bar opens where the last one closed.

    Returns oscillate deterministically rather than growing at a constant rate.
    A constant-return series has *zero* realised volatility, which the cost model
    correctly treats as "no basis for an impact estimate" and prices punitively —
    deadbanding every trade. Real-looking variance is needed for the pipeline to
    do anything, and having hit that once it is worth stating.

    The newest bar sits exactly at `as_of`, so there is always a forming bar for
    the horizon to exclude.
    """
    rows = []
    for i, asset in enumerate(ASSETS):
        close = 100.0 * (i + 1)
        for d in range(n):
            ts = AS_OF - timedelta(days=n - 1 - d)
            nxt = close * (1.002 + 0.01 * ((d % 7) - 3) / 3)
            rows.append(
                {
                    "ts_utc": ts,
                    "asset": asset,
                    "interval_s": DAY,
                    "open": close,
                    "high": max(close, nxt) * 1.01,
                    "low": min(close, nxt) * 0.99,
                    "close": nxt,
                    "volume": 1_000.0,
                    "quote_volume": 500_000_000.0,
                    "trades": 10_000,
                }
            )
            close = nxt
    return pl.DataFrame(rows).with_columns(
        pl.col("ts_utc").dt.replace_time_zone("UTC"), pl.col("interval_s").cast(pl.Int32)
    )


def _config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=DAY,
        universe=list(ASSETS),
        target_gross_exposure=Decimal("0.75"),
        constructor="equal_weight",
        min_dollar_volume=Decimal(1_000_000),
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
    return Config(**(base | over))


@pytest.fixture
def env(tmp_path: Path) -> Path:
    store.write(_bars(), root=tmp_path, source="synthetic")
    universe.record(
        universe.from_config(list(ASSETS)), as_of=AS_OF, source="test", root=tmp_path
    )
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=AS_OF),
        tmp_path / "book.json",
    )
    return tmp_path


def _run(root: Path, *, created_at: datetime, **over) -> dict:
    return pipeline.run(
        as_of=AS_OF, config=_config(**over), data_root=root, created_at=created_at
    ).document


# --- the Phase 0 gate ------------------------------------------------------


def test_the_same_decision_twice_is_one_plan(env):
    """The gate. Different wall-clock stamps, identical decision."""
    a = _run(env, created_at=datetime(2026, 1, 1, tzinfo=timezone.utc))
    b = _run(env, created_at=datetime(2026, 6, 15, 9, 30, tzinfo=timezone.utc))

    assert a["created_at"] != b["created_at"], "the stamps must differ or this proves nothing"
    assert P.digest(a) == P.digest(b)
    assert a["plan_id"] == b["plan_id"]
    assert {k: v for k, v in a.items() if k != "created_at"} == {
        k: v for k, v in b.items() if k != "created_at"
    }


def test_a_changed_input_changes_the_plan(env):
    """The converse: determinism must not be indifference to inputs."""
    a = _run(env, created_at=AS_OF)
    b = _run(env, created_at=AS_OF, target_gross_exposure=Decimal("0.60"))
    assert a["plan_id"] != b["plan_id"]


# --- shape -----------------------------------------------------------------


def test_produces_a_valid_accepted_plan(env):
    doc = _run(env, created_at=AS_OF)
    P.validate(doc)
    assert doc["status"] == "accepted"
    assert doc["mode"] == "dry"
    assert len(doc["orders"]) == 3
    assert all(o["reason"] == "entry" for o in doc["orders"])


def test_disclosures_are_present(env):
    """Unenforced limits and the placeholder signal must be on every plan."""
    doc = _run(env, created_at=AS_OF)
    kinds = {w["kind"] for w in doc["warnings"]}
    assert "unenforced_rule" in kinds
    messages = " ".join(w["message"] for w in doc["warnings"])
    assert "claims no edge" in messages
    assert "max_cluster_exposure" in messages


def test_inputs_hash_is_recorded(env):
    doc = _run(env, created_at=AS_OF)
    assert len(doc["provenance"]["inputs_hash"]) == 16
    assert doc["provenance"]["universe_size"] == 3


# --- the horizon -----------------------------------------------------------


def test_usable_horizon_excludes_the_forming_bar():
    """A decision at T may only see bars that closed at or before T."""
    assert pipeline.usable_horizon(AS_OF, DAY) == AS_OF - timedelta(days=1)


def test_plan_never_sees_a_bar_newer_than_its_horizon(env):
    """The bar opening at as_of has not closed; using it is a look at the future."""
    doc = _run(env, created_at=AS_OF)
    bars = store.read(root=env, interval_s=DAY)
    horizon = pipeline.usable_horizon(AS_OF, DAY)
    used = bars.filter(pl.col("ts_utc") <= horizon)
    assert used.height < bars.height, "the newest bar must be excluded"


# --- gates fail closed -----------------------------------------------------


def test_missing_universe_snapshot_stops_the_run(tmp_path):
    store.write(_bars(), root=tmp_path, source="synthetic")
    with pytest.raises(pipeline.GateFailure, match="no universe snapshot"):
        _run(tmp_path, created_at=AS_OF)


def test_no_bars_stops_the_run(tmp_path):
    universe.record(
        universe.from_config(list(ASSETS)), as_of=AS_OF, source="test", root=tmp_path
    )
    with pytest.raises(pipeline.GateFailure, match="no bars"):
        _run(tmp_path, created_at=AS_OF)


def test_incomplete_universe_stops_the_run(tmp_path):
    """A truncated universe manufactures false 'dropped out' exits."""
    store.write(_bars().filter(pl.col("asset") != "CCC"), root=tmp_path, source="synthetic")
    universe.record(
        universe.from_config(list(ASSETS)), as_of=AS_OF, source="test", root=tmp_path
    )
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=AS_OF),
        tmp_path / "book.json",
    )
    with pytest.raises(pipeline.GateFailure, match="incomplete"):
        _run(tmp_path, created_at=AS_OF)


def test_stale_bars_stop_the_run(tmp_path):
    old = _bars().filter(pl.col("ts_utc") < AS_OF - timedelta(days=10))
    store.write(old, root=tmp_path, source="synthetic")
    universe.record(
        universe.from_config(list(ASSETS)), as_of=AS_OF, source="test", root=tmp_path
    )
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=AS_OF),
        tmp_path / "book.json",
    )
    with pytest.raises(pipeline.GateFailure, match="stale"):
        _run(tmp_path, created_at=AS_OF)


def test_zero_nav_stops_the_run(env):
    state.save(state.Portfolio(cash=Decimal(0), positions=[], as_of=AS_OF), env / "book.json")
    with pytest.raises(pipeline.GateFailure, match="non-positive NAV"):
        _run(env, created_at=AS_OF)


def test_holding_an_unpriced_asset_stops_the_run(env):
    """Marking a held position at zero silently understates NAV and every weight."""
    state.save(
        state.Portfolio(
            cash=Decimal(100_000),
            positions=[state.Position(asset="ZZZ", qty=Decimal(1))],
            as_of=AS_OF,
        ),
        env / "book.json",
    )
    with pytest.raises(pipeline.GateFailure, match="no price"):
        _run(env, created_at=AS_OF)


# --- risk ------------------------------------------------------------------


def test_a_breached_limit_rejects_the_whole_plan(env):
    doc = _run(
        env,
        created_at=AS_OF,
        limits=RiskLimits(
            max_gross_exposure=Decimal("0.10"),
            max_position=Decimal("0.30"),
            max_position_count=20,
            min_position_notional=Decimal(25),
        ),
    )
    assert doc["status"] == "rejected"
    assert doc["orders"] == []
    assert "max_gross_exposure" in doc["risk_report"]["rejected_reason"]


def test_turnover_budget_defers_rather_than_rejects(env):
    """A budget paces the transition; it never vetoes a legal destination."""
    doc = _run(env, created_at=AS_OF, turnover_budget=Decimal("0.25"))
    assert doc["status"] == "accepted"
    assert len(doc["orders"]) == 1
    assert any(w["kind"] == "turnover_capped" for w in doc["warnings"])


def _limits(**over) -> RiskLimits:
    base = dict(
        max_gross_exposure=Decimal("1.0"),
        max_position=Decimal("0.30"),
        max_position_count=20,
        min_position_notional=Decimal(25),
    )
    return RiskLimits(**(base | over))


def test_a_correlated_cluster_rejects_the_whole_plan(env):
    """Three names individually inside every per-asset limit, and one bet."""
    doc = _run(
        env,
        created_at=AS_OF,
        limits=_limits(max_cluster_exposure=Decimal("0.40")),
        clusters={a: "one_group" for a in ASSETS},
    )
    assert doc["status"] == "rejected"
    assert doc["orders"] == []
    assert "max_cluster_exposure" in doc["risk_report"]["rejected_reason"]

    check = next(
        c for c in doc["risk_report"]["checks"] if c["name"] == "max_cluster_exposure"
    )
    assert Decimal(check["value"]) == Decimal("0.75"), "the whole book is one cluster"


def test_separate_clusters_pass_the_same_book(env):
    # Same weights, same limit, different grouping. The limit measures
    # correlation as declared, which is exactly why the declaration matters.
    doc = _run(
        env,
        created_at=AS_OF,
        limits=_limits(max_cluster_exposure=Decimal("0.40")),
        clusters={a: f"group_{a}" for a in ASSETS},
    )
    assert doc["status"] == "accepted"


def test_an_unclassified_asset_is_disclosed_on_the_plan(env):
    doc = _run(
        env,
        created_at=AS_OF,
        limits=_limits(max_cluster_exposure=Decimal("0.40")),
        clusters={"AAA": "one_group"},
    )
    assert doc["status"] == "accepted"
    disclosures = [w["message"] for w in doc["warnings"] if w["kind"] == "unenforced_rule"]
    assert any("BBB" in m and "CCC" in m for m in disclosures), (
        "an asset outside the grouping escapes the limit, and the plan must say so"
    )


def test_an_enforced_beta_limit_with_no_benchmark_stops_the_run(env):
    """A limit that cannot be evaluated is not a limit (design spec 0.3)."""
    with pytest.raises(pipeline.GateFailure, match="no benchmark is configured"):
        _run(env, created_at=AS_OF, limits=_limits(max_benchmark_beta=Decimal("1.0")))


def test_an_enforced_beta_limit_with_an_absent_benchmark_stops_the_run(env):
    with pytest.raises(pipeline.GateFailure, match="has no bars"):
        _run(
            env,
            created_at=AS_OF,
            benchmark="ZZZ",
            limits=_limits(max_benchmark_beta=Decimal("1.0")),
        )


def test_the_beta_limit_is_evaluated_against_the_configured_benchmark(env):
    doc = _run(
        env,
        created_at=AS_OF,
        benchmark="AAA",
        limits=_limits(max_benchmark_beta=Decimal("5.0")),
    )
    check = next(
        c for c in doc["risk_report"]["checks"] if c["name"] == "max_benchmark_beta"
    )
    assert check["passed"]
    assert Decimal(check["value"]) > 0, "a long book has non-zero benchmark exposure"


def test_a_tight_beta_limit_rejects_the_plan(env):
    doc = _run(
        env,
        created_at=AS_OF,
        benchmark="AAA",
        limits=_limits(max_benchmark_beta=Decimal("0.01")),
    )
    assert doc["status"] == "rejected"
    assert "max_benchmark_beta" in doc["risk_report"]["rejected_reason"]


def test_the_benchmark_is_featurized_even_when_it_is_not_eligible(tmp_path: Path):
    """The yardstick is not a holding, and dropping it disables the limit.

    AAA has bars but is not in the recorded universe, so it is never a target.
    Its return series is still what BBB's and CCC's betas are measured against.
    """
    store.write(_bars(), root=tmp_path, source="synthetic")
    universe.record(
        universe.from_config(["BBB", "CCC"]), as_of=AS_OF, source="test", root=tmp_path
    )
    state.save(
        state.Portfolio(cash=Decimal(100_000), positions=[], as_of=AS_OF),
        tmp_path / "book.json",
    )

    doc = _run(
        tmp_path,
        created_at=AS_OF,
        benchmark="AAA",
        limits=_limits(max_benchmark_beta=Decimal("5.0")),
    )
    assert {t["asset"] for t in doc["targets"]} == {"BBB", "CCC"}
    check = next(
        c for c in doc["risk_report"]["checks"] if c["name"] == "max_benchmark_beta"
    )
    assert check["passed"]
    assert not any(
        "beta assumed" in w["message"] for w in doc["warnings"]
    ), "AAA's bars were available, so both betas were estimable"


def test_a_holding_that_leaves_the_universe_can_still_be_marked_and_sold(tmp_path: Path):
    """Eligibility governs what may be ENTERED, never what may be exited.

    A ranking strategy drops names constantly — that is what ranking is. If a
    position leaving the eligible universe made the run impossible, the first
    such drop would deadlock the system permanently. This is the regression
    test for exactly that: it produced 243 gate failures out of 253 dates on
    the first real Phase 1 run.
    """
    store.write(_bars(), root=tmp_path, source="synthetic")
    # CCC has bars but is not in the recorded universe.
    universe.record(
        universe.from_config(["AAA", "BBB"]), as_of=AS_OF, source="test", root=tmp_path
    )
    state.save(
        state.Portfolio(
            cash=Decimal(50_000),
            positions=[state.Position(asset="CCC", qty=Decimal("10"))],
            as_of=AS_OF,
        ),
        tmp_path / "book.json",
    )

    doc = _run(tmp_path, created_at=AS_OF)

    assert doc["status"] == "accepted"
    marked = {c["asset"] for c in doc["current"]}
    assert "CCC" in marked, "a held position must be marked whatever the universe says"
    assert Decimal(doc["nav"]["total"]) > Decimal(50_000), "CCC contributes to NAV"

    # And it is on its way out: no target, so the diff sells it.
    assert "CCC" not in {t["asset"] for t in doc["targets"]}
    exits = [o for o in doc["orders"] if o["asset"] == "CCC"]
    assert exits and exits[0]["side"] == "sell"
    assert exits[0]["reason"] == "exit"


def test_a_holding_with_no_bars_at_all_still_stops_the_run(tmp_path: Path):
    # The original gate is still right for its actual case: an asset we cannot
    # price from any bar cannot be marked, and marking it at zero would
    # understate NAV and every weight computed from it.
    store.write(_bars(), root=tmp_path, source="synthetic")
    universe.record(
        universe.from_config(list(ASSETS)), as_of=AS_OF, source="test", root=tmp_path
    )
    state.save(
        state.Portfolio(
            cash=Decimal(50_000),
            positions=[state.Position(asset="ZZZ", qty=Decimal("10"))],
            as_of=AS_OF,
        ),
        tmp_path / "book.json",
    )
    with pytest.raises(pipeline.GateFailure, match="no price"):
        _run(tmp_path, created_at=AS_OF)
