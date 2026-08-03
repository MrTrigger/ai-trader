"""The committed fixture must match what the current code produces.

This is the Python half of the cross-language contract check. If a change to
the Plan shape lands without regenerating the fixture, this fails - and if the
fixture is regenerated without the Rust types being updated, the Rust test
fails. Neither half can move alone, which is the point.

Regenerate deliberately:

    AI_TRADER_UPDATE_FIXTURE=1 pytest tests/test_fixture.py
"""

from __future__ import annotations

import os

import pytest

from planner import fixture
from planner import plan as P


def test_fixture_matches_current_code():
    expected = P.canonical_json(fixture.build())

    if os.environ.get("AI_TRADER_UPDATE_FIXTURE"):
        fixture.write()
        pytest.skip(f"fixture regenerated at {fixture.FIXTURE_PATH}")

    if not fixture.FIXTURE_PATH.exists():
        pytest.fail(
            f"no fixture at {fixture.FIXTURE_PATH}. "
            "Regenerate with AI_TRADER_UPDATE_FIXTURE=1 pytest tests/test_fixture.py"
        )

    actual = fixture.FIXTURE_PATH.read_text(encoding="utf-8")
    assert actual == expected, (
        "the committed fixture no longer matches what the planner produces.\n"
        "If the Plan shape changed on purpose: update service/crates/plan/src/lib.rs "
        "to match, then regenerate with AI_TRADER_UPDATE_FIXTURE=1."
    )


def test_fixture_is_valid_against_the_schema():
    P.validate(fixture.build())


def test_fixture_exercises_every_warning_kind():
    """An added kind must break here, not on a live plan at 03:00.

    Read from the schema rather than the Python dataclass: the schema is what
    both languages agree on, and a kind added there but not mirrored into the
    Rust enum is exactly the drift this fixture exists to catch.
    """
    declared = set(
        P.schema()["properties"]["warnings"]["items"]["properties"]["kind"]["enum"]
    )
    used = {w["kind"] for w in fixture.build()["warnings"]}
    assert used == declared, (
        f"fixture does not exercise every warning kind: missing {sorted(declared - used)}. "
        "Add one to planner/fixture.py so the Rust enum is forced to cover it."
    )


def test_fixture_covers_the_awkward_decimals():
    doc = fixture.build()
    assert doc["nav"]["cash"] == "25000.10", "a value inexact in binary"
    assert doc["orders"][0]["qty"] == "0.39753288", "8dp precision"
    assert doc["targets"][1]["weight"] == "-0.250000", "a negative weight"
    assert doc["nav"]["benchmark_beta"] is None, "a null decimal"
    assert doc["orders"][1]["limit_price"] is None, "market order carries no price"


def test_fixture_records_a_constructor_fallback():
    """A fallback that isn't recorded makes a backtest uninterpretable."""
    p = fixture.build()["provenance"]
    assert p["constructor"] != p["constructor_requested"]
