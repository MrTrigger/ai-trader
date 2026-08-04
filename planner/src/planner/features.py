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

import polars as pl

FEATURE_SET_VERSION = "fs-phase1-1"

# Windows in bars. At the daily interval these are calendar days.
_RETURN_WINDOWS = (7, 30, 90)
_VOL_WINDOW = 30
_ADV_WINDOW = 20

# Beta wants a longer window than vol: it is a two-series estimate, so it needs
# more observations to say anything, and a noisy short-window beta is worse than
# no beta at all. A beta of 0.4 estimated on 60 days would *relax* the section
# 6.2 constraint on the strength of an estimate that cannot support it, whereas
# a null falls back to 1.0 and can only make the limit bind harder.
_BETA_WINDOW = 90


def build(bars: pl.DataFrame, *, benchmark: str | None = None) -> pl.DataFrame:
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

    df = df.with_columns(exprs)

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

    df = df.with_columns(
        pl.col("ts_utc").cum_count().over("asset").alias("bars_available")
    ).drop("_asset_rows")

    return _with_beta(df, benchmark)


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
