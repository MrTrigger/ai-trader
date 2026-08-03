"""The risk gate (step 6).

Hard limits, evaluated at plan construction. **A plan that breaches any limit is
rejected whole** - never partially applied, never truncated to fit. Truncating
to fit is how a system ends up holding the first fifteen of twenty intended
positions and calling it a portfolio.

The failure mode this exists to prevent is specific: per-asset signals are
correlated. In a broad rally, twenty assets trigger the same day, and per-asset
sizing with no portfolio cap "wants" a multiple of NAV. Without this gate the
system either over-allocates or dies half-built.

Limits configured as `None` are **not enforced**, and every one of them produces
a disclosure that is reported above the plan's numbers rather than beneath them.
"""

from __future__ import annotations

from decimal import Decimal

from .config import RiskLimits
from .plan import RiskCheck, RiskReport


def evaluate(
    *,
    target_weights: dict[str, Decimal],
    current_weights: dict[str, Decimal],
    limits: RiskLimits,
    nav: Decimal,
) -> RiskReport:
    checks: list[RiskCheck] = []

    gross = sum((abs(w) for w in target_weights.values()), Decimal(0))
    checks.append(
        RiskCheck(
            name="max_gross_exposure",
            limit=limits.max_gross_exposure,
            value=gross,
            passed=gross <= limits.max_gross_exposure,
            detail="sum of |weight| across targets",
        )
    )

    largest = max((abs(w) for w in target_weights.values()), default=Decimal(0))
    checks.append(
        RiskCheck(
            name="max_position",
            limit=limits.max_position,
            value=largest,
            passed=largest <= limits.max_position,
        )
    )

    count = Decimal(len([w for w in target_weights.values() if w != 0]))
    checks.append(
        RiskCheck(
            name="max_position_count",
            limit=Decimal(limits.max_position_count),
            value=count,
            passed=count <= limits.max_position_count,
        )
    )

    # Turnover is one-way: the sum of weight changes, not their round trip.
    assets = set(target_weights) | set(current_weights)
    turnover = sum(
        (
            abs(target_weights.get(a, Decimal(0)) - current_weights.get(a, Decimal(0)))
            for a in assets
        ),
        Decimal(0),
    )
    checks.append(
        RiskCheck(
            name="max_turnover",
            limit=limits.max_turnover,
            value=turnover,
            passed=turnover <= limits.max_turnover,
            detail="sum of |target - current| weight, one way",
        )
    )

    if limits.max_net_exposure is not None:
        net = sum(target_weights.values(), Decimal(0))
        checks.append(
            RiskCheck(
                name="max_net_exposure",
                limit=limits.max_net_exposure,
                value=abs(net),
                passed=abs(net) <= limits.max_net_exposure,
            )
        )

    failed = [c for c in checks if not c.passed]
    reason = (
        None
        if not failed
        else "; ".join(f"{c.name} {c.value} exceeds {c.limit}" for c in failed)
    )
    return RiskReport(checks=checks, rejected_reason=reason)
