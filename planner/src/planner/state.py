"""Portfolio state: what we currently hold.

At Phase 0 this is a local JSON book, because there is no venue. From Phase 2 it
comes from the venue through a **read-only** key and is reconciled against this
record every run - and a mismatch stops the run rather than being auto-corrected
(design spec section 6.3). The shape does not change when the source does.

Positions are derived from fills in the real system; this file is the Phase 0
stand-in for that derivation, not a competing source of truth.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path

DEFAULT_PATH = Path(__file__).resolve().parents[3] / "data" / "book.json"


@dataclass(frozen=True)
class Position:
    asset: str
    qty: Decimal


@dataclass(frozen=True)
class Portfolio:
    cash: Decimal
    positions: list[Position]
    as_of: datetime

    def qty(self, asset: str) -> Decimal:
        for p in self.positions:
            if p.asset == asset:
                return p.qty
        return Decimal(0)

    def nav(self, prices: dict[str, Decimal]) -> Decimal:
        """Cash plus marked positions.

        A position with no price is an error, not a zero. Marking an unpriced
        holding at zero silently understates NAV and every weight computed from
        it, which is the kind of wrongness that produces a confidently wrong
        plan rather than a failed one.
        """
        total = self.cash
        for p in self.positions:
            if p.asset not in prices:
                raise KeyError(
                    f"no price for held asset {p.asset}; refusing to mark it at zero"
                )
            total += p.qty * prices[p.asset]
        return total

    def weights(self, prices: dict[str, Decimal]) -> dict[str, Decimal]:
        nav = self.nav(prices)
        if nav <= 0:
            raise ValueError(f"non-positive NAV ({nav}); cannot express weights")
        return {p.asset: (p.qty * prices[p.asset]) / nav for p in self.positions if p.qty != 0}


def load(path: Path = DEFAULT_PATH, *, as_of: datetime | None = None) -> Portfolio:
    """Read the book. A missing book is an empty one, not an error.

    Starting flat is a legitimate state - it is where every deployment begins -
    and forcing a file to exist before the first run adds a step that can only
    be got wrong.
    """
    stamp = as_of or datetime.now(timezone.utc)
    if not path.exists():
        return Portfolio(cash=Decimal(0), positions=[], as_of=stamp)

    raw = json.loads(path.read_text(encoding="utf-8"))
    return Portfolio(
        cash=Decimal(str(raw["cash"])),
        positions=[
            Position(asset=p["asset"].upper(), qty=Decimal(str(p["qty"])))
            for p in raw.get("positions", [])
            if Decimal(str(p["qty"])) != 0
        ],
        as_of=stamp,
    )


def save(portfolio: Portfolio, path: Path = DEFAULT_PATH) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "cash": str(portfolio.cash),
        "positions": [{"asset": p.asset, "qty": str(p.qty)} for p in portfolio.positions],
        "as_of": portfolio.as_of.isoformat(),
    }
    path.write_bytes((json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8"))
