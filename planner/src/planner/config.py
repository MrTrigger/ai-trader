"""Configuration.

Config lives in a versioned file loaded by the engine. It is never fetched from
a record whose contents the system is told to obey - a process holding trading
credentials must not take instructions from anything it reads at runtime
(design spec section 8.2).

Every limit that is *not* enforced must be representable as unenforced, so the
run can disclose it above its results rather than implying a completeness it
does not have.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from decimal import Decimal
from pathlib import Path
from typing import Any

DEFAULT_CONFIG_PATH = Path(__file__).resolve().parents[3] / "config" / "default.toml"


def _dec(v: Any) -> Decimal:
    return Decimal(str(v))


@dataclass(frozen=True)
class RiskLimits:
    """Hard limits. A plan breaching any one is rejected whole (design spec section 6).

    `None` means *not enforced*, which is a legitimate state at Phase 0 but a
    disclosed one: the pipeline emits an `unenforced_rule` warning for each, and
    those warnings are reported before any number derived from the plan.
    """

    max_gross_exposure: Decimal
    max_position: Decimal
    max_position_count: int
    max_turnover: Decimal
    min_position_notional: Decimal
    max_net_exposure: Decimal | None = None
    max_cluster_exposure: Decimal | None = None
    max_benchmark_beta: Decimal | None = None

    @staticmethod
    def from_dict(d: dict[str, Any]) -> RiskLimits:
        def opt(key: str) -> Decimal | None:
            """TOML has no null. An absent or empty value means *unenforced*.

            Deliberately not "set it to a permissive number": a limit of 999 is
            indistinguishable in a report from a limit that passed, whereas
            `None` forces the disclosure.
            """
            v = d.get(key)
            return None if v is None or v == "" else _dec(v)

        return RiskLimits(
            max_gross_exposure=_dec(d["max_gross_exposure"]),
            max_position=_dec(d["max_position"]),
            max_position_count=int(d["max_position_count"]),
            max_turnover=_dec(d["max_turnover"]),
            min_position_notional=_dec(d["min_position_notional"]),
            max_net_exposure=opt("max_net_exposure"),
            max_cluster_exposure=opt("max_cluster_exposure"),
            max_benchmark_beta=opt("max_benchmark_beta"),
        )

    def unenforced(self) -> list[str]:
        names = []
        if self.max_net_exposure is None:
            names.append("max_net_exposure")
        if self.max_cluster_exposure is None:
            names.append("max_cluster_exposure")
        if self.max_benchmark_beta is None:
            names.append("max_benchmark_beta")
        return names


@dataclass(frozen=True)
class CostModel:
    """Per-asset transaction cost in basis points.

    Three components, following the shape the harness uses for futures fills:
    commission, half-spread crossed on entry, and market impact as a function of
    trade size against recent volume.

    Impact is `coefficient * sqrt(notional / ADV) * daily_vol_bps`. The square
    root is the standard concave form - doubling your size costs less than twice
    as much - and the coefficient is the one number here that must be calibrated
    against realised fills rather than assumed. Until it is, every cost number
    this produces is an estimate carrying an unquantified error, and Phase 3's
    gate is exactly the comparison that resolves it.
    """

    commission_bps: Decimal
    spread_bps: Decimal
    impact_coefficient: Decimal
    adv_lookback_days: int
    calibrated: bool = False

    @staticmethod
    def from_dict(d: dict[str, Any]) -> CostModel:
        return CostModel(
            commission_bps=_dec(d["commission_bps"]),
            spread_bps=_dec(d["spread_bps"]),
            impact_coefficient=_dec(d["impact_coefficient"]),
            adv_lookback_days=int(d["adv_lookback_days"]),
            calibrated=bool(d.get("calibrated", False)),
        )


@dataclass(frozen=True)
class Config:
    quote_currency: str
    interval_s: int
    universe: list[str]
    target_gross_exposure: Decimal
    constructor: str
    min_dollar_volume: Decimal
    min_history_bars: int
    rebalance_cost_multiple: Decimal
    limits: RiskLimits
    costs: CostModel
    ruleset_version: str = "phase0"
    signal: str = "placeholder_equal_long"
    meta: dict[str, Any] = field(default_factory=dict)

    @staticmethod
    def load(path: Path | None = None) -> Config:
        path = path or DEFAULT_CONFIG_PATH
        raw = tomllib.loads(path.read_text(encoding="utf-8"))
        p = raw["portfolio"]
        return Config(
            quote_currency=p["quote_currency"],
            interval_s=int(p["interval_s"]),
            universe=[a.upper() for a in p["universe"]],
            target_gross_exposure=_dec(p["target_gross_exposure"]),
            constructor=p["constructor"],
            min_dollar_volume=_dec(p["min_dollar_volume"]),
            min_history_bars=int(p["min_history_bars"]),
            rebalance_cost_multiple=_dec(p["rebalance_cost_multiple"]),
            signal=p.get("signal", "placeholder_equal_long"),
            ruleset_version=raw.get("meta", {}).get("ruleset_version", "phase0"),
            limits=RiskLimits.from_dict(raw["limits"]),
            costs=CostModel.from_dict(raw["costs"]),
            meta=raw.get("meta", {}),
        )
