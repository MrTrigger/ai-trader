"""The market-neutral signal, the borrow table, and the shortable feature.

Three properties carry most of the weight here and each has a specific way of
failing silently:

**Borrow is point-in-time.** Using today's shortable list over history is the
borrow-side twin of a survivorship-biased universe, and it flatters results for
exactly the same reason - the instruments that got listings later are
disproportionately the ones that did well. A test that only checks "is BTC
shortable" would pass while the whole history was wrong, so the tests here check
the *boundary date*.

**Both legs or neither.** A book that keeps one leg when the other cannot form
is not market-neutral, it is a directional bet arrived at by accident. That is
the more dangerous failure because the result still looks like a portfolio.

**The tilt is bounded.** Gross must stay at the configured target in every
regime state; a tilt that could push a leg past the cap would be leverage
introduced through the back door, which §9.2 puts out of scope.
"""

from __future__ import annotations

from datetime import date, datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl
import pytest

from planner import borrow, construct, features, signal
from planner.config import Config, CostModel, RiskLimits


def config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=86_400,
        universe=[],
        benchmark="BTC",
        target_gross_exposure=Decimal("1.0"),
        constructor="conviction_tilt",
        min_dollar_volume=Decimal(1_000_000),
        min_history_bars=60,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("1.0"),
        max_holdings=10,
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


def cross(
    *,
    longs: int = 4,
    shorts: int = 6,
    shortable: bool = True,
    regime: str = "flat",
    slope: float | None = 0.02,
) -> pl.DataFrame:
    """A cross-section with a controllable number of names on each side.

    `regime` places the benchmark relative to its own regime channel, which is
    the only thing the tilt reads.
    """
    rows = []
    for i in range(longs):
        rows.append(
            dict(asset=f"L{i}", gc_breakout_age=i + 1, gc_upper=90.0, gc_lower=70.0,
                 close=100.0, shortable=shortable)
        )
    for i in range(shorts):
        # Spread the shorts below their lower band so the ranking has something
        # to order by; S0 is furthest below.
        rows.append(
            dict(asset=f"S{i}", gc_breakout_age=None, gc_upper=90.0, gc_lower=70.0,
                 close=50.0 + i, shortable=shortable)
        )

    close = {"up": 100.0, "down": 50.0, "flat": 80.0}[regime]
    rows.append(
        dict(asset="BTC", gc_breakout_age=1, gc_upper=90.0, gc_lower=70.0,
             close=close, shortable=True)
    )

    frame = pl.DataFrame(rows)
    return frame.with_columns(
        pl.lit(200).alias("bars_available"),
        pl.lit(5e8).alias("adv_quote"),
        pl.lit(0.5).alias("vol_30"),
        pl.lit(80.0).alias("gc_regime_filter"),
        pl.lit(90.0).alias("gc_regime_upper"),
        pl.lit(slope, dtype=pl.Float64).alias("gc_regime_slope"),
    )


def legs(result) -> tuple[list[str], list[str]]:
    return (
        sorted(s.asset for s in result.signals if s.direction == "long"),
        sorted(s.asset for s in result.signals if s.direction == "short"),
    )


# --- leg formation ----------------------------------------------------------


def test_it_holds_both_legs():
    long_, short = legs(signal.get("gc_long_short").generate(cross(), config=config()))
    assert long_ and short
    assert all(a.startswith("L") or a == "BTC" for a in long_)
    assert all(a.startswith("S") for a in short)


def test_the_short_leg_is_the_residual_not_a_second_selection():
    # Everything eligible, shortable, and not above its band is shorted. If this
    # ever becomes a selection the count will stop matching the complement.
    _, short = legs(signal.get("gc_long_short").generate(cross(shorts=6), config=config()))
    assert len(short) == 6


def test_a_leg_too_thin_stands_the_whole_book_down():
    # Not "keep the leg that formed". One leg alone is a directional bet this
    # signal never claimed to have.
    result = signal.get("gc_long_short").generate(cross(shorts=2), config=config())
    assert result.signals == []
    assert any("minimum" in n for n in result.notes)


def test_it_stands_down_rather_than_holding_a_thin_long_leg():
    result = signal.get("gc_long_short").generate(cross(longs=1), config=config())
    assert result.signals == []


def test_unborrowable_assets_are_never_shorted():
    result = signal.get("gc_long_short").generate(cross(shortable=False), config=config())
    # No borrow means no short leg, which means no book at all.
    assert result.signals == []
    assert any("borrow" in n for n in result.notes)


def test_missing_shortable_column_is_an_error_not_an_assumption():
    frame = cross().drop("shortable")
    with pytest.raises(ValueError, match="shortable"):
        signal.get("gc_long_short").generate(frame, config=config())


# --- the tilt ---------------------------------------------------------------


def _leg_weight(result, direction: str) -> Decimal:
    return sum(
        (s.conviction for s in result.signals if s.direction == direction), Decimal(0)
    )


def test_a_neutral_regime_splits_the_book_evenly():
    result = signal.get("gc_long_short").generate(cross(regime="flat"), config=config())
    assert _leg_weight(result, "long") == pytest.approx(Decimal("0.5"))
    assert _leg_weight(result, "short") == pytest.approx(Decimal("0.5"))


def test_an_uptrend_leans_long_and_a_downtrend_leans_short():
    up = signal.get("gc_long_short").generate(cross(regime="up"), config=config())
    down = signal.get("gc_long_short").generate(cross(regime="down"), config=config())
    assert _leg_weight(up, "long") > Decimal("0.5")
    assert _leg_weight(down, "long") < Decimal("0.5")


def test_the_tilt_never_changes_gross():
    # The whole claim is that this is a rotation, not leverage (§9.2).
    for regime in ("up", "down", "flat"):
        result = signal.get("gc_long_short").generate(
            cross(regime=regime, slope=99.0), config=config()
        )
        total = _leg_weight(result, "long") + _leg_weight(result, "short")
        assert total == pytest.approx(Decimal(1))


def test_an_extreme_slope_is_capped_rather_than_flipping_the_book():
    result = signal.get("gc_long_short").generate(
        cross(regime="up", slope=1000.0), config=config()
    )
    assert _leg_weight(result, "short") >= Decimal(0)
    assert _leg_weight(result, "long") <= Decimal(1)


def test_a_missing_slope_falls_back_to_neutral_rather_than_guessing():
    result = signal.get("gc_long_short").generate(
        cross(regime="up", slope=None), config=config()
    )
    assert _leg_weight(result, "long") == pytest.approx(Decimal("0.5"))
    assert any("neutral" in n for n in result.notes)


def test_no_benchmark_means_no_tilt():
    result = signal.get("gc_long_short").generate(
        cross(regime="up"), config=config(benchmark=None)
    )
    assert _leg_weight(result, "long") == pytest.approx(Decimal("0.5"))


# --- fitting the risk limit -------------------------------------------------


def test_it_truncates_to_max_position_count():
    # A book the gate would reject is not a book. The limit stays a limit.
    limits = RiskLimits(
        max_gross_exposure=Decimal("1.0"),
        max_position=Decimal("0.30"),
        max_position_count=8,
        min_position_notional=Decimal(25),
    )
    result = signal.get("gc_long_short").generate(
        cross(longs=10, shorts=20), config=config(limits=limits)
    )
    assert len(result.signals) <= 8
    assert any("truncated" in n for n in result.notes)


def test_truncation_keeps_both_legs_above_the_minimum():
    limits = RiskLimits(
        max_gross_exposure=Decimal("1.0"),
        max_position=Decimal("0.30"),
        max_position_count=6,
        min_position_notional=Decimal(25),
    )
    result = signal.get("gc_long_short").generate(
        cross(longs=20, shorts=20, regime="up", slope=99.0), config=config(limits=limits)
    )
    long_, short = legs(result)
    assert len(long_) >= 3 and len(short) >= 3


def test_conviction_survives_construction_as_the_intended_leg_weights():
    # The tilt rides on conviction precisely so no new interface is needed; if
    # `conviction_tilt` ever stops preserving the split this must fail.
    cfg = config()
    result = signal.get("gc_long_short").generate(cross(regime="flat"), config=cfg)
    built = construct.get("conviction_tilt").construct(result.signals, config=cfg)
    longs = sum(w for w in built.weights.values() if w > 0)
    shorts = sum(-w for w in built.weights.values() if w < 0)
    assert longs == pytest.approx(shorts, rel=Decimal("0.01"))


# --- the borrow table -------------------------------------------------------


def test_listings_is_empty_when_no_funding_data_exists(tmp_path: Path):
    assert borrow.listings(root=tmp_path) == {}


def test_listings_takes_the_earliest_date_per_asset(tmp_path: Path):
    d = tmp_path / borrow.FUNDING_DIR
    d.mkdir(parents=True)
    pl.DataFrame(
        {
            "asset": ["AAA", "AAA", "BBB"],
            "day": [
                datetime(2021, 5, 1, tzinfo=timezone.utc),
                datetime(2020, 3, 2, tzinfo=timezone.utc),
                datetime(2022, 1, 9, tzinfo=timezone.utc),
            ],
        }
    ).write_parquet(d / "venue.parquet")
    assert borrow.listings(root=tmp_path) == {
        "AAA": date(2020, 3, 2),
        "BBB": date(2022, 1, 9),
    }


def test_a_borrow_at_any_venue_is_a_borrow(tmp_path: Path):
    d = tmp_path / borrow.FUNDING_DIR
    d.mkdir(parents=True)
    for name, day in (("a.parquet", date(2023, 1, 1)), ("b.parquet", date(2021, 6, 6))):
        pl.DataFrame(
            {
                "asset": ["AAA"],
                "day": [datetime(day.year, day.month, day.day, tzinfo=timezone.utc)],
            }
        ).write_parquet(d / name)
    assert borrow.listings(root=tmp_path)["AAA"] == date(2021, 6, 6)


# --- the shortable feature --------------------------------------------------


def _bars(assets: list[str], n: int = 200) -> pl.DataFrame:
    start = datetime(2020, 1, 1, tzinfo=timezone.utc)
    rows = []
    for a in assets:
        for i in range(n):
            price = 100.0 + i
            rows.append(
                dict(
                    asset=a,
                    ts_utc=start + timedelta(days=i),
                    open=price,
                    high=price + 1,
                    low=price - 1,
                    close=price,
                    volume=1000.0,
                    quote_volume=1e9,
                )
            )
    return pl.DataFrame(rows)


def test_shortable_is_false_before_the_listing_and_true_after():
    frame = features.build(
        _bars(["AAA"]),
        benchmark=None,
        shortable_from={"AAA": date(2020, 3, 1)},
    )
    before = frame.filter(pl.col("ts_utc") < datetime(2020, 3, 1, tzinfo=timezone.utc))
    after = frame.filter(pl.col("ts_utc") >= datetime(2020, 3, 1, tzinfo=timezone.utc))
    assert not before["shortable"].any()
    assert after["shortable"].all()


def test_an_asset_absent_from_the_borrow_table_is_never_shortable():
    frame = features.build(
        _bars(["AAA", "BBB"]), benchmark=None, shortable_from={"AAA": date(2020, 1, 1)}
    )
    assert not frame.filter(pl.col("asset") == "BBB")["shortable"].any()


def test_no_borrow_table_means_nothing_is_shortable():
    frame = features.build(_bars(["AAA"]), benchmark=None)
    assert not frame["shortable"].any()


def test_shortable_is_causal():
    # Same property every other feature is held to: deleting the future must not
    # change any earlier row. A borrow flag derived from "does this asset appear
    # in the table" rather than from a date would fail this.
    bars = _bars(["AAA"])
    table = {"AAA": date(2020, 3, 1)}
    full = features.build(bars, benchmark=None, shortable_from=table)
    cutoff = datetime(2020, 4, 1, tzinfo=timezone.utc)
    prefix = features.build(
        bars.filter(pl.col("ts_utc") <= cutoff), benchmark=None, shortable_from=table
    )
    left = full.filter(pl.col("ts_utc") <= cutoff).sort(["asset", "ts_utc"])["shortable"]
    right = prefix.sort(["asset", "ts_utc"])["shortable"]
    assert left.to_list() == right.to_list()
