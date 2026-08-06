"""The Plan: the artifact, and the contract with the Rust executor.

A Plan is the complete description of what the system intends. It is immutable
once written, addressed by id, and the only thing `execute_plan` accepts. See
design spec sections 3.5 and 5.3.

Three properties this module is responsible for:

**Decimals are strings on the wire.** A JSON number is an IEEE double on both
sides of this contract, and Python and Rust do not have to round one identically.
Money, weights and quantities cross as exact decimal strings and are parsed into
`Decimal` here and `rust_decimal` there.

**Serialisation is canonical.** Sorted keys, fixed separators, UTF-8, trailing
newline. Two runs over the same inputs must produce byte-identical output, which
is the Phase 0 gate.

**Identity is derived, not random.** `plan_id` is a UUIDv5 over the plan's own
content, so the same decision computed twice is the same plan rather than two
plans that happen to agree. `created_at` is wall-clock and therefore excluded
from the digest - it is the one field allowed to differ between two runs of the
same decision.
"""

from __future__ import annotations

import hashlib
import json
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from typing import Any, Literal

import jsonschema

# 1.1.0 added the `turnover_capped` warning kind when turnover moved from a
# whole-plan veto to a diff budget. Additive to an enum, so a 1.0.0 consumer
# would reject a plan carrying the new value - which is the correct behaviour
# for a system that fails closed, and the reason the minor version moved.
SCHEMA_VERSION = "1.2.0"

SCHEMA_PATH = Path(__file__).resolve().parents[3] / "schema" / "plan.schema.json"

# Stable namespace for deriving plan ids. Never change this: doing so renames
# every plan the system has ever produced.
_PLAN_NAMESPACE = uuid.UUID("6f1d5a1e-6b1c-5f4a-9c2e-3d7b8a0c4e11")

# Fields excluded from the content digest. `created_at` is wall clock;
# `plan_id` is derived from the digest and so cannot be an input to it.
_DIGEST_EXCLUDED = ("created_at", "plan_id")


# ---------------------------------------------------------------------------
# Decimal handling
# ---------------------------------------------------------------------------


def dec(value: Decimal | int | str) -> str:
    """Render an exact decimal for the wire.

    Plain notation only - `Decimal("1E+2")` formats as `1E+2` under `str()`,
    which the schema rejects, and which Rust would reject too. Negative zero is
    normalised away because `-0` is a true statement about a float and a
    meaningless one about a position.
    """
    d = value if isinstance(value, Decimal) else Decimal(str(value))
    if not d.is_finite():
        raise ValueError(f"non-finite decimal cannot cross the contract: {value!r}")
    text = format(d, "f")
    if text.startswith("-") and Decimal(text) == 0:
        text = text[1:]
    return text


# ---------------------------------------------------------------------------
# Plan parts
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Nav:
    total: Decimal
    cash: Decimal
    gross_exposure: Decimal
    net_exposure: Decimal
    benchmark_beta: Decimal | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "total": dec(self.total),
            "cash": dec(self.cash),
            "gross_exposure": dec(self.gross_exposure),
            "net_exposure": dec(self.net_exposure),
            "benchmark_beta": None if self.benchmark_beta is None else dec(self.benchmark_beta),
        }


@dataclass(frozen=True)
class Provenance:
    planner_version: str
    feature_set_version: str
    scoring_version: str
    risk_model_version: str
    ruleset_version: str
    constructor: str
    constructor_requested: str
    inputs_hash: str
    universe_size: int
    model_id: str | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "planner_version": self.planner_version,
            "feature_set_version": self.feature_set_version,
            "scoring_version": self.scoring_version,
            "risk_model_version": self.risk_model_version,
            "ruleset_version": self.ruleset_version,
            "constructor": self.constructor,
            "constructor_requested": self.constructor_requested,
            "model_id": self.model_id,
            "inputs_hash": self.inputs_hash,
            "universe_size": self.universe_size,
        }


@dataclass(frozen=True)
class Target:
    asset: str
    weight: Decimal
    direction: Literal["long", "short"]
    conviction: Decimal | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "asset": self.asset,
            "weight": dec(self.weight),
            "direction": self.direction,
            "conviction": None if self.conviction is None else dec(self.conviction),
        }


@dataclass(frozen=True)
class CurrentPosition:
    asset: str
    qty: Decimal
    weight: Decimal

    def to_wire(self) -> dict[str, Any]:
        return {"asset": self.asset, "qty": dec(self.qty), "weight": dec(self.weight)}


@dataclass(frozen=True)
class Order:
    asset: str
    side: Literal["buy", "sell"]
    qty: Decimal
    order_type: Literal["market", "limit"]
    reason: Literal["entry", "exit", "increase", "reduce", "rebalance"]
    limit_price: Decimal | None = None
    est_cost_bps: Decimal | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "asset": self.asset,
            "side": self.side,
            "qty": dec(self.qty),
            "order_type": self.order_type,
            "limit_price": None if self.limit_price is None else dec(self.limit_price),
            "reason": self.reason,
            "est_cost_bps": None if self.est_cost_bps is None else dec(self.est_cost_bps),
        }


@dataclass(frozen=True)
class RiskCheck:
    name: str
    limit: Decimal
    value: Decimal
    passed: bool
    detail: str | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "limit": dec(self.limit),
            "value": dec(self.value),
            "passed": self.passed,
            "detail": self.detail,
        }


@dataclass(frozen=True)
class RiskReport:
    checks: list[RiskCheck]
    rejected_reason: str | None = None

    @property
    def passed(self) -> bool:
        return all(c.passed for c in self.checks)

    def to_wire(self) -> dict[str, Any]:
        return {
            "passed": self.passed,
            "checks": [c.to_wire() for c in self.checks],
            "rejected_reason": self.rejected_reason,
        }


@dataclass(frozen=True)
class AssetCost:
    asset: str
    bps: Decimal
    spread_bps: Decimal | None = None
    impact_bps: Decimal | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "asset": self.asset,
            "bps": dec(self.bps),
            "spread_bps": None if self.spread_bps is None else dec(self.spread_bps),
            "impact_bps": None if self.impact_bps is None else dec(self.impact_bps),
        }


@dataclass(frozen=True)
class CostEstimate:
    total_bps: Decimal
    total_quote: Decimal
    per_asset: list[AssetCost] = field(default_factory=list)

    def to_wire(self) -> dict[str, Any]:
        return {
            "total_bps": dec(self.total_bps),
            "total_quote": dec(self.total_quote),
            "per_asset": [a.to_wire() for a in self.per_asset],
        }


@dataclass(frozen=True)
class Warning:
    kind: Literal[
        "degenerate_feature",
        "unenforced_rule",
        "insufficient_sample",
        "constructor_fallback",
        "stale_input",
        "turnover_capped",
        "other",
    ]
    message: str

    def to_wire(self) -> dict[str, Any]:
        return {"kind": self.kind, "message": self.message}


# ---------------------------------------------------------------------------
# Assembly
# ---------------------------------------------------------------------------


def canonical_json(doc: dict[str, Any]) -> str:
    """The one serialisation. Byte-stability is a gate, so this has no options."""
    return json.dumps(doc, sort_keys=True, indent=2, ensure_ascii=False) + "\n"


def canonical_bytes(doc: dict[str, Any]) -> bytes:
    """The wire form. **Always write this, never `write_text`.**

    `Path.write_text` opens in text mode, which on Windows translates every
    `\\n` into `\\r\\n`. For a file whose *bytes are the contract*, that makes
    the serialisation platform-dependent - which is exactly what §3.5 says it
    must not be. The asymmetry is what makes it hard to catch: `read_text`
    translates the CRLFs back on the way in, so a round-trip test passes on the
    machine that produced the wrong bytes, and only a Linux CI job comparing
    against the committed file notices.
    """
    return canonical_json(doc).encode("utf-8")


def digest(doc: dict[str, Any]) -> str:
    """SHA-256 over the plan's decision content.

    Excludes `created_at` and `plan_id`. Two runs of the same decision over the
    same inputs produce the same digest; that equality is the Phase 0 gate and
    the thing `plan --dry-run --verify` compares.
    """
    body = {k: v for k, v in doc.items() if k not in _DIGEST_EXCLUDED}
    return hashlib.sha256(canonical_json(body).encode("utf-8")).hexdigest()


def build(
    *,
    run_id: uuid.UUID,
    bot_id: str,
    as_of: datetime,
    mode: Literal["dry", "live"],
    quote_currency: str,
    nav: Nav,
    provenance: Provenance,
    targets: list[Target],
    current: list[CurrentPosition],
    orders: list[Order],
    risk_report: RiskReport,
    cost_estimate: CostEstimate,
    warnings: list[Warning],
    created_at: datetime | None = None,
) -> dict[str, Any]:
    """Assemble, derive the id, validate, return the wire document.

    A rejected plan carries no orders. That invariant is in the schema too, but
    it is enforced here as well so a caller cannot construct a document that
    only fails at the boundary.
    """
    if as_of.tzinfo is None:
        raise ValueError("as_of must be timezone-aware")

    passed = risk_report.passed
    status: str = "accepted" if passed else "rejected"
    if not passed and orders:
        raise ValueError("a rejected plan cannot carry orders - reject whole, never partially")
    if not passed and risk_report.rejected_reason is None:
        raise ValueError("a rejected plan must say why")

    doc: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "plan_id": str(uuid.UUID(int=0)),  # placeholder, replaced below
        "run_id": str(run_id),
        "bot_id": bot_id,
        "created_at": (created_at or datetime.now(timezone.utc)).isoformat(),
        "as_of": as_of.isoformat(),
        "mode": mode,
        "status": status,
        "quote_currency": quote_currency,
        "nav": nav.to_wire(),
        "provenance": provenance.to_wire(),
        "targets": [t.to_wire() for t in targets],
        "current": [c.to_wire() for c in current],
        "orders": [o.to_wire() for o in orders],
        "risk_report": risk_report.to_wire(),
        "cost_estimate": cost_estimate.to_wire(),
        "warnings": [w.to_wire() for w in warnings],
    }

    doc["plan_id"] = str(uuid.uuid5(_PLAN_NAMESPACE, digest(doc)))
    validate(doc)
    return doc


_schema_cache: dict[str, Any] | None = None


def schema() -> dict[str, Any]:
    global _schema_cache
    if _schema_cache is None:
        _schema_cache = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    return _schema_cache


def validate(doc: dict[str, Any]) -> None:
    """Validate against the shared schema. Raises `jsonschema.ValidationError`.

    Called on every write. The contract is validated, not trusted - a planner
    bug should surface here, in Python, with a path to the offending field,
    rather than as a parse failure in the executor at 03:00.
    """
    jsonschema.validate(instance=doc, schema=schema())


def write(path: Path, doc: dict[str, Any]) -> str:
    """Validate and write canonically. Returns the digest."""
    validate(doc)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(canonical_bytes(doc))
    return digest(doc)


def read(path: Path) -> dict[str, Any]:
    doc = json.loads(path.read_bytes().decode("utf-8"))
    validate(doc)
    return doc
