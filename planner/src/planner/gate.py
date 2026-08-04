"""The Phase 1 gate (design spec §9), run and reported as evidence.

    positive expectancy after costs; **survives 2x slippage**; walk-forward
    beats baseline out of sample; sample size adequate or the run says so.

Four criteria, evaluated independently and reported whether or not they pass.
This module has no opinion about the strategy and deliberately no way to express
one: it computes, it compares against a baseline, and it prints. A gate that can
be argued with is not a gate.

**The baseline is the null hypothesis, not a formality.** `xs_momentum` has to
beat holding everything eligible — if the ranking does nothing, the extra
turnover is pure cost and the honest conclusion is to delete the ranking.

**Beta is reported next to every result** because the most likely way a
long-only crypto momentum book "works" is by being a leveraged BTC position, and
an attribution that ignores that will call it alpha (§6.2).
"""

from __future__ import annotations

from dataclasses import dataclass, field, replace
from datetime import datetime
from decimal import Decimal
from pathlib import Path

from . import backtest, validate
from .config import Config


@dataclass(frozen=True)
class Criterion:
    name: str
    passed: bool
    detail: str

    def __str__(self) -> str:
        return f"  [{'PASS' if self.passed else 'FAIL'}] {self.name:<34} {self.detail}"


@dataclass(frozen=True)
class GateResult:
    candidate: str
    baseline: str
    criteria: list[Criterion]
    disclosures: list[str] = field(default_factory=list)
    candidate_metrics: backtest.Metrics | None = None
    baseline_metrics: backtest.Metrics | None = None
    stressed_metrics: backtest.Metrics | None = None
    walk: validate.WalkForward | None = None
    holdout: validate.Holdout | None = None

    @property
    def passed(self) -> bool:
        return bool(self.criteria) and all(c.passed for c in self.criteria)


def run(
    *,
    config: Config,
    start: datetime,
    end: datetime,
    data_root: Path,
    initial_cash: Decimal,
    baseline_signal: str = "liquidity_top",
    baseline_constructor: str = "equal_weight",
) -> GateResult:
    """Replay the candidate and the baseline, then evaluate the four criteria."""
    candidate = backtest.replay(
        config=config,
        start=start,
        end=end,
        data_root=data_root,
        initial_cash=initial_cash,
    )
    stressed = backtest.replay(
        config=config,
        start=start,
        end=end,
        data_root=data_root,
        initial_cash=initial_cash,
        slippage_multiple=Decimal(2),
    )
    baseline = backtest.replay(
        config=replace(config, signal=baseline_signal, constructor=baseline_constructor),
        start=start,
        end=end,
        data_root=data_root,
        initial_cash=initial_cash,
    )

    walk = validate.walk_forward(candidate, interval_s=config.interval_s, folds=4)
    baseline_walk = validate.walk_forward(baseline, interval_s=config.interval_s, folds=4)
    split = validate.holdout(candidate, interval_s=config.interval_s)

    criteria = [
        Criterion(
            name="positive expectancy after costs",
            passed=candidate.metrics.total_return > 0,
            detail=(
                f"{float(candidate.metrics.total_return) * 100:+.2f}% over "
                f"{candidate.metrics.n} rebalances, "
                f"{float(candidate.metrics.cost_drag_bps):.1f}bps of cost drag"
            ),
        ),
        Criterion(
            name="survives 2x slippage",
            passed=stressed.metrics.total_return > 0,
            detail=(
                f"{float(stressed.metrics.total_return) * 100:+.2f}% at 2x "
                f"(vs {float(candidate.metrics.total_return) * 100:+.2f}% at 1x)"
            ),
        ),
        Criterion(
            name="walk-forward beats the baseline",
            passed=_beats(walk, baseline_walk),
            detail=(
                f"{walk.positive_folds}/{len(walk.folds)} candidate folds positive "
                f"vs {baseline_walk.positive_folds}/{len(baseline_walk.folds)} baseline; "
                f"out-of-sample {_oos(walk):+.2f}% vs {_oos(baseline_walk):+.2f}%"
            ),
        ),
        Criterion(
            name="sample adequate, or it says so",
            passed=not candidate.metrics.insufficient_sample,
            detail=(
                f"n={candidate.metrics.n} rebalances "
                f"(floor {backtest.INSUFFICIENT_SAMPLE_N})"
            ),
        ),
    ]

    disclosures = list(candidate.disclosures)
    if not walk.folds:
        disclosures.append(
            "walk-forward produced no folds: too few rebalances to split. The "
            "out-of-sample criterion above is reporting nothing, not passing."
        )
    if not split.consistent:
        disclosures.append(
            f"holdout: train {float(split.train.metrics.total_return) * 100:+.2f}%, "
            f"test {float(split.test.metrics.total_return) * 100:+.2f}%. The sign did "
            "not hold out of sample."
        )
    disclosures.append(
        "this gate measures the portfolio, not the signal. §7.5 argues the "
        "information coefficient - rank correlation between the score and "
        "subsequent returns, one observation per asset per period - is roughly "
        "30x more informative per unit of calendar time, and is not computed here."
    )

    return GateResult(
        candidate=f"{config.signal} + {config.constructor}",
        baseline=f"{baseline_signal} + {baseline_constructor}",
        criteria=criteria,
        disclosures=disclosures,
        candidate_metrics=candidate.metrics,
        baseline_metrics=baseline.metrics,
        stressed_metrics=stressed.metrics,
        walk=walk,
        holdout=split,
    )


def _oos(walk: validate.WalkForward) -> float:
    """Mean out-of-sample return across folds, in percent."""
    if not walk.folds:
        return 0.0
    return (
        sum(float(f.test.metrics.total_return) for f in walk.folds) / len(walk.folds) * 100
    )


def _beats(candidate: validate.WalkForward, baseline: validate.WalkForward) -> bool:
    """Candidate must be consistent AND better out of sample than the baseline.

    Both halves matter. Consistent-but-worse means the ranking cost turnover and
    bought nothing; better-but-inconsistent means one fold carried the result.
    """
    return bool(candidate.consistent) and _oos(candidate) > _oos(baseline)


def format_result(result: GateResult) -> str:
    """Disclosures first, then the verdict, then the numbers behind it."""
    lines = ["DISCLOSURES (read before any number below)"]
    lines += [f"  ! {d}" for d in result.disclosures]
    lines.append("")
    lines.append(f"candidate  {result.candidate}")
    lines.append(f"baseline   {result.baseline}")
    lines.append("")
    lines.append(f"PHASE 1 GATE: {'PASSED' if result.passed else 'NOT PASSED'}")
    lines += [str(c) for c in result.criteria]

    if result.candidate_metrics and result.baseline_metrics:
        lines.append("")
        lines.append(
            f"{'':<12}{'n':>6}{'return':>10}{'CAGR':>9}{'vol':>8}"
            f"{'Sharpe':>8}{'maxDD':>9}{'turnover':>10}{'cost bps':>10}"
        )
        for label, m in (
            ("candidate", result.candidate_metrics),
            ("at 2x slip", result.stressed_metrics),
            ("baseline", result.baseline_metrics),
        ):
            if m is None:
                continue
            lines.append(
                f"{label:<12}{m.n:>6}{float(m.total_return) * 100:>9.2f}%"
                f"{m.cagr * 100:>8.2f}%{m.volatility * 100:>7.1f}%{m.sharpe:>8.2f}"
                f"{float(m.max_drawdown) * 100:>8.2f}%"
                f"{float(m.turnover_per_rebalance) * 100:>9.2f}%"
                f"{float(m.cost_drag_bps):>10.1f}"
            )

    if result.walk and result.walk.folds:
        lines.append("")
        lines.append("WALK-FORWARD (out of sample)")
        for fold in result.walk.folds:
            lines.append(f"  {fold.test}")

    if not result.passed:
        lines.append("")
        lines.append(
            "A failed gate is the system working. §9: if Phase 1's gate fails, the "
            "correct outcome is a different strategy - or none - not a softer gate."
        )
    return "\n".join(lines)
