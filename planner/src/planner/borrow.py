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

**The data is Binance USD-M**, not Hyperliquid. Listing dates and funding rates
differ between venues, so every funding figure downstream is an estimate from the
wrong venue until a Hyperliquid pull replaces it.
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
