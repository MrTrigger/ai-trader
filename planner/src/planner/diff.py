"""Target minus actual (step 7).

Target-state convergence, not imperative order placement. The engine computes
what *should* be true and subtracts what *is* true; re-running after a partial
failure converges rather than duplicating. That property is what makes the whole
run idempotent and crash-safe.

Two rules from design spec section 3.2 that fall out of the diff naturally but
are asserted rather than assumed:

  * **Exits before entries.** Frees capital before entries size against it, so a
    rebalance never sizes off a stale NAV.
  * **No re-entry into an asset exited this run.** Kills churn from a signal
    that flips on stale intermediate state.

The deadband is where transaction costs bind at Phase 0. A drift worth less than
`rebalance_cost_multiple` times its round-trip cost is left alone: correcting it
would pay a certain spread to chase an uncertain improvement.
"""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal

from .costs import AssetCostEstimate, estimate_one
from .config import Config
from .plan import Order

_BPS = Decimal(10_000)

# Order in which reasons are emitted. Exits and reductions free capital, so they
# must precede the entries and increases that consume it.
_REASON_ORDER = {"exit": 0, "reduce": 1, "increase": 2, "entry": 3, "rebalance": 4}


@dataclass(frozen=True)
class Trade:
    asset: str
    delta_weight: Decimal
    delta_notional: Decimal
    qty: Decimal
    reason: str
    cost: AssetCostEstimate


@dataclass(frozen=True)
class DiffResult:
    trades: list[Trade]
    skipped: list[str]

    @property
    def orders(self) -> list[Order]:
        return [
            Order(
                asset=t.asset,
                side="buy" if t.delta_notional > 0 else "sell",
                qty=abs(t.qty),
                order_type="market",
                reason=t.reason,
                est_cost_bps=t.cost.total_bps,
            )
            for t in self.trades
        ]


def _reason(current: Decimal, target: Decimal) -> str:
    if current == 0:
        return "entry"
    if target == 0:
        return "exit"
    if abs(target) > abs(current):
        return "increase"
    return "reduce"


def compute(
    *,
    target_weights: dict[str, Decimal],
    current_weights: dict[str, Decimal],
    prices: dict[str, Decimal],
    adv: dict[str, Decimal | None],
    vol: dict[str, Decimal | None],
    nav: Decimal,
    config: Config,
) -> DiffResult:
    trades: list[Trade] = []
    skipped: list[str] = []

    for asset in sorted(set(target_weights) | set(current_weights)):
        target = target_weights.get(asset, Decimal(0))
        current = current_weights.get(asset, Decimal(0))
        drift = target - current
        if drift == 0:
            continue

        if asset not in prices:
            skipped.append(f"{asset}: no price, cannot size a trade")
            continue

        notional = drift * nav
        cost = estimate_one(
            asset=asset,
            notional=abs(notional),
            adv_quote=adv.get(asset),
            daily_vol=vol.get(asset),
            model=config.costs,
        )
        reason = _reason(current, target)

        # An exit is never suppressed. Getting out is the one action whose
        # value is not measured against its spread: a position we no longer
        # want is a risk we are still carrying.
        if reason != "exit":
            if abs(notional) < config.limits.min_position_notional:
                skipped.append(
                    f"{asset}: {abs(notional):.2f} {config.quote_currency} below the "
                    f"{config.limits.min_position_notional} minimum"
                )
                continue

            drift_value_bps = abs(drift) * _BPS
            threshold = cost.round_trip_bps * config.rebalance_cost_multiple
            if drift_value_bps < threshold:
                skipped.append(
                    f"{asset}: drift {drift_value_bps:.1f}bps under the "
                    f"{threshold:.1f}bps deadband ({config.rebalance_cost_multiple}x round trip)"
                )
                continue

        trades.append(
            Trade(
                asset=asset,
                delta_weight=drift,
                delta_notional=notional,
                qty=notional / prices[asset],
                reason=reason,
                cost=cost,
            )
        )

    trades.sort(key=lambda t: (_REASON_ORDER[t.reason], t.asset))
    _assert_ordering(trades)
    return DiffResult(trades=trades, skipped=skipped)


def _assert_ordering(trades: list[Trade]) -> None:
    """Exits and reductions precede entries and increases.

    Asserted rather than trusted: the sort above is one edit away from being
    reordered by someone who does not know why it is there.
    """
    seen_consuming = False
    for t in trades:
        consuming = t.reason in ("entry", "increase")
        if consuming:
            seen_consuming = True
        elif seen_consuming:
            raise AssertionError(
                f"capital-freeing trade {t.asset}/{t.reason} ordered after a consuming one"
            )
