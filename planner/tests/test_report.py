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
    assert record["current"]["decomposition"] and record["spread"]
    # The page is the current state, not a log. Sections describing retired
    # candidates belong in docs/phase-1-findings.md and must not creep back.
    for archived in ("runs", "ic", "combined"):
        assert archived not in record, f"{archived} is history, not current state"


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
    assert page.index("Read before any number below") < page.index("Current result")


def test_the_disclosures_are_not_collapsible(page):
    # A caveat behind a toggle is a caveat nobody read.
    head = page[: page.index("Current result")]
    assert "<details" not in head
    assert "uncalibrated" in head


# --- every chart has a table twin ------------------------------------------


def test_each_chart_has_a_table_carrying_the_same_values(page):
    """Every charted quantity is also readable as numbers.

    Counting tables against charts was the old form of this and it broke as
    soon as one table served a stacked pair sharing an x-axis. The property
    that actually matters is that no SECTION shows a chart without also
    carrying a table, which is what a reader who cannot separate the hues
    needs.
    """
    import re

    for section in re.findall(r"<section\b.*?</section>", page, re.S):
        if 'class="chart"' not in section:
            continue
        assert "<table" in section, (
            "a section charts values without tabulating them: "
            + re.sub(r"<[^>]+>", " ", section)[:120]
        )


def test_series_are_direct_labelled_not_only_coloured(page):
    import re

    for svg in re.findall(r"<svg\b.*?</svg>", page, re.S):
        lines = re.findall(r'class="line (s\d+)"', svg)
        labels = re.findall(r'class="endlabel s\d+"', svg)
        assert len(labels) >= len(lines), "a series carries colour but no name"


def test_no_series_is_drawn_without_a_defined_colour(page):
    """Six series against four defined hues left two lines unstyled once."""
    import re

    for svg in re.findall(r"<svg\b.*?</svg>", page, re.S):
        for cls in set(re.findall(r'class="line (s\d+)"', svg)):
            assert f".line.{cls}" in page, f"{cls} has no colour rule"


def test_end_labels_do_not_overprint(page):
    """Two series finishing at similar values used to stack their labels."""
    import re

    for svg in re.findall(r"<svg\b.*?</svg>", page, re.S):
        ys = sorted(
            float(y)
            for y in re.findall(r'<text x="[\d.]+" y="([\d.]+)" class="endlabel', svg)
        )
        for a, b in zip(ys, ys[1:]):
            assert b - a >= 12, f"labels {a:.0f} and {b:.0f} overlap"


def test_both_themes_are_defined(page):
    assert "prefers-color-scheme: dark" in page
    assert ':root[data-theme="dark"]' in page
    assert ':root[data-theme="light"]' in page


# --- the numbers reach the page --------------------------------------------


def test_the_open_questions_are_not_softened(page):
    """The page states what is unsettled, above the fold of its own section."""
    for unresolved in ("parameters were chosen on a single phase", "null test"):
        assert unresolved in page


def test_the_headline_returns_are_rendered(page, record):
    for row in record["current"]["decomposition"]:
        pct = f"{row['orig']['final'] * 100:.1f}%"
        assert pct in page, f"{row['name']} return {pct} missing from the page"


def test_the_phase_spread_is_shown_not_just_the_median(page, record):
    """A single-phase number is one draw; the range has to be visible."""
    rows = record["phases"]["rows"]
    for extreme in (min(r["combined"] for r in rows), max(r["combined"] for r in rows)):
        assert f"{extreme * 100:.1f}%" in page


def test_the_effective_sample_is_shown_beside_the_raw_one(page):
    # The correction that changed the IC verdict must be visible, not implied.
    assert "eff n" in page.lower() or "EFF N" in page.upper()


def test_wide_tables_scroll_inside_their_own_container(page):
    assert page.count("class='scroll'") + page.count('class="scroll"') >= 3
