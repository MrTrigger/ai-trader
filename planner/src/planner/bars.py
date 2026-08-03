"""Canonical bar schema and ingest validation.

Every data source normalises to `BAR_SCHEMA`. Nothing downstream ever sees a
source-specific shape.

The load-bearing convention, stated once and enforced everywhere:

    ts_utc is the bar's OPEN time, timezone-aware UTC.

Many exchange APIs stamp a candle with its close time. Ingesting that as an open
time hands every bar a full interval of lookahead - the most damaging bug this
system can have, and a silent one - so the conversion happens in the adapter, at
the boundary, and never again.

Two deliberate divergences from `trading-journal/backtest/schema.py`, which this
otherwise mirrors:

  * `volume` is Float64, not Int64. Futures trade in whole contracts; crypto
    does not. 0.4231 BTC is a legitimate bar volume and rounding it to an
    integer would destroy every liquidity and impact estimate built on it.
  * the key column is `asset` (canonical, venue-independent), not `symbol`.
    The same asset is `BTC-USDT` on one venue and `BTC-USD` on another; the
    mapping lives in the venue adapter and never reaches the store.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta
from enum import Enum

import polars as pl

BAR_SCHEMA: dict[str, pl.DataType] = {
    "ts_utc": pl.Datetime(time_unit="us", time_zone="UTC"),  # bar OPEN
    "asset": pl.String,
    "interval_s": pl.Int32,
    "open": pl.Float64,
    "high": pl.Float64,
    "low": pl.Float64,
    "close": pl.Float64,
    "volume": pl.Float64,        # base-asset units; fractional in crypto
    "quote_volume": pl.Float64,  # nullable; quote-currency turnover where given
    "trades": pl.Int64,          # nullable; tick count where given
}

BAR_COLUMNS = list(BAR_SCHEMA)

# Columns that define a bar's content. Used for hashing and de-duplication;
# deliberately excludes nullable source-dependent extras so the same bars from
# two sources hash identically.
CONTENT_COLUMNS = ("ts_utc", "asset", "interval_s", "open", "high", "low", "close", "volume")


class Severity(str, Enum):
    """ERROR refuses the write. WARNING is recorded in the manifest and kept."""

    ERROR = "error"
    WARNING = "warning"


@dataclass(frozen=True)
class Issue:
    severity: Severity
    code: str
    detail: str
    count: int = 1


def empty_frame() -> pl.DataFrame:
    """An empty frame with exactly the canonical schema."""
    return pl.DataFrame(schema=BAR_SCHEMA)


def conform(df: pl.DataFrame) -> pl.DataFrame:
    """Coerce a frame to the canonical schema: columns present, ordered, typed.

    Missing nullable columns are filled with nulls. A missing required column is
    an error rather than a silent null, because a bar with no close is not a bar
    with an unknown close - it is a broken adapter.
    """
    required = ("ts_utc", "asset", "interval_s", "open", "high", "low", "close", "volume")
    missing = [c for c in required if c not in df.columns]
    if missing:
        raise ValueError(f"source frame is missing required columns: {missing}")

    for col, dtype in BAR_SCHEMA.items():
        if col not in df.columns:
            df = df.with_columns(pl.lit(None).cast(dtype).alias(col))

    return df.select(
        [pl.col(c).cast(dtype) for c, dtype in BAR_SCHEMA.items()]
    ).sort(["asset", "interval_s", "ts_utc"])


def validate(df: pl.DataFrame) -> list[Issue]:
    """Structural checks on conformed bars.

    ERROR-level problems mean the data is wrong, not merely unusual, and refuse
    the write. Gaps and zero-volume bars are warnings: exchanges have outages
    and thin assets have quiet hours, and both are legitimate history.
    """
    issues: list[Issue] = []
    if df.is_empty():
        return issues

    if df["ts_utc"].null_count():
        issues.append(Issue(Severity.ERROR, "null_timestamp", "bars with a null ts_utc"))

    dupes = df.select(["asset", "interval_s", "ts_utc"]).is_duplicated().sum()
    if dupes:
        issues.append(
            Issue(Severity.ERROR, "duplicate_bar", "same asset/interval/timestamp twice", int(dupes))
        )

    bad_range = df.filter(
        (pl.col("high") < pl.col("low"))
        | (pl.col("open") > pl.col("high"))
        | (pl.col("open") < pl.col("low"))
        | (pl.col("close") > pl.col("high"))
        | (pl.col("close") < pl.col("low"))
    ).height
    if bad_range:
        issues.append(
            Issue(Severity.ERROR, "ohlc_inconsistent", "OHLC outside the bar's own range", bad_range)
        )

    nonpositive = df.filter(
        (pl.col("open") <= 0) | (pl.col("high") <= 0) | (pl.col("low") <= 0) | (pl.col("close") <= 0)
    ).height
    if nonpositive:
        issues.append(
            Issue(Severity.ERROR, "nonpositive_price", "a price at or below zero", nonpositive)
        )

    negative_volume = df.filter(pl.col("volume") < 0).height
    if negative_volume:
        issues.append(
            Issue(Severity.ERROR, "negative_volume", "negative volume", negative_volume)
        )

    zero_volume = df.filter(pl.col("volume") == 0).height
    if zero_volume:
        issues.append(
            Issue(Severity.WARNING, "zero_volume", "bars with no trading", zero_volume)
        )

    issues.extend(_gap_issues(df))
    return issues


def _gap_issues(df: pl.DataFrame) -> list[Issue]:
    """Missing bars on a 24/7 grid.

    Crypto never closes, so unlike the futures harness there is no session
    boundary that legitimately breaks the grid. A gap here is an exchange
    outage or an incomplete pull - worth surfacing, not worth refusing.
    """
    issues: list[Issue] = []
    for (asset, interval_s), group in df.group_by(["asset", "interval_s"], maintain_order=True):
        if group.height < 2:
            continue
        step = timedelta(seconds=int(interval_s))
        deltas = group["ts_utc"].sort().diff().drop_nulls()
        gaps = int((deltas > step).sum())
        if gaps:
            issues.append(
                Issue(Severity.WARNING, "gap", f"missing bars for {asset}/{interval_s}s", gaps)
            )
    return issues


def has_errors(issues: list[Issue]) -> bool:
    return any(i.severity is Severity.ERROR for i in issues)
