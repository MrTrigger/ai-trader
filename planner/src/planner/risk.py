"""The risk gate (step 6).

Hard limits on the **resulting portfolio**, evaluated at plan construction. A
plan that breaches any limit is **rejected whole** - never partially applied,
never truncated to fit. Truncating to fit is how a system ends up holding the
first fifteen of twenty intended positions and calling it a portfolio.

Every limit here is a property of the destination: how concentrated is the book,
how many names, how much gross. Turnover is deliberately *not* here - it
describes the transition, not the destination, and conflating the two made an
initial build from flat unreachable (0% -> 75% invested is 75% turnover, so the
first run of any deployment breached a 50% cap). It is a budget in `diff.py`.

The failure mode this exists to prevent is specific: per-asset signals are
correlated. In a broad rally, twenty assets trigger the same day, and per-asset
sizing with no portfolio cap "wants" a multiple of NAV. Without this gate the
system either over-allocates or dies half-built.

Limits configured as `None` are **not enforced**, and every one of them produces
a disclosure that is reported above the plan's numbers rather than beneath them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from decimal import Decimal

from .config import RiskLimits
from .plan import RiskCheck, RiskReport

# What an asset's beta is assumed to be when it cannot be estimated.
#
# One, not zero, and the direction is the point: for a crypto alt measured
# against BTC, full co-movement is both the sane prior and the conservative one.
# A missing estimate can then only make the section 6.2 constraint bind harder,
# never relax it. Assuming zero would let an unmeasurable book pass a beta limit
# by virtue of being unmeasurable.
UNKNOWN_BETA = Decimal(1)

# The cluster an asset falls into when the configured grouping does not name it.
_UNCLASSIFIED = "_unclassified"


@dataclass(frozen=True)
class RiskEvaluation:
    """The gate's verdict, plus what it could not fully vouch for.

    `disclosures` exists because a limit can be *enforced* and still be weaker
    than it looks - a cluster limit over a grouping that names half the universe
    constrains half the universe. Section 12 says report what was not enforced
    before any number, so the gate has to be able to say it.
    """

    report: RiskReport
    disclosures: list[str] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return self.report.passed


def evaluate(
    *,
    target_weights: dict[str, Decimal],
    current_weights: dict[str, Decimal],
    limits: RiskLimits,
    nav: Decimal,
    clusters: dict[str, str] | None = None,
    betas: dict[str, Decimal | None] | None = None,
) -> RiskEvaluation:
    checks: list[RiskCheck] = []
    disclosures: list[str] = []

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

    if limits.max_cluster_exposure is not None:
        check, cluster_notes = _cluster_check(
            target_weights, clusters or {}, limits.max_cluster_exposure
        )
        checks.append(check)
        disclosures.extend(cluster_notes)

    if limits.max_benchmark_beta is not None:
        check, beta_notes = _beta_check(
            target_weights, betas or {}, limits.max_benchmark_beta
        )
        checks.append(check)
        disclosures.extend(beta_notes)

    failed = [c for c in checks if not c.passed]
    reason = (
        None
        if not failed
        else "; ".join(f"{c.name} {c.value} exceeds {c.limit}" for c in failed)
    )
    return RiskEvaluation(
        report=RiskReport(checks=checks, rejected_reason=reason),
        disclosures=disclosures,
    )


def _cluster_check(
    target_weights: dict[str, Decimal],
    clusters: dict[str, str],
    limit: Decimal,
) -> tuple[RiskCheck, list[str]]:
    """Gross exposure per correlated group (design spec section 6).

    A hundred crypto assets are approximately one asset, and an equal-weight
    book across them is a leveraged beta bet wearing a diversification costume.
    This is the crude version the spec asks for - a configured grouping, not a
    correlation clustering - because crude and present beats sophisticated and
    Phase 5.

    An asset the grouping does not name becomes its own singleton cluster, which
    means it is *unconstrained by this limit*. That is a real weakening and it is
    disclosed by name rather than absorbed: if every asset were unclassified the
    check would pass trivially while appearing to have been enforced, and a
    limit that cannot fail is worse than one that is declared off.
    """
    notes: list[str] = []
    held = {a: w for a, w in target_weights.items() if w != 0}

    unclassified = sorted(a for a in held if a not in clusters)
    if unclassified:
        exposed = sum((abs(held[a]) for a in unclassified), Decimal(0))
        notes.append(
            f"cluster limit does not constrain {', '.join(unclassified)} "
            f"({exposed} gross): no cluster is configured for them, so each is "
            "treated as its own group and the limit binds only within itself"
        )

    gross_by_cluster: dict[str, Decimal] = {}
    for asset, weight in held.items():
        # Unclassified assets get a per-asset key rather than sharing one, or
        # they would be lumped into a single fictitious cluster and could fail
        # the check for being unrelated to each other.
        key = clusters.get(asset, f"{_UNCLASSIFIED}:{asset}")
        gross_by_cluster[key] = gross_by_cluster.get(key, Decimal(0)) + abs(weight)

    if gross_by_cluster:
        worst, value = max(gross_by_cluster.items(), key=lambda kv: (kv[1], kv[0]))
        detail = f"largest cluster {worst.removeprefix(_UNCLASSIFIED + ':')}"
    else:
        worst, value, detail = "", Decimal(0), "no positions"

    return (
        RiskCheck(
            name="max_cluster_exposure",
            limit=limit,
            value=value,
            passed=value <= limit,
            detail=detail,
        ),
        notes,
    )


def _beta_check(
    target_weights: dict[str, Decimal],
    betas: dict[str, Decimal | None],
    limit: Decimal,
) -> tuple[RiskCheck, list[str]]:
    """Portfolio beta against the configured benchmark: `|w'b|`.

    Section 6.2. A long book of alts with no beta constraint is a leveraged BTC
    position wearing a diversification costume, and the attribution will credit
    "alpha" for what was beta in a bull market. Constrain it, then attribute
    against it.
    """
    notes: list[str] = []
    held = {a: w for a, w in target_weights.items() if w != 0}

    assumed = sorted(a for a in held if betas.get(a) is None)
    if assumed:
        notes.append(
            f"beta assumed {UNKNOWN_BETA} for {', '.join(assumed)}: too little "
            "history to estimate one. The assumption is conservative - it can "
            "only tighten this limit - but it is an assumption"
        )

    portfolio_beta = sum(
        (w * (betas.get(a) if betas.get(a) is not None else UNKNOWN_BETA)
         for a, w in held.items()),
        Decimal(0),
    )

    return (
        RiskCheck(
            name="max_benchmark_beta",
            limit=limit,
            value=abs(portfolio_beta),
            passed=abs(portfolio_beta) <= limit,
            detail="|w'beta| against the configured benchmark",
        ),
        notes,
    )
