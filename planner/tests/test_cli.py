"""The CLI surface.

Design spec §0.5: every action is a command you can run yourself, and §12: what
was not enforced is reported *before* any number. The second one is an ordering
invariant, and an ordering invariant that nothing checks is a convention rather
than a guarantee — a disclosure printed under a table has already failed at its
job.
"""

from __future__ import annotations

import math
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl
import pytest

from planner import cli, state, store, universe

DAY = 86_400
AS_OF = datetime(2026, 8, 1, tzinfo=timezone.utc)
ASSETS = ("BTC", "ETH", "SOL", "AVAX", "LINK", "UNI")


def _bars(assets=ASSETS, n: int = 200) -> pl.DataFrame:
    rows = []
    for i, asset in enumerate(assets):
        close = 100.0 * (i + 1)
        for d in range(n):
            ts = AS_OF - timedelta(seconds=DAY * (n - 1 - d))
            nxt = close * math.exp(0.004 * math.sin(i + d * 0.6) + 0.0006 * (i + 1))
            rows.append(
                {
                    "asset": asset,
                    "ts_utc": ts,
                    "interval_s": DAY,
                    "open": close,
                    "high": max(close, nxt),
                    "low": min(close, nxt),
                    "close": nxt,
                    "volume": 100_000.0,
                    "quote_volume": 5e8 * (i + 1),
                    "trades": 10_000,
                }
            )
            close = nxt
    return pl.DataFrame(rows).with_columns(
        pl.col("ts_utc").dt.replace_time_zone("UTC"), pl.col("interval_s").cast(pl.Int32)
    )


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


def run(env: Path, *args: str) -> int:
    return cli.main(["--data-root", str(env), *args])


def test_scores_prints_the_cross_section(env, capsys):
    assert run(env, "scores", "--as-of", "2026-08-01") == 0
    out = capsys.readouterr().out

    for asset in ASSETS:
        assert asset in out
    assert "composite" in out
    assert "momentum" in out


def test_disclosures_come_before_any_number(env, capsys):
    run(env, "scores", "--as-of", "2026-08-01")
    out = capsys.readouterr().out
    assert out.index("DISCLOSURES") < out.index("composite"), (
        "a number read before its caveats has already misled"
    )


def test_the_candidate_factor_set_never_claims_an_edge(env, capsys):
    run(env, "scores", "--as-of", "2026-08-01")
    out = capsys.readouterr().out
    assert "claim no edge" in out
    assert "not a chosen strategy" in out


def test_scores_are_ordered_by_composite(env, capsys):
    run(env, "scores", "--as-of", "2026-08-01")
    lines = [
        line for line in capsys.readouterr().out.splitlines() if line[:8].strip() in ASSETS
    ]
    composites = [float(line.split()[2]) for line in lines]
    assert composites == sorted(composites, reverse=True)


def test_by_cluster_discloses_every_group_too_small_to_rank_within(env, capsys):
    # The default grouping puts six assets into four clusters, none of which
    # reaches the threshold. Every score is neutral and every one says why -
    # which is the honest reading, and far better than a table of numbers that
    # look like measurements.
    assert run(env, "scores", "--as-of", "2026-08-01", "--by-cluster") == 0
    out = capsys.readouterr().out

    assert out.count("scored neutral") == 4, "one disclosure per undersized group"
    assert "small_group" in out
    for asset in ASSETS:
        assert asset in out


def test_scores_fails_closed_when_the_universe_was_never_recorded(tmp_path, capsys):
    store.write(_bars(), root=tmp_path, source="synthetic")
    assert run(tmp_path, "scores", "--as-of", "2026-08-01") == 2
    assert "GATE FAILED" in capsys.readouterr().err


def test_scores_fails_closed_with_no_bars(tmp_path, capsys):
    assert run(tmp_path, "scores", "--as-of", "2026-08-01") == 2
    assert "no bars" in capsys.readouterr().err


def test_the_benchmark_is_scored_only_when_it_is_also_eligible(tmp_path, capsys):
    # BTC has bars and is the configured benchmark, but is not in the recorded
    # universe. It must be featurized (betas need it) and must not be scored:
    # the yardstick is not a holding.
    store.write(_bars(), root=tmp_path, source="synthetic")
    eligible = [a for a in ASSETS if a != "BTC"]
    universe.record(
        universe.from_config(eligible), as_of=AS_OF, source="test", root=tmp_path
    )
    run(tmp_path, "scores", "--as-of", "2026-08-01")

    lines = [
        line
        for line in capsys.readouterr().out.splitlines()
        if line[:8].strip() in ASSETS
    ]
    assert {line.split()[0] for line in lines} == set(eligible)
