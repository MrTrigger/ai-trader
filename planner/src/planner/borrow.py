"""Which assets have a listed perpetual, and from when.

The venue is Hyperliquid, which is perps-first, so **both** legs of the book are
perpetual futures. That makes this one table gate the entire universe rather than
just the short side: an asset with no perp cannot be held long either.

It is a *point-in-time* question and one of the easier places to introduce
look-ahead. The set of assets with perps today is not the set that had them in
2021, and applying today's list over history is the instrument-side equivalent of
a survivorship-biased universe. It reads as free alpha, because the assets that
got listings later are disproportionately the ones that did well.

The honest form is a first-available date per asset, which is what this returns.
Funding history is a faithful record of it, because funding is only ever paid on
a contract that exists.

An asset absent from the table is **never tradeable**, not silently tradeable. A
gap here makes the book trade less, which is the failure direction that costs
opportunity rather than money.

**The data is Binance USD-M**, not Hyperliquid, and that has been checked rather
than waved at. Over the 2023-05 onward overlap Hyperliquid funding runs about
*double* Binance's on the majors (+13.2%/yr against +6.5%/yr, daily correlation
0.716). The proxy survives anyway, for a structural reason rather than a lucky
one: net funding is `ws*f_short - wl*f_long`, so adding a constant to every rate
contributes `gap * (ws - wl)`, which is the gap times minus the net exposure.
This book's mean signed net exposure is -0.03x NAV, so a level shift cancels
between the legs - worth about +1.3% across the whole run.

Two limits stand. Only five majors were sampled, and the book trades alts. And
**Hyperliquid did not exist before 2023-05-12**, which is 53% of the backtest
window, so for the early period there is no substitution to make at all.
"""

from __future__ import annotations

from datetime import date
from pathlib import Path

import polars as pl

from .store import DEFAULT_ROOT

#: Where funding history lands. One file per venue; every file contributes, and
#: the earliest date across them wins, because a borrow anywhere is a borrow.
FUNDING_DIR = "funding"


def listings(*, root: Path = DEFAULT_ROOT) -> dict[str, date]:
    """First date each asset could be shorted, from recorded funding history.

    Returns an empty mapping when no funding data has been pulled, which makes
    every short unavailable rather than raising. That is the right default for a
    system whose long-only path must keep working without this file.
    """
    d = Path(root) / FUNDING_DIR
    if not d.is_dir():
        return {}

    first: dict[str, date] = {}
    for path in sorted(d.glob("*.parquet")):
        frame = pl.read_parquet(path, columns=["asset", "day"])
        if frame.is_empty():
            continue
        earliest = frame.group_by("asset").agg(pl.col("day").min().alias("first"))
        for asset, day in earliest.iter_rows():
            seen = first.get(asset)
            when = day.date()
            if seen is None or when < seen:
                first[asset] = when
    return first
