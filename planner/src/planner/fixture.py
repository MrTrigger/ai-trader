"""The cross-language contract fixture.

A plan built from fixed inputs, with no market data and no clock, so it is
byte-identical on every machine. Python writes it; Rust parses it and compares
values. That pair is the only thing standing between the two halves of the
system and silent drift, so the fixture is committed and its regeneration is
deliberate rather than automatic.

Deliberately includes the awkward cases rather than a tidy happy path:

  * a decimal that is exact in base 10 and not in binary (0.1), which is where a
    float-carrying contract would first disagree
  * high precision (8dp quantity), to catch a narrower decimal type downstream
  * a negative weight, for when shorts exist
  * a null (`benchmark_beta`, `limit_price` on a market order)
  * a limit order carrying a price, so the conditional requirement is exercised
  * every warning kind, so an added variant breaks the Rust enum loudly instead
    of at 03:00 on a live plan
"""

from __future__ import annotations

from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
import uuid

from . import plan as P

FIXTURE_PATH = (
    Path(__file__).resolve().parents[3]
    / "service"
    / "crates"
    / "plan"
    / "tests"
    / "fixtures"
    / "plan.json"
)

_RUN_ID = uuid.UUID("7c9e6679-7425-40de-944b-e07fc1f90ae7")
_AS_OF = datetime(2026, 8, 1, tzinfo=timezone.utc)
_CREATED_AT = datetime(2026, 8, 1, 6, 30, tzinfo=timezone.utc)


def build() -> dict:
    return P.build(
        run_id=_RUN_ID,
        as_of=_AS_OF,
        created_at=_CREATED_AT,
        mode="live",
        quote_currency="USD",
        nav=P.Nav(
            total=Decimal("100000.00"),
            cash=Decimal("25000.10"),
            gross_exposure=Decimal("0.750000"),
            net_exposure=Decimal("0.250000"),
            benchmark_beta=None,
        ),
        provenance=P.Provenance(
            planner_version="0.1.0",
            feature_set_version="fs-phase0-1",
            scoring_version="none-phase0",
            risk_model_version="none-phase0",
            ruleset_version="phase0",
            constructor="conviction_tilt",
            constructor_requested="mvo",
            inputs_hash="2117ef6d27b612fd",
            universe_size=30,
            model_id=None,
        ),
        targets=[
            P.Target(
                asset="BTC",
                weight=Decimal("0.500000"),
                direction="long",
                conviction=Decimal("0.87"),
            ),
            P.Target(
                asset="ETH",
                weight=Decimal("-0.250000"),
                direction="short",
                conviction=None,
            ),
        ],
        current=[
            P.CurrentPosition(
                asset="BTC", qty=Decimal("0.10000000"), weight=Decimal("0.100000")
            )
        ],
        orders=[
            P.Order(
                asset="BTC",
                side="buy",
                qty=Decimal("0.39753288"),
                order_type="limit",
                limit_price=Decimal("64000.50"),
                reason="increase",
                est_cost_bps=Decimal("7.08"),
            ),
            P.Order(
                asset="ETH",
                side="sell",
                qty=Decimal("13.42209814"),
                order_type="market",
                limit_price=None,
                reason="entry",
                est_cost_bps=Decimal("7.17"),
            ),
        ],
        risk_report=P.RiskReport(
            checks=[
                P.RiskCheck(
                    name="max_gross_exposure",
                    limit=Decimal("1.00"),
                    value=Decimal("0.75"),
                    passed=True,
                    detail="sum of |weight| across targets",
                ),
                P.RiskCheck(
                    name="max_position",
                    limit=Decimal("0.50"),
                    value=Decimal("0.50"),
                    passed=True,
                    detail=None,
                ),
            ]
        ),
        cost_estimate=P.CostEstimate(
            total_bps=Decimal("5.39"),
            total_quote=Decimal("53.88"),
            per_asset=[
                P.AssetCost(
                    asset="BTC",
                    bps=Decimal("7.08"),
                    spread_bps=Decimal("2.00"),
                    impact_bps=Decimal("0.08"),
                ),
                P.AssetCost(
                    asset="ETH",
                    bps=Decimal("7.17"),
                    spread_bps=Decimal("2.00"),
                    impact_bps=None,
                ),
            ],
        ),
        warnings=[
            P.Warning(kind="degenerate_feature", message="revisions: 12 of 30 snapshots"),
            P.Warning(kind="unenforced_rule", message="limit max_cluster_exposure is not enforced"),
            P.Warning(kind="insufficient_sample", message="n=41, below the 100 floor"),
            P.Warning(kind="constructor_fallback", message="requested mvo, used conviction_tilt"),
            P.Warning(kind="stale_input", message="adv_quote is 2 bars old for ETH"),
            P.Warning(kind="turnover_capped", message="budget 0.50 spent; 1 trade deferred"),
            P.Warning(kind="other", message="cost model is uncalibrated"),
        ],
    )


def write() -> str:
    """Regenerate the committed fixture. Returns its digest.

    Bytes, not text - see `plan.canonical_bytes`. This file is compared
    byte-for-byte by CI, so a platform newline translation here would make the
    contract depend on which machine last regenerated it.
    """
    doc = build()
    FIXTURE_PATH.parent.mkdir(parents=True, exist_ok=True)
    FIXTURE_PATH.write_bytes(P.canonical_bytes(doc))
    return P.digest(doc)
