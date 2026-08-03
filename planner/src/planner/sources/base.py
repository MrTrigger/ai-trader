"""The `DataSource` interface.

Market data is deliberately decoupled from the venue. Public OHLCV needs no
account anywhere, which is what makes the venue decision deferrable at zero cost
(design spec section 10.1) - the strategy can be researched and gated long
before anything is opened.

Consequence worth stating: the data source and the venue are separate choices.
Trading on OKX while pricing from a different public source is normal and fine;
what is *not* fine is silently mixing the two inside one series.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Protocol, runtime_checkable

import polars as pl


@dataclass(frozen=True)
class UniverseMember:
    """One asset's eligibility as of a point in time.

    `reason` records *why* it was eligible or not, so a later question about a
    strange plan has an answer that does not require re-deriving anything.
    """

    asset: str
    rank: int
    eligible: bool
    reason: str


@runtime_checkable
class DataSource(Protocol):
    """Bars and universe membership.

    Implementations normalise to `bars.BAR_SCHEMA` and are responsible for the
    open/close timestamp conversion at their own boundary - see `bars.py`.
    """

    name: str

    def supported_intervals(self) -> list[int]: ...

    def fetch_bars(
        self,
        asset: str,
        *,
        interval_s: int,
        start: datetime,
        end: datetime,
    ) -> pl.DataFrame:
        """Bars in `[start, end)`, conformed to the canonical schema.

        `ts_utc` is the bar OPEN. An implementation whose upstream stamps close
        times converts here and never again.
        """
        ...

    def universe(self, as_of: datetime) -> list[UniverseMember]:
        """Eligible assets as of a point in time.

        **Never backfilled.** A universe reconstructed today and applied to
        history is survivorship bias with extra steps: the delisted, the rugged
        and the dead must remain in the record exactly as they were. Snapshots
        are appended as they are observed, and a backtest reads the snapshot for
        its own date or refuses to run.
        """
        ...
