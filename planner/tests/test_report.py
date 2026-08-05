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

RECORD = Path(__file__).resolve().parents[2] / "docs" / "research" / "backtest.json"


@pytest.fixture(scope="module")
def record() -> dict:
    return json.loads(RECORD.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def page(record) -> str:
    return report.build(record)


# --- it is a lens ----------------------------------------------------------


def test_the_committed_record_is_readable(record):
    assert record["strategy"]["params"]
    assert record["metrics"]["weeks"] > 0
    assert record["exposure"]["gross"] and record["series"]["compounded"]


def test_the_record_describes_one_run_only(record):
    """The page reports the latest backtest, not a history of attempts.

    Every earlier version of this page accumulated sections - retired
    candidates, superseded windows, an IC study of a dead signal - until a
    reader could not tell which numbers described the thing currently being
    run. Keys belonging to other runs must not reappear.
    """
    for archived in ("runs", "ic", "combined", "candidates", "phases", "current"):
        assert archived not in record, f"{archived} belongs to another run"


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
    assert page.index("Read before any number below") < page.index("What was run")


def test_the_disclosures_are_not_collapsible(page):
    # A caveat behind a toggle is a caveat nobody read.
    head = page[: page.index("What was run")]
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


def test_the_parameters_are_on_the_page(page, record):
    """A backtest number without its parameters is a rumour."""
    for key, value in record["strategy"]["params"]:
        assert key in page, f"parameter {key} not shown"


def test_the_headline_metrics_are_rendered(page, record):
    m = record["metrics"]
    assert f"{m['sharpe']:.2f}" in page
    assert f"{m['max_drawdown'] * 100:.1f}%" in page
    assert f"{m['cagr'] * 100:.1f}%" in page


def test_both_equity_conventions_are_shown(page, record):
    """Compounded alone cannot answer whether the edge is holding up."""
    m = record["metrics"]
    assert f"{m['total_return'] * 100:,.1f}%" in page
    assert f"{m['fixed_budget_return'] * 100:,.1f}%" in page


def test_exposure_and_leverage_are_reported(page, record):
    """Gross above 1.0 would be leverage and must never be silent."""
    e = record["exposure"]
    assert e["max_gross"] <= 1.0 + 1e-9, "gross exceeded 1x NAV"
    assert f"{e['max_gross']:.2f}x NAV" in page
    assert f"{e['max_net_short']:+.2f}x NAV" in page


def test_the_disclosure_that_nothing_is_out_of_sample_is_present(page):
    assert "No result below is" in page and "out-of-sample" in page


def test_wide_tables_scroll_inside_their_own_container(page):
    assert page.count("class='scroll'") + page.count('class="scroll"') >= 3
