"""Information coefficient: does the score rank assets better than chance?

Design spec §7.5, which argues this is roughly **30× more informative per unit
of calendar time** than portfolio P&L and is the thing that should have been
measured first.

The arithmetic behind that claim: portfolio return yields one observation per
rebalance — 253 over five years here. Rank correlation between the score and the
subsequent return yields one observation *per asset per period*: ~30 names ×
253 periods is ~7,500. Same calendar, thirty times the evidence, and it answers
the question that actually matters rather than a question downstream of it.

## Why this splits the Phase 1 result cleanly

A losing backtest has two very different causes and they need different work:

- **IC ≈ 0** — the ranking has no content. Sweeping holding counts, cadences or
  constructors is tuning the packaging of noise.
- **IC > 0 but the portfolio loses** — the ranking works and *construction*
  destroys it: sizing, turnover, costs, or a constraint set that vetoes the good
  periods. Tractable, and a completely different investigation.

Running the sweeps before knowing which is a category error.

## Causality

Two things could leak and neither does:

**Scores are point-in-time.** Features are built once over full history and
sliced per period, which is legitimate *only* because they are prefix-invariant
— build over everything, rebuild over every prefix, row *i* is identical. That
is tested in `test_features.py`, and it is what makes the shortcut sound rather
than convenient.

**Forward returns are measured where a trade could have happened.** Entry and
exit both use `mark_open` — the bar that opens at the decision timestamp, the
one the planner excluded as still forming — so the return is over prices the
system could actually have transacted at, on the same convention the backtest
fills at.

## What is deliberately *not* dropped

An asset that delists between *T* and *T+h* keeps its return to the last price
it traded at. Dropping it would quietly restore survivorship at the level of the
measurement rather than the universe — and for a momentum signal, the assets
that vanish are disproportionately the ones it had just bought.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from decimal import Decimal
from pathlib import Path

import polars as pl

from . import features, store, universe
from .bars import mark_discontinuities
from .config import Config

#: Below this many assets a cross-sectional rank correlation is noise. Same
#: reasoning as `scores.DEFAULT_MIN_GROUP_SIZE`, one size up: a correlation over
#: five points is not a measurement.
MIN_CROSS_SECTION = 10


@dataclass(frozen=True)
class PeriodIC:
    as_of: datetime
    n_assets: int
    ic: float


@dataclass(frozen=True)
class ICResult:
    """One horizon's answer."""

    horizon_days: int
    step_days: int
    periods: list[PeriodIC]
    disclosures: list[str] = field(default_factory=list)

    @property
    def n_periods(self) -> int:
        return len(self.periods)

    @property
    def n_observations(self) -> int:
        return sum(p.n_assets for p in self.periods)

    @property
    def mean_ic(self) -> float:
        return sum(p.ic for p in self.periods) / self.n_periods if self.periods else 0.0

    @property
    def std_ic(self) -> float:
        if self.n_periods < 2:
            return 0.0
        mean = self.mean_ic
        return math.sqrt(sum((p.ic - mean) ** 2 for p in self.periods) / (self.n_periods - 1))

    @property
    def overlap(self) -> float:
        """How many times each forward return is reused.

        Sampling a 30-day forward return every 7 days means consecutive
        observations share ~77% of their window. They are not independent, and
        treating them as though they were is the standard way an overlapping-
        returns study manufactures significance.
        """
        return max(1.0, self.horizon_days / self.step_days)

    @property
    def effective_n(self) -> float:
        """Periods, deflated for overlap. The denominator the t-stat deserves."""
        return self.n_periods / self.overlap

    @property
    def t_stat(self) -> float:
        """`mean / (std / sqrt(effective_n))` over periods.

        Two deflations, both of which cut the other way from flattering:

        **Periods, not assets.** Within one cross-section the assets are heavily
        correlated, so counting each as independent would inflate this by
        roughly `sqrt(n_assets)` - about 6x here - and turn noise into a result.

        **Effective periods, not raw ones.** Overlapping forward windows reuse
        the same price action, so the honest denominator is `n / overlap`.
        """
        if self.std_ic == 0 or self.n_periods < 2:
            return 0.0
        return self.mean_ic / (self.std_ic / math.sqrt(self.effective_n))

    @property
    def hit_rate(self) -> float:
        """Fraction of periods with a positive IC. 0.5 is a coin."""
        if not self.periods:
            return 0.0
        return sum(1 for p in self.periods if p.ic > 0) / self.n_periods

    @property
    def distinguishable_from_zero(self) -> bool:
        """|t| > 2. A conventional line, and a weak claim even when it passes."""
        return abs(self.t_stat) > 2.0


def spearman(xs: list[float], ys: list[float]) -> float | None:
    """Rank correlation. Pearson over ranks, ties averaged.

    Rank rather than level because the claim being tested is about *ordering*.
    A signal that gets the order right and the magnitude wrong is a usable
    signal; one that correlates in levels because both series trend is not.
    """
    n = len(xs)
    if n < 2:
        return None

    rx, ry = _ranks(xs), _ranks(ys)
    mx, my = sum(rx) / n, sum(ry) / n
    cov = sum((a - mx) * (b - my) for a, b in zip(rx, ry))
    vx = sum((a - mx) ** 2 for a in rx)
    vy = sum((b - my) ** 2 for b in ry)
    if vx == 0 or vy == 0:
        return None  # no dispersion on one side; nothing to correlate
    return cov / math.sqrt(vx * vy)


def _ranks(values: list[float]) -> list[float]:
    order = sorted(range(len(values)), key=lambda i: values[i])
    ranks = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        shared = (i + j) / 2 + 1
        for k in range(i, j + 1):
            ranks[order[k]] = shared
        i = j + 1
    return ranks


def measure(
    *,
    config: Config,
    start: datetime,
    end: datetime,
    data_root: Path,
    score_column: str = "ret_30_skip_7",
    horizons_days: tuple[int, ...] = (7, 14, 30),
) -> list[ICResult]:
    """IC of `score_column` at each horizon, over the same decisions the gate ran.

    The universe at each date comes from its recorded snapshot, so this measures
    the signal over exactly the assets the strategy could have chosen from — not
    over everything that has bars.
    """
    bars = store.read(root=data_root, interval_s=config.interval_s)
    if bars.is_empty():
        raise ValueError("no bars in the store")

    # Prefix-invariance is what makes building once and slicing legitimate.
    frame = features.build(bars, benchmark=config.benchmark)
    if score_column not in frame.columns:
        raise ValueError(f"no such feature {score_column!r}")

    prices = (
        mark_discontinuities(bars)
        .select(["asset", "ts_utc", "mark_open"])
        .sort(["asset", "ts_utc"])
    )

    step = timedelta(seconds=config.interval_s * max(1, config.rebalance_every))
    results: list[ICResult] = []

    for horizon in horizons_days:
        periods: list[PeriodIC] = []
        thin = 0
        missing_snapshot = 0

        as_of = start
        while as_of <= end:
            try:
                members = universe.load(as_of, root=data_root)
            except FileNotFoundError:
                missing_snapshot += 1
                as_of += step
                continue

            eligible = {m.asset for m in members if m.eligible}
            horizon_ts = as_of - timedelta(seconds=config.interval_s)

            cross = (
                frame.filter(
                    (pl.col("ts_utc") == horizon_ts) & pl.col("asset").is_in(list(eligible))
                )
                .filter(
                    (pl.col("bars_available") >= config.min_history_bars)
                    & pl.col("adv_quote").is_not_null()
                    & (pl.col("adv_quote") >= float(config.min_dollar_volume))
                    & pl.col("vol_30").is_not_null()
                    & (pl.col("vol_30") >= float(config.min_volatility))
                    & pl.col(score_column).is_not_null()
                )
                .select(["asset", score_column])
            )

            if cross.height < MIN_CROSS_SECTION:
                thin += 1
                as_of += step
                continue

            forward = _forward_returns(prices, cross["asset"].to_list(), as_of, horizon)
            paired = cross.join(forward, on="asset", how="inner").drop_nulls()

            if paired.height >= MIN_CROSS_SECTION:
                rho = spearman(
                    paired[score_column].to_list(), paired["forward_return"].to_list()
                )
                if rho is not None:
                    periods.append(PeriodIC(as_of=as_of, n_assets=paired.height, ic=rho))
            else:
                thin += 1

            as_of += step

        disclosures = []
        if missing_snapshot:
            disclosures.append(
                f"{missing_snapshot} date(s) had no universe snapshot and were skipped"
            )
        if thin:
            disclosures.append(
                f"{thin} date(s) had fewer than {MIN_CROSS_SECTION} rankable assets; "
                "a rank correlation over a handful of points is not a measurement"
            )
        if horizon > step.days:
            disclosures.append(
                f"{horizon}d forward returns sampled every {step.days}d overlap "
                f"{horizon / step.days:.1f}x; the t-stat below is computed on the "
                "deflated effective sample, not on the raw period count"
            )
        results.append(
            ICResult(
                horizon_days=horizon,
                step_days=max(1, step.days),
                periods=periods,
                disclosures=disclosures,
            )
        )

    return results


def _forward_returns(
    prices: pl.DataFrame, assets: list[str], as_of: datetime, horizon_days: int
) -> pl.DataFrame:
    """Return from the bar opening at `as_of` to the one `horizon_days` later.

    Both legs are `mark_open`: the price a trade could have been done at, on the
    same convention the backtest fills at. An asset that stops trading in
    between keeps its return to its last actual price — dropping it would
    restore survivorship at the level of the measurement.
    """
    exit_ts = as_of + timedelta(days=horizon_days)
    window = prices.filter(pl.col("asset").is_in(assets))

    entry = (
        window.filter(pl.col("ts_utc") <= as_of)
        .group_by("asset")
        .agg(pl.col("mark_open").last().alias("entry"), pl.col("ts_utc").last().alias("entry_ts"))
    )
    exit_ = (
        window.filter(pl.col("ts_utc") <= exit_ts)
        .group_by("asset")
        .agg(pl.col("mark_open").last().alias("exit"))
    )

    joined = entry.join(exit_, on="asset", how="inner")
    return joined.filter(
        # The entry bar must actually be at `as_of`; an asset whose newest bar
        # predates the decision was not tradeable then.
        (pl.col("entry_ts") == as_of) & (pl.col("entry") > 0)
    ).select(
        "asset", (pl.col("exit") / pl.col("entry") - 1).alias("forward_return")
    )


def format_results(results: list[ICResult], *, score_column: str) -> str:
    """Disclosures first, then the numbers, then what they do not license."""
    lines = ["DISCLOSURES (read before any number below)"]
    seen = set()
    for r in results:
        for d in r.disclosures:
            if d not in seen:
                seen.add(d)
                lines.append(f"  ! {d}")
    lines.append(
        "  ! IC is measured on the signal, not the portfolio. A positive IC does "
        "not mean a strategy built on it makes money after costs - that is what "
        "the gate is for."
    )
    lines.append(
        "  ! periods are the independent unit, not assets. Assets within one "
        "cross-section are heavily correlated, so the t-stat below uses n_periods."
    )
    lines.append("")
    lines.append(f"score: {score_column}")
    lines.append("")
    lines.append(
        f"{'horizon':<10}{'periods':>9}{'eff n':>8}{'obs':>8}{'mean IC':>10}"
        f"{'std':>8}{'t-stat':>9}{'hit rate':>10}   verdict"
    )
    for r in results:
        verdict = (
            "distinguishable from zero"
            if r.distinguishable_from_zero
            else "NOT distinguishable from zero"
        )
        lines.append(
            f"{str(r.horizon_days) + 'd':<10}{r.n_periods:>9}{r.effective_n:>8.0f}"
            f"{r.n_observations:>8}{r.mean_ic:>10.4f}{r.std_ic:>8.4f}{r.t_stat:>9.2f}"
            f"{r.hit_rate * 100:>9.1f}%   {verdict}"
        )
    return "\n".join(lines)
