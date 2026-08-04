"""Validation: holdout, walk-forward, and one-axis sweeps (design spec §2.2, §9).

The discipline here is inherited from `trading-journal/backtest`; none of the
code is (§2.1). Its splits are by trading session and its metric is an
R-multiple per discrete trade, and neither survives translation to a
continuously rebalanced book. What survives is the reasoning, which is the
valuable half anyway.

Three rules, all of which make the harness weaker and the conclusions stronger:

**Split by date, and apply splits to results rather than to inputs.** Slicing
bars to a window and replaying the slice would start every window flat, with a
cold feature frame and no positions — so each window would be evaluated under
different conditions than the others. Replaying once and attributing steps
afterwards keeps every date identical to how it would have been traded in
sequence.

**One axis at a time. No grid search.** With ~25–50 semi-independent periods a
year (§7.5), a grid over five parameters at five values each is 3125 fits
against a couple of hundred observations, and the best cell of that grid is
noise with a decimal point.

**Report the plateau's centre, never the peak.** A strategy that works at
exactly 30 days and dies at 25 and 35 has found an artefact of this particular
history. A broad shelf is weak evidence of something structural. So a sweep
returns the widest contiguous run of values that all hold up, reports the value
at its centre, and flags a width-1 "plateau" as what it is — a peak.
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from datetime import date, datetime, timedelta
from decimal import Decimal
from pathlib import Path
from typing import Callable, Sequence

from . import backtest
from .config import Config


@dataclass(frozen=True)
class Window:
    """A named set of dates, and the metrics of the steps that fell in it."""

    name: str
    dates: tuple[date, ...]
    metrics: backtest.Metrics

    @property
    def span(self) -> str:
        if not self.dates:
            return "empty"
        return f"{self.dates[0]} .. {self.dates[-1]}"

    def __str__(self) -> str:
        flag = "  [insufficient_sample]" if self.metrics.insufficient_sample else ""
        return (
            f"{self.name:<12} {self.span}  n={self.metrics.n:<4d} "
            f"ret={float(self.metrics.total_return) * 100:+.2f}%  "
            f"sharpe={self.metrics.sharpe:+.2f}{flag}"
        )


@dataclass(frozen=True)
class Holdout:
    train: Window
    test: Window

    @property
    def consistent(self) -> bool:
        """Whether the sign of the result held out of sample.

        Deliberately not "did test beat train": a strategy that does *better*
        out of sample is usually a smaller sample, not a better strategy.
        """
        return (
            self.train.metrics.total_return > 0 and self.test.metrics.total_return > 0
        )


@dataclass(frozen=True)
class WalkForward:
    folds: tuple[Holdout, ...]

    @property
    def positive_folds(self) -> int:
        return sum(1 for f in self.folds if f.test.metrics.total_return > 0)

    @property
    def consistent(self) -> bool:
        """A majority of out-of-sample folds positive.

        A weak criterion on purpose. With this few folds, demanding all of them
        would be demanding the strategy pass a coin-flip test at a confidence
        the sample cannot support in either direction.
        """
        return self.folds and self.positive_folds * 2 > len(self.folds)


@dataclass(frozen=True)
class SweepPoint:
    value: object
    metrics: backtest.Metrics
    holds_up: bool


@dataclass(frozen=True)
class Plateau:
    """The widest contiguous run of values that all held up."""

    axis: str
    values: tuple[object, ...]
    centre: object | None

    @property
    def width(self) -> int:
        return len(self.values)

    @property
    def is_a_peak(self) -> bool:
        """A plateau of one is a peak wearing a plateau's name."""
        return self.width == 1

    def __str__(self) -> str:
        if not self.values:
            return f"{self.axis}: nothing held up"
        note = "  <- a PEAK, not a plateau" if self.is_a_peak else ""
        return (
            f"{self.axis}: plateau {self.values[0]}..{self.values[-1]} "
            f"(width {self.width}), centre {self.centre}{note}"
        )


@dataclass(frozen=True)
class Sweep:
    axis: str
    points: tuple[SweepPoint, ...]
    plateau: Plateau
    disclosures: list[str] = field(default_factory=list)


# --- splitting -------------------------------------------------------------


def _window(name: str, steps: Sequence[backtest.Step], interval_s: int) -> Window:
    return Window(
        name=name,
        dates=tuple(s.as_of.date() for s in steps),
        metrics=backtest.metrics(list(steps), interval_s=interval_s),
    )


def holdout(result: backtest.Result, *, interval_s: int, train_fraction: float = 0.7) -> Holdout:
    """Split the replayed steps in time order: earlier trains, later tests.

    In time order and never shuffled — a shuffled split lets the test window's
    neighbours leak into training through overlapping feature windows, which is
    lookahead by a slower route.
    """
    if not 0 < train_fraction < 1:
        raise ValueError(f"train_fraction must be in (0, 1), got {train_fraction}")

    steps = sorted(result.steps, key=lambda s: s.as_of)
    cut = int(len(steps) * train_fraction)
    return Holdout(
        train=_window("train", steps[:cut], interval_s),
        test=_window("test", steps[cut:], interval_s),
    )


def walk_forward(
    result: backtest.Result, *, interval_s: int, folds: int = 4, train_fraction: float = 0.6
) -> WalkForward:
    """Rolling train/test folds over the replayed steps.

    Each fold trains on a contiguous block and tests on the block immediately
    after it, so the test window is always *later* than what preceded it. The
    folds overlap in training data and not in test data, which is what makes
    the out-of-sample results countable.
    """
    if folds < 1:
        raise ValueError(f"folds must be positive, got {folds}")

    steps = sorted(result.steps, key=lambda s: s.as_of)
    if len(steps) < folds * 2:
        return WalkForward(folds=())

    block = len(steps) // folds
    train_size = max(1, int(block * train_fraction))

    out: list[Holdout] = []
    for i in range(folds):
        start = i * block
        end = start + block if i < folds - 1 else len(steps)
        chunk = steps[start:end]
        if len(chunk) < 2:
            continue
        out.append(
            Holdout(
                train=_window(f"fold{i}-train", chunk[:train_size], interval_s),
                test=_window(f"fold{i}-test", chunk[train_size:], interval_s),
            )
        )
    return WalkForward(folds=tuple(out))


# --- sweeping --------------------------------------------------------------


def find_plateau(points: Sequence[SweepPoint], *, axis: str) -> Plateau:
    """The widest contiguous run of values that held up, and its centre.

    Centre, not peak. If a run of five values all hold up, the third is a far
    better estimate of a structural setting than whichever of the five happened
    to score highest on this particular history.
    """
    best: tuple[int, int] = (0, 0)
    run_start: int | None = None

    for i, point in enumerate(list(points) + [SweepPoint(None, points[0].metrics, False)]):
        if point.holds_up and run_start is None:
            run_start = i
        elif not point.holds_up and run_start is not None:
            if i - run_start > best[1] - best[0]:
                best = (run_start, i)
            run_start = None

    values = tuple(p.value for p in points[best[0] : best[1]])
    centre = values[len(values) // 2] if values else None
    return Plateau(axis=axis, values=values, centre=centre)


def sweep_axis(
    *,
    axis: str,
    values: Sequence[object],
    apply: Callable[[Config, object], Config],
    config: Config,
    start: datetime,
    end: datetime,
    data_root: Path,
    initial_cash: Decimal,
    holds_up: Callable[[backtest.Metrics], bool] | None = None,
) -> Sweep:
    """Replay once per value of one axis, holding everything else fixed.

    One axis, deliberately. See the module docstring: a grid over this sample
    size returns the best cell of a noise field.
    """
    survives = holds_up or (lambda m: m.total_return > 0)
    points: list[SweepPoint] = []

    for value in values:
        result = backtest.replay(
            config=apply(config, value),
            start=start,
            end=end,
            data_root=data_root,
            initial_cash=initial_cash,
        )
        points.append(
            SweepPoint(value=value, metrics=result.metrics, holds_up=survives(result.metrics))
        )

    plateau = find_plateau(points, axis=axis)
    disclosures: list[str] = []
    if plateau.is_a_peak:
        disclosures.append(
            f"{axis}: the widest run that held up is one value wide. That is a peak, "
            "not a plateau, and a setting that works at exactly one value and fails "
            "on either side of it is an artefact of this history."
        )
    if not plateau.values:
        disclosures.append(f"{axis}: no value held up anywhere on the swept range.")
    if any(p.metrics.insufficient_sample for p in points):
        disclosures.append(
            f"{axis}: at least one point has an inadequate sample; the plateau is "
            "drawn over results that individually establish nothing."
        )

    return Sweep(axis=axis, points=tuple(points), plateau=plateau, disclosures=disclosures)


# --- the axes worth sweeping -----------------------------------------------


def with_holdings(config: Config, value: object) -> Config:
    return replace(config, max_holdings=int(value))  # type: ignore[arg-type]


def with_turnover_budget(config: Config, value: object) -> Config:
    return replace(config, turnover_budget=Decimal(str(value)))


def with_constructor(config: Config, value: object) -> Config:
    return replace(config, constructor=str(value))


def with_rebalance_every(config: Config, value: object) -> Config:
    """Rebalance frequency (§10.3).

    Not the bar interval: the bars stay daily and the *decision* is taken less
    often. Changing the bar interval instead would change the features too, and
    the sweep would be measuring two things at once.
    """
    return replace(config, rebalance_every=int(value))  # type: ignore[arg-type]
