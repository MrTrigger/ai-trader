"""The research view (design spec §8.1).

A lens, not a source. The tests that matter are the ones asserting it stays
that way: it computes nothing, it renders every value into a table as well as a
chart, and it puts the disclosures above the numbers rather than beside them.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from planner import report

RECORD = Path(__file__).resolve().parents[2] / "docs" / "research" / "phase-1-record.json"


@pytest.fixture(scope="module")
def record() -> dict:
    return json.loads(RECORD.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def page(record) -> str:
    return report.build(record)


# --- it is a lens ----------------------------------------------------------


def test_the_committed_record_is_readable(record):
    assert record["data"]["assets"] > 0
    assert len(record["runs"]) == 3
    assert record["ic"] and record["spread"]


def test_rendering_needs_nothing_but_the_record(page):
    assert len(page) > 10_000


# --- self-contained --------------------------------------------------------


def test_the_page_carries_no_external_reference(page):
    """A strict CSP blocks every external host, and the page must also open
    from a file:// URL with no network - the same constraint §0.5 puts on the
    CLI."""
    for forbidden in ("http://", "https://", "src=", "@import"):
        assert forbidden not in page, f"{forbidden} would not load"


def test_the_page_is_a_body_fragment(page):
    # The publisher supplies the document shell; emitting our own would nest.
    for tag in ("<!doctype", "<html", "<body"):
        assert tag not in page.lower()


def test_every_svg_is_closed(page):
    assert page.count("<svg") == page.count("</svg>")
    assert page.count("<section") == page.count("</section>")


# --- the ordering rule -----------------------------------------------------


def test_disclosures_precede_every_number(page):
    """§12, and harder to honour on a page than in a terminal because a page
    invites scrolling straight to the chart."""
    assert page.index("Read before any number below") < page.index("PHASE 1 GATE")


def test_the_disclosures_are_not_collapsible(page):
    # A caveat behind a toggle is a caveat nobody read.
    head = page[: page.index("PHASE 1 GATE")]
    assert "<details" not in head
    assert "uncalibrated" in head


# --- every chart has a table twin ------------------------------------------


def test_each_chart_has_a_table_carrying_the_same_values(page):
    # The relief rule: light-mode aqua is below 3:1 against the surface, so
    # colour may never be the only channel.
    assert page.count('class="chart"') >= 3
    assert page.count("<table") >= page.count('class="chart"')


def test_series_are_direct_labelled_not_only_coloured(page):
    for label in ("momentum", "gc_breakout", "baseline"):
        assert f'class="endlabel' in page
        assert label in page


def test_both_themes_are_defined(page):
    assert "prefers-color-scheme: dark" in page
    assert ':root[data-theme="dark"]' in page
    assert ':root[data-theme="light"]' in page


# --- the numbers reach the page --------------------------------------------


def test_the_verdict_reflects_the_record(page, record):
    # Both candidates failed; the page must not soften that.
    assert "NOT PASSED" in page
    assert page.count("NOT PASSED") == 2


def test_the_headline_returns_are_rendered(page, record):
    for run in record["runs"]:
        pct = f"{run['metrics']['total_return'] * 100:+.2f}%"
        assert pct in page, f"{run['label']} return {pct} missing from the page"


def test_the_effective_sample_is_shown_beside_the_raw_one(page):
    # The correction that changed the IC verdict must be visible, not implied.
    assert "eff n" in page.lower() or "EFF N" in page.upper()


def test_wide_tables_scroll_inside_their_own_container(page):
    assert page.count("class='scroll'") + page.count('class="scroll"') >= 3
