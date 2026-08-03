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


_REGISTRY: dict[str, PortfolioConstructor] = {c.name: c for c in (EqualWeight(),)}


def get(name: str) -> PortfolioConstructor:
    if name not in _REGISTRY:
        raise ValueError(f"unknown constructor {name!r}; have {sorted(_REGISTRY)}")
    return _REGISTRY[name]
