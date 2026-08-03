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

FEATURE_SET_VERSION = "fs-phase0-1"

# Windows in bars. At the daily interval these are calendar days.
_RETURN_WINDOWS = (7, 30, 90)
_VOL_WINDOW = 30
_ADV_WINDOW = 20


def build(bars: pl.DataFrame) -> pl.DataFrame:
    """Feature frame, one row per (asset, bar).

    The caller is responsible for having already trimmed `bars` to the horizon
    (see `pipeline.usable_bars`). This function does not know what `as_of` is,
    which is deliberate: a function that cannot see the horizon cannot leak
    across it.
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

    return df.with_columns(
        pl.col("ts_utc").cum_count().over("asset").alias("bars_available")
    ).drop("_asset_rows")


def latest(features: pl.DataFrame) -> pl.DataFrame:
    """The most recent row per asset - the cross-section a decision is made on."""
    if features.is_empty():
        return features
    return (
        features.sort(["asset", "ts_utc"])
        .group_by("asset", maintain_order=True)
        .last()
    )
