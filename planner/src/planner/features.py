"""The point-in-time feature frame.

Every feature here is causal: computed from closed bars at or before `as_of` and
from nothing else. The property that defines causality, borrowed from the
harness, is testable and is tested - build features over the full history, then
rebuild over every prefix, and require row *i* to be identical both times. A
feature is causal exactly when deleting the future does not change it.

Rolling windows in polars satisfy this by construction as long as nothing
centres a window or fills forward from a later row. The tests are what stop that
creeping in later.
"""

from __future__ import annotations

import math
from datetime import date

import polars as pl

from .bars import mark_discontinuities

FEATURE_SET_VERSION = "fs-phase1-6"

# Windows in bars. At the daily interval these are calendar days.
_RETURN_WINDOWS = (7, 30, 90)
_VOL_WINDOW = 30
_ADV_WINDOW = 20

# Momentum measured from t-30 to t-7, skipping the most recent week.
#
# The skip is the point. Short-horizon reversal is well documented in crypto,
# and a plain 30-day return has last week's reversal baked into it - the two
# effects partially cancel and the measurement is of neither. Dropping the
# recent window is the standard equity 12-1 construction adapted to a shorter
# horizon, and it is why `ret_30` is kept separately rather than reused: they
# are different measurements and a strategy should have to say which it means.
_MOMENTUM_LOOKBACK = 30
_MOMENTUM_SKIP = 7

# Beta wants a longer window than vol: it is a two-series estimate, so it needs
# more observations to say anything, and a noisy short-window beta is worse than
# no beta at all. A beta of 0.4 estimated on 60 days would *relax* the section
# 6.2 constraint on the strength of an estimate that cannot support it, whereas
# a null falls back to 1.0 and can only make the limit bind harder.
_BETA_WINDOW = 90



def build(
    bars: pl.DataFrame,
    *,
    benchmark: str | None = None,
    shortable_from: dict[str, date] | None = None,
) -> pl.DataFrame:
    """Feature frame, one row per (asset, bar).

    The caller is responsible for having already trimmed `bars` to the horizon
    (see `pipeline.usable_bars`). This function does not know what `as_of` is,
    which is deliberate: a function that cannot see the horizon cannot leak
    across it.

    `benchmark` is the asset betas are measured against - BTC for crypto, SPY
    for equities (design spec section 6.2). Same code, different benchmark; that
    is the whole reason it is a parameter and not a constant. When it is absent
    from `bars`, `beta_bench` is null everywhere and the risk gate decides what
    to do about that rather than this function guessing.

    `shortable_from` maps an asset to the first date a borrow existed for it -
    in crypto, the day its perpetual listed. It produces a `shortable` column
    that is False before that date and True after, which is the point-in-time
    form the question has to take: whether an asset *can* be shorted is a fact
    about a date, not about an asset, and treating it as the latter would let a
    2026 instrument list open a 2021 short. Omitted, `shortable` is False
    everywhere - a book that does not know what it may borrow may not borrow.
    """
    if bars.is_empty():
        return bars

    df = bars.sort(["asset", "ts_utc"])

    exprs: list[pl.Expr] = [
        # Log return of the bar itself.
        (pl.col("close") / pl.col("close").shift(1)).log().over("asset").alias("ret_1"),
    ]

    for w in _RETURN_WINDOWS:
        exprs.append(
            (pl.col("close") / pl.col("close").shift(w) - 1).over("asset").alias(f"ret_{w}")
        )

    exprs.append(
        (
            pl.col("close").shift(_MOMENTUM_SKIP)
            / pl.col("close").shift(_MOMENTUM_LOOKBACK)
            - 1
        )
        .over("asset")
        .alias(f"ret_{_MOMENTUM_LOOKBACK}_skip_{_MOMENTUM_SKIP}")
    )

    df = df.with_columns(exprs)

    # Borrow availability, as of each bar. Assets with no entry are never
    # shortable rather than silently shortable, so a gap in the borrow table
    # under-trades instead of inventing a position that could not be opened.
    if shortable_from:
        first = pl.col("asset").replace_strict(
            {a: d.isoformat() for a, d in shortable_from.items()},
            default=None,
            return_dtype=pl.Utf8,
        )
        df = df.with_columns(
            pl.when(first.is_null())
            .then(pl.lit(False))
            .otherwise(pl.col("ts_utc").dt.date() >= first.str.to_date())
            .alias("shortable")
        )
    else:
        df = df.with_columns(pl.lit(False).alias("shortable"))

    df = df.with_columns(
        [
            # Realised volatility, annualised on a 365-day crypto year - not 252.
            # Crypto never closes, so there are no non-trading days to exclude,
            # and using the equity convention here would understate vol by ~20%.
            (
                pl.col("ret_1").rolling_std(window_size=_VOL_WINDOW, min_samples=_VOL_WINDOW)
                * (365 ** 0.5)
            )
            .over("asset")
            .alias("vol_30"),
            # Median, not mean: one exchange-outage day or one listing spike
            # should not set the liquidity estimate that position sizing leans on.
            pl.col("quote_volume")
            .rolling_median(window_size=_ADV_WINDOW, min_samples=_ADV_WINDOW)
            .over("asset")
            .alias("adv_quote"),
            pl.len().over("asset").alias("_asset_rows"),
        ]
    )

    # A ticker whose identity changed is never tradeable again. Not merely
    # "young": we cannot tell which units a position is denominated in without
    # corporate-action data we do not have, so the honest answer is to stop
    # trading it, mark what is held at the last pre-break price, and say so.
    df = mark_discontinuities(df)
    df = df.with_columns(
        pl.when(pl.col("had_discontinuity"))
        .then(0)
        .otherwise(pl.col("ts_utc").cum_count().over("asset"))
        .cast(pl.UInt32)
        .alias("bars_available"),
    ).drop("_asset_rows")

    return _with_beta(_with_gaussian_channel(df), benchmark)


# Gaussian Channel (Donovan Wall). The detector the TR-GC prompt family uses,
# reimplemented here so the same gate can be pointed at it.
#
# The N-pole filter is N cascaded single-pole filters with a shared alpha, which
# is exactly `ewm_mean(adjust=False)` applied N times — so no recursion is
# needed and the whole thing stays vectorised. Defaults are the indicator's:
# 144-period, 4-pole, 1.414x the filtered true range.
_GC_PERIOD = 144
_GC_POLES = 4
_GC_MULTIPLIER = 1.414

# A SECOND channel, at a shorter period, for reading market state rather than
# picking assets. These are not the same job and they do not want the same
# horizon: 144 days is deliberately slow so that selection is not whipsawed by
# noise, but a regime read at 144 days turns weeks after the market has, which
# for a tilt is the entire cost. Phase 1 measured this - a 48-day read sits at
# the centre of a five-wide plateau and improves the worse of both windows,
# while the 144-day read gives most of it back.
#
# `gc_regime_slope` is the filter's own rate of change, which is what makes the
# tilt proportional to how hard the market is moving rather than a step
# function that treats a drift and a collapse identically.
_GC_REGIME_PERIOD = 48
_GC_REGIME_SLOPE_BARS = 20


def _gc_alpha(period: int, poles: int) -> float:
    beta = (1 - math.cos(2 * math.pi / period)) / (2 ** (1 / poles) - 1)
    return -beta + math.sqrt(beta * beta + 2 * beta)


def _cascade(expr: pl.Expr, alpha: float, poles: int) -> pl.Expr:
    for _ in range(poles):
        expr = expr.ewm_mean(alpha=alpha, adjust=False).over("asset")
    return expr


def _with_gaussian_channel(df: pl.DataFrame) -> pl.DataFrame:
    """Filter, bands, and how long the close has been above the upper band.

    `gc_breakout_age` is 1 on the bar the close first crosses above the upper
    band and counts up while it stays there, null when it is below. That single
    column carries both the state (above/below) and the recency the TR-GC rules
    size on, without the caller having to re-derive a crossing.

    Nulled until `_GC_PERIOD` bars exist. An IIR filter converges rather than
    needing a full window, but its first values are pulled toward the seed, and
    a band computed from a transient is not a band.
    """
    alpha = _gc_alpha(_GC_PERIOD, _GC_POLES)

    prev_close = pl.col("close").shift(1).over("asset")
    true_range = pl.max_horizontal(
        pl.col("high") - pl.col("low"),
        (pl.col("high") - prev_close).abs(),
        (pl.col("low") - prev_close).abs(),
    )

    df = df.with_columns(
        _cascade((pl.col("high") + pl.col("low") + pl.col("close")) / 3, alpha, _GC_POLES)
        .alias("gc_filter"),
        _cascade(true_range, alpha, _GC_POLES).alias("_gc_tr"),
    )
    df = df.with_columns(
        (pl.col("gc_filter") + pl.col("_gc_tr") * _GC_MULTIPLIER).alias("gc_upper"),
        (pl.col("gc_filter") - pl.col("_gc_tr") * _GC_MULTIPLIER).alias("gc_lower"),
    )

    # Warm-up: the filter is not trustworthy before it has seen a full period.
    warm = pl.col("bars_available") >= _GC_PERIOD
    df = df.with_columns(
        [
            pl.when(warm).then(pl.col(c)).otherwise(None).alias(c)
            for c in ("gc_filter", "gc_upper", "gc_lower")
        ]
    )

    above = (pl.col("close") > pl.col("gc_upper")).fill_null(False)
    df = df.with_columns(above.alias("_above"))
    df = df.with_columns(
        (pl.col("_above") & ~pl.col("_above").shift(1).fill_null(False))
        .cum_sum()
        .over("asset")
        .alias("_run")
    )
    df = df.with_columns(
        pl.when(pl.col("_above"))
        .then(pl.col("ts_utc").cum_count().over(["asset", "_run"]))
        .otherwise(None)
        .alias("gc_breakout_age")
    ).drop(["_gc_tr", "_above", "_run"])

    # The faster regime channel. Same construction, shorter period; computed for
    # every asset because it is vectorised and costs nothing, though only the
    # benchmark's is read.
    ra = _gc_alpha(_GC_REGIME_PERIOD, _GC_POLES)
    df = df.with_columns(
        _cascade((pl.col("high") + pl.col("low") + pl.col("close")) / 3, ra, _GC_POLES)
        .alias("gc_regime_filter"),
        _cascade(true_range, ra, _GC_POLES).alias("_gcr_tr"),
    )
    df = df.with_columns(
        (pl.col("gc_regime_filter") + pl.col("_gcr_tr") * _GC_MULTIPLIER).alias(
            "gc_regime_upper"
        ),
        (
            pl.col("gc_regime_filter")
            / pl.col("gc_regime_filter").shift(_GC_REGIME_SLOPE_BARS).over("asset")
            - 1
        ).alias("gc_regime_slope"),
    )
    rwarm = pl.col("bars_available") >= _GC_REGIME_PERIOD
    return df.with_columns(
        [
            pl.when(rwarm).then(pl.col(c)).otherwise(None).alias(c)
            for c in ("gc_regime_filter", "gc_regime_upper", "gc_regime_slope")
        ]
    ).drop("_gcr_tr")


def _with_beta(df: pl.DataFrame, benchmark: str | None) -> pl.DataFrame:
    """Rolling beta of each asset's returns against the benchmark's.

    `cov(r, r_b) / var(r_b)` over a trailing window - the ordinary univariate
    regression slope, computed directly rather than fitted, because with one
    regressor the two are the same number and the closed form has no solver to
    fail.

    Causal by construction: the window is trailing, and the benchmark return
    joined onto row *t* is the benchmark's return *for that same bar*, which was
    knowable at the same moment the asset's own return was. No shift is needed
    and adding one would introduce the off-by-one this codebase is most
    concerned about.
    """
    if benchmark is None or benchmark not in df["asset"].unique().to_list():
        return df.with_columns(pl.lit(None, dtype=pl.Float64).alias("beta_bench"))

    bench = (
        df.filter(pl.col("asset") == benchmark)
        .select(["ts_utc", pl.col("ret_1").alias("_ret_bench")])
        .unique(subset=["ts_utc"], keep="first")
    )

    joined = df.join(bench, on="ts_utc", how="left").sort(["asset", "ts_utc"])

    covariance = pl.rolling_cov(
        "ret_1", "_ret_bench", window_size=_BETA_WINDOW, min_samples=_BETA_WINDOW
    ).over("asset")
    variance = (
        pl.col("_ret_bench")
        .rolling_var(window_size=_BETA_WINDOW, min_samples=_BETA_WINDOW)
        .over("asset")
    )

    return joined.with_columns(
        # A benchmark that did not move over the whole window has no beta to
        # measure against it. Null, not a division by zero and not a silent 0.
        pl.when(variance > 0)
        .then(covariance / variance)
        .otherwise(None)
        .alias("beta_bench")
    ).drop("_ret_bench")


def latest(features: pl.DataFrame) -> pl.DataFrame:
    """The most recent row per asset - the cross-section a decision is made on."""
    if features.is_empty():
        return features
    return (
        features.sort(["asset", "ts_utc"])
        .group_by("asset", maintain_order=True)
        .last()
    )
