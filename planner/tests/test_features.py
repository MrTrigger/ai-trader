"""The feature frame, and the one property that matters most about it.

**A feature is causal exactly when deleting the future does not change it.**
Build over the full history, rebuild over every prefix, require row *i*
identical both times. That is the harness's guarantee (design spec §2) and it is
the single most important test this project inherits — everything downstream
measures a strategy that does not exist if it fails.

`beta_bench` is why this file exists now rather than later. Every other feature
is a rolling window over one asset's own column, which polars makes causal by
construction. Beta joins a *second* series across assets on timestamp, and a
join is exactly the shape of operation that can quietly reach forward.
"""

from __future__ import annotations

import math
from datetime import datetime, timedelta, timezone

import polars as pl
import pytest

from planner import features

DAY = timedelta(days=1)
START = datetime(2026, 1, 1, tzinfo=timezone.utc)


def _series(returns: list[float], start: float = 100.0) -> list[float]:
    """Close prices whose bar-over-bar log returns are `returns`."""
    closes = [start]
    for r in returns:
        closes.append(closes[-1] * math.exp(r))
    return closes


def _bars(closes: dict[str, list[float]]) -> pl.DataFrame:
    """Contiguous daily bars: each bar opens where the last one closed."""
    rows = []
    for asset, series in closes.items():
        for i, close in enumerate(series):
            open_ = series[i - 1] if i else close
            rows.append(
                {
                    "asset": asset,
                    "ts_utc": START + i * DAY,
                    "open": open_,
                    "high": max(open_, close),
                    "low": min(open_, close),
                    "close": close,
                    "volume": 1_000.0,
                    "quote_volume": 1_000.0 * close,
                }
            )
    return pl.DataFrame(rows).sort(["asset", "ts_utc"])


def _wobble(n: int, seed: int) -> list[float]:
    """Deterministic pseudo-returns. Not random: a fixed test needs a fixed series."""
    return [0.01 * math.sin(seed + i * 0.7) + 0.002 * math.cos(seed * 2 + i * 0.31) for i in range(n)]


N = 140
BENCH_RETURNS = _wobble(N, seed=1)


@pytest.fixture
def bars() -> pl.DataFrame:
    return _bars(
        {
            "BTC": _series(BENCH_RETURNS),
            # Exactly twice the benchmark's log return, every bar. Beta must
            # come out at 2 and nothing else.
            "LEV": _series([2 * r for r in BENCH_RETURNS], start=50.0),
            "ETH": _series(_wobble(N, seed=7), start=2_000.0),
        }
    )


# --- causality -------------------------------------------------------------


def test_deleting_the_future_does_not_change_the_past(bars):
    """Build over every prefix; row i must be identical to the full-history row i.

    This is the test that catches a centred window, a forward fill from a later
    row, or a join that resolved against data the row could not have seen.
    """
    full = features.build(bars, benchmark="BTC")
    timestamps = sorted(bars["ts_utc"].unique().to_list())

    # Every prefix long enough for the widest window to have produced anything.
    for k in range(features._BETA_WINDOW, len(timestamps) + 1):
        cutoff = timestamps[k - 1]
        prefix = features.build(bars.filter(pl.col("ts_utc") <= cutoff), benchmark="BTC")
        expected = full.filter(pl.col("ts_utc") <= cutoff)

        assert prefix.sort(["asset", "ts_utc"]).equals(expected.sort(["asset", "ts_utc"])), (
            f"features over the first {k} bars differ from the same rows computed "
            "with the full history: something is reading forward"
        )


def test_causality_holds_for_beta_specifically(bars):
    # Narrowed to the one column that joins across assets, so a failure names
    # the culprit instead of pointing at the whole frame.
    full = features.build(bars, benchmark="BTC")
    timestamps = sorted(bars["ts_utc"].unique().to_list())
    cutoff = timestamps[features._BETA_WINDOW + 5]

    prefix = features.build(bars.filter(pl.col("ts_utc") <= cutoff), benchmark="BTC")

    left = prefix.sort(["asset", "ts_utc"]).select(["asset", "ts_utc", "beta_bench"])
    right = (
        full.filter(pl.col("ts_utc") <= cutoff)
        .sort(["asset", "ts_utc"])
        .select(["asset", "ts_utc", "beta_bench"])
    )
    assert left.equals(right)


# --- beta ------------------------------------------------------------------


def _beta_of(frame: pl.DataFrame, asset: str) -> float | None:
    row = features.latest(frame).filter(pl.col("asset") == asset)
    return row["beta_bench"][0]


def test_the_benchmark_has_a_beta_of_one_against_itself(bars):
    frame = features.build(bars, benchmark="BTC")
    assert _beta_of(frame, "BTC") == pytest.approx(1.0)


def test_an_asset_that_moves_twice_as_hard_has_a_beta_of_two(bars):
    frame = features.build(bars, benchmark="BTC")
    assert _beta_of(frame, "LEV") == pytest.approx(2.0)


def test_beta_is_null_until_the_window_is_full(bars):
    frame = features.build(bars, benchmark="BTC")
    eth = frame.filter(pl.col("asset") == "ETH").sort("ts_utc")

    # Row 1 is the first with a return at all, so the window closes at
    # _BETA_WINDOW returns -- one row later than the bar count suggests.
    assert eth["beta_bench"][features._BETA_WINDOW - 1] is None
    assert eth["beta_bench"][features._BETA_WINDOW] is not None


def test_beta_is_null_when_no_benchmark_is_configured(bars):
    frame = features.build(bars)
    assert frame["beta_bench"].null_count() == frame.height
    assert _beta_of(frame, "ETH") is None


def test_beta_is_null_when_the_benchmark_has_no_bars(bars):
    # Not an error here: the risk gate decides what an unmeasurable beta means,
    # and it fails closed on that. This function does not guess.
    frame = features.build(bars, benchmark="SPY")
    assert frame["beta_bench"].null_count() == frame.height


def test_a_motionless_benchmark_gives_null_not_a_division_by_zero():
    flat = _bars({"BTC": [100.0] * N, "ETH": _series(_wobble(N, seed=3), start=2_000.0)})
    frame = features.build(flat, benchmark="BTC")
    assert _beta_of(frame, "ETH") is None, "no variance to regress against"


def test_an_inverse_asset_has_a_negative_beta(bars):
    inverse = _bars(
        {"BTC": _series(BENCH_RETURNS), "SHORT": _series([-r for r in BENCH_RETURNS], start=10.0)}
    )
    frame = features.build(inverse, benchmark="BTC")
    assert _beta_of(frame, "SHORT") == pytest.approx(-1.0)


# --- the rest of the frame -------------------------------------------------


def test_volatility_is_annualised_on_a_365_day_year(bars):
    frame = features.build(bars, benchmark="BTC")
    eth = frame.filter(pl.col("asset") == "ETH").sort("ts_utc")

    daily = eth["ret_1"].tail(features._VOL_WINDOW).std()
    assert eth["vol_30"][-1] == pytest.approx(daily * math.sqrt(365))


def test_bars_available_counts_this_assets_own_history(bars):
    frame = features.build(bars, benchmark="BTC")
    assert frame.filter(pl.col("asset") == "ETH")["bars_available"].max() == N + 1


def test_an_empty_frame_stays_empty():
    assert features.build(pl.DataFrame()).is_empty()


def test_latest_returns_one_row_per_asset(bars):
    cross = features.latest(features.build(bars, benchmark="BTC"))
    assert cross.height == 3
    assert sorted(cross["asset"].to_list()) == ["BTC", "ETH", "LEV"]


# --- ticker discontinuities ------------------------------------------------


def test_a_ticker_that_changes_meaning_is_retired_and_its_mark_frozen():
    """The LUNA case, and the reason this check exists.

    Binance renamed the collapsed Terra token to LUNC and launched Luna 2.0
    under the same `LUNA` ticker. The series is continuous and describes two
    different assets. A backtest bought the old token at $0.0001, received 12.4
    million units, and the ticker restarted at $7 — a $43k book became $88m in
    one step, and every number after it was fiction.

    The ticker is retired rather than merely aged: without corporate-action data
    we cannot tell which units a position is denominated in, so the honest
    answer is to stop trading it and mark what is held at the last price the old
    token actually traded at.
    """
    old = _series(_wobble(120, seed=2), start=100.0)
    new = _series(_wobble(60, seed=5), start=old[-1] * 100_000)
    frame = features.build(_bars({"X": old + new}))

    x = frame.filter(pl.col("asset") == "X").sort("ts_utc")
    assert x["had_discontinuity"][-1]
    assert x["bars_available"][-1] == 0, "never tradeable again"
    assert x["bars_available"].max() == len(old), "the pre-break regime is untouched"

    # Frozen at the last pre-break close, not the post-break one.
    assert x["mark_close"][-1] == pytest.approx(old[-1])
    assert x["close"][-1] > old[-1] * 10_000, "the raw series really did jump"


def test_an_ordinary_asset_has_no_discontinuity(bars):
    frame = features.build(bars, benchmark="BTC")
    assert not frame["had_discontinuity"].any()


def test_a_collapse_is_not_treated_as_a_discontinuity():
    """One-sided on purpose.

    A 90% single-day loss is a thing that genuinely happens in crypto — it is
    what LUNA actually did — and erasing it would remove exactly the history a
    momentum strategy most needs to be tested against.
    """
    collapsing = _series(_wobble(120, seed=3)) + [1.0, 0.05, 0.004, 0.0009]
    frame = features.build(_bars({"X": collapsing}))
    x = frame.filter(pl.col("asset") == "X")
    assert not x["had_discontinuity"].any()
    assert x["bars_available"].max() == len(collapsing)
    assert x["mark_close"][-1] == pytest.approx(collapsing[-1]), "no freeze applied"


def test_a_large_but_plausible_gain_is_not_a_discontinuity():
    # 4x in a day is extraordinary and does happen. 10x is the line.
    frame = features.build(_bars({"X": _series(_wobble(120, seed=4)) + [100.0, 400.0]}))
    assert not frame.filter(pl.col("asset") == "X")["had_discontinuity"].any()


def test_a_discontinuity_does_not_leak_across_assets():
    frame = features.build(
        _bars(
            {
                "BROKEN": _series(_wobble(60, seed=1)) + [100.0, 100_000_000.0],
                "FINE": _series(_wobble(62, seed=6), start=50.0),
            }
        )
    )
    assert frame.filter(pl.col("asset") == "BROKEN")["had_discontinuity"].any()
    assert not frame.filter(pl.col("asset") == "FINE")["had_discontinuity"].any()
