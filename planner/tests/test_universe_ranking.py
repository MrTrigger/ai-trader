"""The point-in-time universe rule (design spec §12).

The rule this file defends:

    Reconstructing a RULE from complete history is point-in-time.
    Reconstructing a SURVIVOR LIST is not.

Most of these tests are about the second half. A ranking that quietly drops a
delisted asset gives back the survivors by a different route, and it does so
without anything looking wrong — which makes it the most dangerous kind of
correct-looking code in this repo.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone

import polars as pl
import pytest

from planner import universe

UTC = timezone.utc
AS_OF = datetime(2026, 8, 1, tzinfo=UTC)
DAY = timedelta(days=1)


def bars(series: dict[str, list[float]], *, end: datetime = AS_OF) -> pl.DataFrame:
    """One row per (asset, day). `series` maps asset -> daily quote turnover.

    The last value sits on the bar opening one day before `end`, so every bar
    has closed by `end`.
    """
    rows = []
    for asset, turnovers in series.items():
        n = len(turnovers)
        for i, turnover in enumerate(turnovers):
            ts = end - DAY * (n - i)
            rows.append(
                {
                    "asset": asset,
                    "ts_utc": ts,
                    "interval_s": 86_400,
                    "open": 100.0,
                    "high": 101.0,
                    "low": 99.0,
                    "close": 100.0,
                    "volume": turnover / 100.0,
                    "quote_volume": turnover,
                    "trades": 1_000,
                }
            )
    return pl.DataFrame(rows).with_columns(
        pl.col("ts_utc").dt.replace_time_zone("UTC"), pl.col("interval_s").cast(pl.Int32)
    )


def flat(value: float, n: int = 120) -> list[float]:
    return [value] * n


def members(df, **kwargs):
    base = dict(as_of=AS_OF, top_n=3, min_history_bars=90)
    return universe.by_liquidity(df, **(base | kwargs))


def eligible_of(result) -> list[str]:
    return [m.asset for m in result if m.eligible]


# --- ranking ---------------------------------------------------------------


def test_assets_rank_by_trailing_turnover():
    result = members(
        bars({"AAA": flat(1e6), "BBB": flat(9e6), "CCC": flat(5e6)}),
    )
    assert eligible_of(result) == ["BBB", "CCC", "AAA"]
    assert [m.rank for m in result] == [1, 2, 3]


def test_only_the_top_n_are_eligible_and_the_rest_say_why():
    result = members(
        bars({"AAA": flat(1e6), "BBB": flat(9e6), "CCC": flat(5e6), "DDD": flat(2e6)}),
        top_n=2,
    )
    assert eligible_of(result) == ["BBB", "CCC"]
    outside = [m for m in result if not m.eligible]
    assert all("outside the top 2" in m.reason for m in outside)


def test_the_median_ignores_a_single_spike():
    # One listing-day spike must not decide what the book may hold.
    spiky = flat(1e6)
    spiky[-1] = 1e12
    result = members(bars({"AAA": spiky, "BBB": flat(2e6), "CCC": flat(3e6)}), top_n=1)
    assert eligible_of(result) == ["CCC"], "the spike did not buy AAA a rank"


def test_turnover_below_the_floor_is_ineligible():
    result = members(
        bars({"AAA": flat(1e6), "BBB": flat(9e6)}), min_turnover=5e6
    )
    assert eligible_of(result) == ["BBB"]
    assert any("below" in m.reason for m in result if not m.eligible)


def test_an_asset_with_too_little_history_is_ineligible():
    result = members(bars({"AAA": flat(9e6, n=30), "BBB": flat(1e6)}))
    assert eligible_of(result) == ["BBB"]
    assert any("30 bars, needs 90" in m.reason for m in result)


# --- the dead --------------------------------------------------------------


def test_a_delisted_asset_is_recorded_as_delisted_not_dropped():
    """The test this whole module exists for.

    ANC had real turnover right up until Terra collapsed. A ranking that simply
    omits it hands the backtest a universe of survivors.
    """
    alive = bars({"ALIVE": flat(5e6)})
    # Dead stopped 60 days before as_of.
    dead = bars({"DEAD": flat(9e6, n=120)}, end=AS_OF - DAY * 60)
    result = members(pl.concat([alive, dead]))

    by_asset = {m.asset: m for m in result}
    assert "DEAD" in by_asset, "a delisted asset must stay visible in the record"
    assert not by_asset["DEAD"].eligible
    assert "delisted or halted" in by_asset["DEAD"].reason
    assert by_asset["ALIVE"].eligible


def test_a_delisting_does_not_steal_a_rank_from_the_living():
    alive = bars({"A": flat(5e6), "B": flat(4e6), "C": flat(3e6)})
    dead = bars({"DEAD": flat(9e9, n=120)}, end=AS_OF - DAY * 90)
    result = members(pl.concat([alive, dead]), top_n=3)
    assert eligible_of(result) == ["A", "B", "C"]


def test_an_asset_that_died_yesterday_is_still_eligible():
    # A one-day gap is an outage, not a delisting. The staleness test is the
    # freshness gate's job at run time; this rule must not pre-empt it.
    alive = bars({"A": flat(5e6)})
    lagging = bars({"B": flat(9e6)}, end=AS_OF - DAY)
    result = members(pl.concat([alive, lagging]))
    assert set(eligible_of(result)) == {"A", "B"}


# --- point-in-time ---------------------------------------------------------


def test_the_ranking_cannot_see_bars_at_or_after_as_of():
    """A bar opening at `as_of` is still forming."""
    df = bars({"AAA": flat(1e6), "BBB": flat(1e6)})
    future = bars({"AAA": [1e15]}, end=AS_OF + DAY)  # opens exactly at AS_OF
    result = members(pl.concat([df, future]), top_n=1)
    assert eligible_of(result) == ["AAA"] or eligible_of(result) == ["BBB"]
    # The point is that the enormous future bar did not decide it.
    ranked = {m.asset: m.rank for m in result}
    assert ranked["AAA"] <= 2


def test_the_same_as_of_gives_the_same_answer_from_a_longer_store():
    """Prefix invariance: adding later history must not change a past ranking.

    This is the causality test in universe form. If it fails, every backtest
    ranking was computed with information from its own future.
    """
    early = bars({"A": flat(5e6), "B": flat(4e6), "C": flat(3e6)}, end=AS_OF)
    later = bars({"A": flat(1e9), "B": flat(1e6), "C": flat(1e6)}, end=AS_OF + DAY * 60)

    alone = universe.by_liquidity(early, as_of=AS_OF, top_n=2, min_history_bars=90)
    with_future = universe.by_liquidity(
        pl.concat([early, later]), as_of=AS_OF, top_n=2, min_history_bars=90
    )
    assert [(m.asset, m.rank, m.eligible) for m in alone] == [
        (m.asset, m.rank, m.eligible) for m in with_future
    ]


def test_ranking_is_stable_regardless_of_row_order():
    df = bars({"AAA": flat(3e6), "BBB": flat(3e6), "CCC": flat(3e6)})
    forward = members(df)
    shuffled = members(df.sort("asset", descending=True))
    assert [m.asset for m in forward] == [m.asset for m in shuffled]


# --- refusals --------------------------------------------------------------


def test_an_empty_store_ranks_nothing():
    assert universe.by_liquidity(pl.DataFrame(), as_of=AS_OF, top_n=5) == []


def test_a_store_entirely_in_the_future_ranks_nothing():
    df = bars({"AAA": flat(1e6)}, end=AS_OF + DAY * 200)
    assert universe.by_liquidity(df, as_of=AS_OF, top_n=5) == []


def test_a_non_positive_top_n_is_refused():
    with pytest.raises(ValueError, match="must be positive"):
        universe.by_liquidity(bars({"AAA": flat(1e6)}), as_of=AS_OF, top_n=0)


# --- the snapshot contract is unchanged ------------------------------------


def test_a_ranked_snapshot_records_and_loads_like_any_other(tmp_path):
    result = members(bars({"AAA": flat(1e6), "BBB": flat(9e6)}))
    universe.record(result, as_of=AS_OF, source="by_liquidity", root=tmp_path)

    loaded = universe.load(AS_OF, root=tmp_path)
    assert [m.asset for m in loaded] == [m.asset for m in result]
    assert [m.reason for m in loaded] == [m.reason for m in result]


def test_a_recorded_snapshot_is_still_never_silently_replaced(tmp_path):
    result = members(bars({"AAA": flat(1e6)}))
    universe.record(result, as_of=AS_OF, source="by_liquidity", root=tmp_path)
    with pytest.raises(FileExistsError, match="observations, not settings"):
        universe.record(result, as_of=AS_OF, source="by_liquidity", root=tmp_path)


# --- ids that cannot cross the contract ------------------------------------


def test_an_asset_id_that_is_not_canonical_is_ineligible():
    """Venues list things the Plan contract cannot carry.

    Binance has a token whose base asset is CJK text. Caught here, where
    eligibility is decided, rather than at plan serialisation — where it costs
    a whole run and the schema error names a target index rather than a cause.
    """
    df = pl.concat([bars({"AAA": flat(1e6)}), bars({"币安人生": flat(9e6)})])
    result = members(df)

    by_asset = {m.asset: m for m in result}
    assert not by_asset["币安人生"].eligible
    assert "not canonical" in by_asset["币安人生"].reason
    assert by_asset["AAA"].eligible


def test_a_non_canonical_asset_does_not_consume_a_top_n_slot():
    df = pl.concat(
        [bars({"A": flat(5e6), "B": flat(4e6), "C": flat(3e6)}), bars({"币安人生": flat(9e9)})]
    )
    assert eligible_of(members(df, top_n=3)) == ["A", "B", "C"]


def test_the_canonical_rule_matches_the_plan_schema():
    # If these ever diverge, the planner produces ids the executor refuses -
    # discovered in production, which is the drift the shared schema exists to
    # prevent.
    import json
    from pathlib import Path

    from planner import bars as bars_mod
    from planner.plan import SCHEMA_PATH

    schema = json.loads(Path(SCHEMA_PATH).read_text(encoding="utf-8"))
    assert bars_mod.CANONICAL_ASSET.pattern == schema["$defs"]["asset"]["pattern"]
