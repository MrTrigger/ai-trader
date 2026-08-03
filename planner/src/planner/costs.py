"""Transaction cost estimation.

Three components in basis points of traded notional:

    commission   what the venue charges
    spread       the half-spread crossed to get filled
    impact       coefficient * sqrt(notional / ADV) * daily_vol_bps

The square root is the standard concave form: doubling size costs less than
twice as much, because the order walks a book that deepens. The coefficient is
the one number here that cannot be assumed - it has to be fitted against
realised fills, and until it is, every estimate this module produces carries an
unquantified error. `CostModel.calibrated` records which of those two worlds we
are in, and an uncalibrated model attaches a warning to every plan it prices.

Where costs bind (design spec section 6.1): in a constructor's objective, so it
declines a trade on its merits. `equal_weight` has no objective, so at Phase 0
they bind in the rebalance deadband instead - a drift worth less than a multiple
of its round-trip cost is not worth crossing a spread to correct.
"""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal

from .config import CostModel

_BPS = Decimal(10_000)


@dataclass(frozen=True)
class AssetCostEstimate:
    asset: str
    notional: Decimal
    spread_bps: Decimal
    commission_bps: Decimal
    impact_bps: Decimal

    @property
    def total_bps(self) -> Decimal:
        return self.spread_bps + self.commission_bps + self.impact_bps

    @property
    def total_quote(self) -> Decimal:
        return self.notional * self.total_bps / _BPS

    @property
    def round_trip_bps(self) -> Decimal:
        """Cost of getting in and back out - what a rebalance actually risks."""
        return self.total_bps * 2


def estimate_one(
    *,
    asset: str,
    notional: Decimal,
    adv_quote: Decimal | None,
    daily_vol: Decimal | None,
    model: CostModel,
) -> AssetCostEstimate:
    """Cost of trading `notional` of `asset`.

    Missing liquidity or volatility data does not silently become zero impact.
    An unknown-liquidity asset is treated as the *most* impactful case the model
    can express, because the alternative - assuming it is free - is how an
    illiquid position gets sized as though it were a major.
    """
    impact_bps = Decimal(0)
    if notional > 0:
        if adv_quote is None or adv_quote <= 0 or daily_vol is None or daily_vol <= 0:
            # No basis for an estimate. Charge the full daily move as impact:
            # deliberately punitive, so an unpriceable asset loses to a
            # priceable one rather than winning by having no known cost.
            impact_bps = _BPS
        else:
            participation = notional / adv_quote
            impact_bps = (
                model.impact_coefficient
                * Decimal(str(float(participation) ** 0.5))
                * daily_vol
                * _BPS
            )

    return AssetCostEstimate(
        asset=asset,
        notional=notional,
        spread_bps=model.spread_bps,
        commission_bps=model.commission_bps,
        impact_bps=impact_bps,
    )


def estimate(
    trades: dict[str, Decimal],
    *,
    adv: dict[str, Decimal | None],
    vol: dict[str, Decimal | None],
    model: CostModel,
) -> list[AssetCostEstimate]:
    """Cost per asset for a set of trade notionals (absolute values)."""
    return [
        estimate_one(
            asset=asset,
            notional=abs(notional),
            adv_quote=adv.get(asset),
            daily_vol=vol.get(asset),
            model=model,
        )
        for asset, notional in sorted(trades.items())
        if notional != 0
    ]


def total_quote(estimates: list[AssetCostEstimate]) -> Decimal:
    return sum((e.total_quote for e in estimates), Decimal(0))


def total_bps(estimates: list[AssetCostEstimate], *, nav: Decimal) -> Decimal:
    """Total cost as bps **of NAV**, not of traded notional.

    Stated against NAV because that is what it is charged against: a 30 bps cost
    on a 10% rebalance is 3 bps of the fund, and reporting the first number
    where the second belongs overstates the drag by the inverse of turnover.
    """
    if nav <= 0:
        return Decimal(0)
    return total_quote(estimates) / nav * _BPS
