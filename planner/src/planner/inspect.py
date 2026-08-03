"""Evidence that a source's timestamps mean what we think they mean.

A bar CSV or API response cannot tell you whether its timestamp marks the bar's
OPEN or its CLOSE. Guessing wrong shifts every bar by one interval and hands the
strategy a free look at the future - the most damaging bug available here, and a
silent one. The futures harness solves this for session data by finding where
volume steps up at the cash open; crypto has no session, so that trick does not
transfer.

What does transfer: on a contiguous series, `open[t]` should equal `close[t-1]`,
because the next bar starts where the last one stopped. That identity holds only
under the correct reading. If a source stamps close times and we ingest them as
opens, the series is shifted and the identity breaks at every bar.

This is evidence, not proof - a source could be shifted *and* internally
consistent - but a series that fails it is definitely wrong, and that is worth
knowing before any of it reaches a strategy.
"""

from __future__ import annotations

from dataclasses import dataclass

import polars as pl


@dataclass(frozen=True)
class ContinuityReport:
    asset: str
    interval_s: int
    checked: int
    breaks: int
    worst_rel_gap: float

    @property
    def ok(self) -> bool:
        return self.checked > 0 and self.breaks == 0

    def render(self) -> str:
        if self.checked == 0:
            return f"{self.asset}/{self.interval_s}s: too few contiguous bars to check"
        verdict = "consistent with ts_utc = bar OPEN" if self.ok else "SUSPECT"
        return (
            f"{self.asset}/{self.interval_s}s: {self.checked} adjacent pairs, "
            f"{self.breaks} breaks, worst {self.worst_rel_gap:.2%} - {verdict}"
        )


def continuity(df: pl.DataFrame, *, tolerance: float = 0.005) -> list[ContinuityReport]:
    """Check `open[t] == close[t-1]` on adjacent bars, per asset and interval.

    Only adjacent pairs are compared - a real gap in the series (exchange
    outage, incomplete pull) is not a timestamp problem and must not be counted
    as one.

    `tolerance` is relative. It is not zero because some sources round the two
    fields independently, and a fraction of a basis point of disagreement is
    rounding, not a shift. A genuine one-interval shift produces disagreements
    of whole percent on daily crypto bars, so the two are not close together.
    """
    reports: list[ContinuityReport] = []

    for (asset, interval_s), group in df.sort("ts_utc").group_by(
        ["asset", "interval_s"], maintain_order=True
    ):
        g = group.sort("ts_utc").with_columns(
            prev_close=pl.col("close").shift(1),
            gap_s=pl.col("ts_utc").diff().dt.total_seconds(),
        )
        adjacent = g.filter(
            pl.col("prev_close").is_not_null() & (pl.col("gap_s") == int(interval_s))
        )
        if adjacent.is_empty():
            reports.append(ContinuityReport(str(asset), int(interval_s), 0, 0, 0.0))
            continue

        rel = (
            (pl.col("open") - pl.col("prev_close")).abs() / pl.col("prev_close")
        ).alias("rel")
        scored = adjacent.with_columns(rel)
        reports.append(
            ContinuityReport(
                asset=str(asset),
                interval_s=int(interval_s),
                checked=scored.height,
                breaks=int(scored.filter(pl.col("rel") > tolerance).height),
                worst_rel_gap=float(scored["rel"].max() or 0.0),
            )
        )

    return reports
