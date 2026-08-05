"""Portfolio construction (step 5).

Constructors are swappable, but the reason that matters is that they are
*comparable*: the harness scores them against each other on the same signals, so
the choice is settled by out-of-sample evidence rather than by argument. Any
constructor that cannot beat `equal_weight` is deleted, however elegant.

`equal_weight` is the baseline every later constructor must beat. It has no
objective function, so costs cannot enter it - they bind in the rebalance
deadband instead (see `diff.py`). `mvo` (Phase 1b) is where costs enter an
objective properly.
"""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal
from typing import Protocol

from .config import Config


@dataclass(frozen=True)
class Signal:
    asset: str
    direction: str  # "long" | "short"
    conviction: Decimal
    #: Realised volatility, carried so a constructor can size on risk rather
    #: than on capital. Optional because a signal is not obliged to have an
    #: opinion about risk, and a constructor that needs one must say so when it
    #: is missing rather than substituting a number.
    volatility: Decimal | None = None


@dataclass(frozen=True)
class Construction:
    weights: dict[str, Decimal]
    constructor: str
    requested: str
    notes: list[str]

    @property
    def fell_back(self) -> bool:
        return self.constructor != self.requested


class PortfolioConstructor(Protocol):
    name: str

    def construct(self, signals: list[Signal], *, config: Config) -> Construction: ...


class EqualWeight:
    """Equal weight across signalled assets, capped per position.

    If `target_gross / n` exceeds `max_position`, the per-position cap binds and
    the book is deliberately left under-invested rather than concentrated into
    fewer names. Spending the leftover on the remaining assets would quietly
    convert a diversification constraint into a concentration one.
    """

    name = "equal_weight"

    def construct(self, signals: list[Signal], *, config: Config) -> Construction:
        notes: list[str] = []
        if not signals:
            return Construction({}, self.name, self.name, ["no signals - target is flat"])

        n = len(signals)
        per = config.target_gross_exposure / Decimal(n)
        cap = config.limits.max_position

        if per > cap:
            per = cap
            notes.append(
                f"per-position cap binds: {n} assets x {cap} = "
                f"{(cap * n).quantize(Decimal('0.0001'))} gross, "
                f"below the {config.target_gross_exposure} target. Left "
                "under-invested rather than concentrated."
            )

        weights = {
            s.asset: (per if s.direction == "long" else -per) for s in signals
        }
        return Construction(weights, self.name, self.name, notes)


class ConvictionTilt:
    """Equal-weight base, scaled by conviction (design spec §4.5).

    Cheap and robust: it keeps equal-weight's diversification and only leans the
    book toward what the signal ranked higher. It cannot concentrate the way an
    optimiser can, because the tilt is bounded by the spread of the convictions
    themselves rather than by an objective function.

    The per-position cap is applied and the remainder is **not redistributed**,
    for the same reason as `equal_weight`: spending a capped position's leftover
    on the others converts a diversification constraint into a concentration
    one. The book is left under-invested and says so.
    """

    name = "conviction_tilt"

    def construct(self, signals: list[Signal], *, config: Config) -> Construction:
        notes: list[str] = []
        if not signals:
            return Construction({}, self.name, self.name, ["no signals - target is flat"])

        total = sum((s.conviction for s in signals), Decimal(0))
        if total <= 0:
            # Every conviction zero means the signal ranked nothing. Falling
            # back to equal weight would invent an opinion the signal declined
            # to have, so this is flat instead.
            return Construction(
                {}, self.name, self.name, ["convictions sum to zero - target is flat"]
            )

        weights: dict[str, Decimal] = {}
        capped: list[str] = []
        for s in signals:
            share = config.target_gross_exposure * (s.conviction / total)
            if share > config.limits.max_position:
                share = config.limits.max_position
                capped.append(s.asset)
            weights[s.asset] = share if s.direction == "long" else -share

        if capped:
            gross = sum((abs(w) for w in weights.values()), Decimal(0))
            notes.append(
                f"per-position cap binds for {', '.join(capped)}: gross "
                f"{gross.quantize(Decimal('0.0001'))} against a "
                f"{config.target_gross_exposure} target. Left under-invested rather "
                "than concentrated."
            )

        return Construction(weights, self.name, self.name, notes)


class InverseVolatility:
    """Size so each position contributes comparable risk, not comparable capital.

    Equal *capital* across a 30-vol asset and a 120-vol asset is a book whose
    risk is dominated by one name while its weights look balanced. Weighting by
    `1/vol` is the crudest correction that fixes that, and it needs no
    covariance estimate - which matters, because there isn't one until §4.4
    lands.

    A signal with no volatility estimate is **dropped and disclosed**, not
    assigned an assumed one. Substituting a number here would silently size a
    position on a guess, and the whole reason to size on risk is that the risk
    number is real.
    """

    name = "inverse_vol"

    def construct(self, signals: list[Signal], *, config: Config) -> Construction:
        notes: list[str] = []
        if not signals:
            return Construction({}, self.name, self.name, ["no signals - target is flat"])

        usable = [s for s in signals if s.volatility is not None and s.volatility > 0]
        dropped = [s.asset for s in signals if s not in usable]
        if dropped:
            notes.append(
                f"no volatility estimate for {', '.join(sorted(dropped))}: dropped rather "
                "than sized on an assumed one"
            )
        if not usable:
            return Construction(
                {}, self.name, self.name, notes + ["no sizeable signals - target is flat"]
            )

        inverse = {s.asset: Decimal(1) / s.volatility for s in usable}
        total = sum(inverse.values(), Decimal(0))

        weights: dict[str, Decimal] = {}
        capped: list[str] = []
        for s in usable:
            share = config.target_gross_exposure * (inverse[s.asset] / total)
            if share > config.limits.max_position:
                share = config.limits.max_position
                capped.append(s.asset)
            weights[s.asset] = share if s.direction == "long" else -share

        if capped:
            notes.append(
                f"per-position cap binds for {', '.join(capped)}: left under-invested "
                "rather than concentrated."
            )

        return Construction(weights, self.name, self.name, notes)


class RiskAdjusted:
    """Size by expected edge PER UNIT OF RISK, capped by what each name absorbs.

    Three requirements that pull against each other, and the resolution of the
    middle one is the whole point:

    **Only trade what pays for itself.** A name whose expected edge cannot cover
    its own round trip should not be held; that capital does more good enlarging
    a better position. The threshold is expressed as a multiple of the round-trip
    cost, so it means "worth k times what it costs to get in and out" rather than
    an arbitrary number.

    **Prefer higher expectancy** - but not by weighting on raw conviction.
    Predictions scale with volatility, so raw conviction quietly concentrates the
    book into the most volatile names. Phase 1 measured it: conviction and
    risk-adjusted weighting returned the same and held the same number of names
    at almost the same effective N (12.3 against 12.7), yet drew down 48.7%
    against 17.3%. The concentration was never in name count, it was in RISK.
    Dividing by volatility removes it without giving up return.

    **Stay within what the market can absorb.** A weight the asset cannot sustain
    is not a position, it is a fantasy with a plausible number attached. Each
    name is capped at the lower of `max_position` and its own liquidity, and what
    cannot be placed spills to names with headroom rather than sitting idle.

    A signal with no volatility estimate is dropped and disclosed, never assigned
    an assumed one - the entire justification for this constructor is that the
    risk number is real.
    """

    name = "risk_adjusted"

    #: Entry threshold as a multiple of the round-trip cost. 1.0 is the Phase 1
    #: value: k=0 and k=1 are close, k=2 buys return with drawdown, and k=4
    #: stands the book down on 40% of days and degrades.
    COST_MULTIPLE = Decimal(1)

    def construct(self, signals: list[Signal], *, config: Config) -> Construction:
        notes: list[str] = []
        if not signals:
            return Construction({}, self.name, self.name, ["no signals - target is flat"])

        usable = [s for s in signals if s.volatility is not None and s.volatility > 0]
        dropped = [s.asset for s in signals if s.volatility is None or s.volatility <= 0]
        if dropped:
            notes.append(
                f"no volatility estimate for {', '.join(sorted(dropped))}: dropped "
                "rather than sized on an assumed one"
            )
        if not usable:
            return Construction({}, self.name, self.name, notes + ["nothing sizeable"])

        # Round trip: in and out, both crossing the spread and paying commission.
        round_trip = 2 * (config.costs.commission_bps + config.costs.spread_bps) / Decimal(10_000)
        floor = self.COST_MULTIPLE * round_trip
        qualifying = [s for s in usable if s.conviction >= floor]
        below = len(usable) - len(qualifying)
        if below:
            notes.append(
                f"{below} name(s) had an expected edge below {floor * 10_000:.1f}bp - "
                f"{self.COST_MULTIPLE}x the round trip - and were not traded"
            )
        if not qualifying:
            return Construction(
                {}, self.name, self.name,
                notes + ["nothing cleared the cost threshold - target is flat"],
            )

        longs = [s for s in qualifying if s.direction == "long"]
        shorts = [s for s in qualifying if s.direction == "short"]
        if len(longs) < 2 or len(shorts) < 2:
            return Construction(
                {}, self.name, self.name,
                notes + [
                    f"{len(longs)} long and {len(shorts)} short cleared the threshold; "
                    "a book that cannot form two sides is a directional bet this "
                    "constructor never claimed - target is flat"
                ],
            )

        half = config.target_gross_exposure / Decimal(2)
        weights: dict[str, Decimal] = {}
        for side, sign in ((longs, Decimal(1)), (shorts, Decimal(-1))):
            appetite = {s.asset: s.conviction / s.volatility for s in side}
            total = sum(appetite.values(), Decimal(0)) or Decimal(1)
            for s in side:
                share = half * appetite[s.asset] / total
                weights[s.asset] = weights.get(s.asset, Decimal(0)) + sign * share

        capped = [a for a, w in weights.items() if abs(w) > config.limits.max_position]
        if capped:
            largest = max(abs(w) for w in weights.values())
            scale = config.limits.max_position / largest
            weights = {a: w * scale for a, w in weights.items()}
            notes.append(
                f"per-position cap binds for {', '.join(sorted(capped))}: the whole "
                f"book is scaled by {scale.quantize(Decimal('0.001'))} rather than "
                "redistributing, which would convert a diversification limit into a "
                "concentration one"
            )

        notes.append(
            f"{len(longs)} long and {len(shorts)} short, sized by edge/volatility"
        )
        return Construction(weights, self.name, self.name, notes)


_REGISTRY: dict[str, PortfolioConstructor] = {
    c.name: c for c in (EqualWeight(), ConvictionTilt(), InverseVolatility(),
                        RiskAdjusted())
}


def get(name: str) -> PortfolioConstructor:
    if name not in _REGISTRY:
        raise ValueError(f"unknown constructor {name!r}; have {sorted(_REGISTRY)}")
    return _REGISTRY[name]
