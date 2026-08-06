"""The Plan contract.

These tests guard the properties the Rust executor depends on. A failure here
is a contract break, not a unit-test nit.
"""

from __future__ import annotations

import uuid
from datetime import datetime, timedelta, timezone
from decimal import Decimal

import jsonschema
import pytest

from planner import plan as P

RUN_ID = uuid.UUID("11111111-2222-3333-4444-555555555555")
AS_OF = datetime(2026, 8, 1, tzinfo=timezone.utc)


def _provenance(**over) -> P.Provenance:
    base = dict(
        planner_version="0.1.0",
        feature_set_version="fs-1",
        scoring_version="sc-1",
        risk_model_version="rm-0",
        ruleset_version="rules-1",
        constructor="equal_weight",
        constructor_requested="equal_weight",
        inputs_hash="abcdef0123456789",
        universe_size=30,
    )
    return P.Provenance(**(base | over))


def _nav() -> P.Nav:
    return P.Nav(
        total=Decimal("10000"),
        cash=Decimal("2000"),
        gross_exposure=Decimal("0.8"),
        net_exposure=Decimal("0.8"),
    )


def _passing_risk() -> P.RiskReport:
    return P.RiskReport(
        checks=[
            P.RiskCheck(
                name="max_gross_exposure",
                limit=Decimal("1.0"),
                value=Decimal("0.8"),
                passed=True,
            )
        ]
    )


def _build(**over):
    base = dict(
        run_id=RUN_ID,
        bot_id="testbot",
        as_of=AS_OF,
        mode="dry",
        quote_currency="USD",
        nav=_nav(),
        provenance=_provenance(),
        targets=[P.Target(asset="BTC", weight=Decimal("0.4"), direction="long")],
        current=[],
        orders=[
            P.Order(
                asset="BTC",
                side="buy",
                qty=Decimal("0.05"),
                order_type="limit",
                limit_price=Decimal("64000.5"),
                reason="entry",
            )
        ],
        risk_report=_passing_risk(),
        cost_estimate=P.CostEstimate(total_bps=Decimal("8"), total_quote=Decimal("3.2")),
        warnings=[],
    )
    return P.build(**(base | over))


def test_builds_and_validates():
    doc = _build()
    assert doc["status"] == "accepted"
    assert doc["schema_version"] == P.SCHEMA_VERSION
    P.validate(doc)


def test_plan_id_is_derived_from_content():
    """Same decision computed twice is the same plan, not two that agree."""
    a = _build(created_at=datetime(2026, 8, 1, 12, tzinfo=timezone.utc))
    b = _build(created_at=datetime(2026, 8, 2, 18, tzinfo=timezone.utc))
    assert a["created_at"] != b["created_at"]
    assert a["plan_id"] == b["plan_id"]
    assert P.digest(a) == P.digest(b)


def test_different_targets_give_a_different_plan():
    a = _build()
    b = _build(targets=[P.Target(asset="ETH", weight=Decimal("0.4"), direction="long")])
    assert a["plan_id"] != b["plan_id"]


def test_serialisation_is_byte_stable():
    doc = _build(created_at=AS_OF)
    assert P.canonical_json(doc) == P.canonical_json(_build(created_at=AS_OF))


def test_decimals_cross_as_strings():
    doc = _build()
    assert doc["nav"]["total"] == "10000"
    assert doc["orders"][0]["qty"] == "0.05"
    assert isinstance(doc["targets"][0]["weight"], str)


def test_decimal_rendering_is_plain_notation():
    assert P.dec(Decimal("1E+2")) == "100"
    assert P.dec(Decimal("-0.00")) == "0.00"
    assert P.dec(Decimal("0.10")) == "0.10"
    assert P.dec(Decimal("-1.5")) == "-1.5"


def test_non_finite_decimal_is_refused():
    with pytest.raises(ValueError):
        P.dec(Decimal("NaN"))


def test_rejected_plan_cannot_carry_orders():
    failing = P.RiskReport(
        checks=[
            P.RiskCheck(
                name="max_gross_exposure",
                limit=Decimal("1.0"),
                value=Decimal("1.6"),
                passed=False,
            )
        ],
        rejected_reason="gross exposure 1.6 exceeds limit 1.0",
    )
    with pytest.raises(ValueError, match="reject whole"):
        _build(risk_report=failing)


def test_rejected_plan_must_say_why():
    failing = P.RiskReport(
        checks=[
            P.RiskCheck(
                name="max_gross_exposure",
                limit=Decimal("1.0"),
                value=Decimal("1.6"),
                passed=False,
            )
        ]
    )
    with pytest.raises(ValueError, match="must say why"):
        _build(orders=[], risk_report=failing)


def test_rejected_plan_is_valid_with_no_orders():
    failing = P.RiskReport(
        checks=[
            P.RiskCheck(
                name="max_gross_exposure",
                limit=Decimal("1.0"),
                value=Decimal("1.6"),
                passed=False,
            )
        ],
        rejected_reason="gross exposure 1.6 exceeds limit 1.0",
    )
    doc = _build(orders=[], risk_report=failing)
    assert doc["status"] == "rejected"
    assert doc["orders"] == []


def test_naive_as_of_is_refused():
    with pytest.raises(ValueError, match="timezone-aware"):
        _build(as_of=datetime(2026, 8, 1))


def test_limit_order_without_a_price_fails_schema():
    doc = _build()
    doc["orders"][0]["limit_price"] = None
    with pytest.raises(jsonschema.ValidationError):
        P.validate(doc)


def test_unknown_field_is_refused():
    """additionalProperties:false everywhere - the executor fails closed too."""
    doc = _build()
    doc["surprise"] = "hello"
    with pytest.raises(jsonschema.ValidationError):
        P.validate(doc)


def test_float_weight_is_refused_by_schema():
    doc = _build()
    doc["targets"][0]["weight"] = 0.4
    with pytest.raises(jsonschema.ValidationError):
        P.validate(doc)


def test_round_trip_through_disk(tmp_path):
    doc = _build()
    path = tmp_path / "plan.json"
    written = P.write(path, doc)
    back = P.read(path)
    assert back == doc
    assert written == P.digest(back)


def test_as_of_horizon_is_explicit():
    """A plan's as_of is the newest bar it may have seen, not 'now'."""
    doc = _build(as_of=AS_OF - timedelta(days=1))
    assert doc["as_of"] == (AS_OF - timedelta(days=1)).isoformat()
