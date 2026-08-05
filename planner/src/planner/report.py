"""The research view: a lens over what `backtest`, `gate` and `ic` produced.

Design spec §8.1 — *"Interactive surfaces are lenses over that CLI, never a
dependency of it."* Nothing here computes a result. It renders a record that the
CLI produced, and deleting this module would cost the project a convenience and
no evidence.

**This is not the Phase 5 UI.** §9.2 defers a UI over *operations* — positions,
NAV, fills, alerts — until there is a book to display. This is a view over
*research output*, which exists now and is what the current work is actually
made of. The distinction matters because the two have different consumers and
the operational one has different failure modes.

## The one rule the layout obeys

Disclosures first, above every number (§12). On a page rather than a terminal
that means the banner is the first thing in the document and is not collapsible
— a caveat behind a toggle is a caveat that was not read.

## Self-contained by construction

Inline SVG, inline CSS, inline JS, no external anything. The page has to survive
being opened from a file:// URL on a laptop with no network, which is the same
constraint §0.5 puts on the CLI.
"""

from __future__ import annotations

import html
import json
from dataclasses import dataclass
from datetime import date
from pathlib import Path

# --- palette ----------------------------------------------------------------
# Validated with the dataviz validator in both modes:
#   light  worst adjacent CVD ΔE 9.2, normal-vision 27.6
#   dark   worst adjacent CVD ΔE 9.4, normal-vision 26.5
# Light-mode aqua sits at 2.74:1 against the surface, below the 3:1 line, so the
# relief rule applies: every series carries a visible direct label AND a table
# view, and colour never carries identity alone.
SERIES_LIGHT = ("#2a78d6", "#eb6834", "#1baf7a", "#eda100")
SERIES_DARK = ("#3987e5", "#d95926", "#199e70", "#d99000")

#: Minimum vertical separation between two end labels, in SVG units. Below this
#: they overprint and the reader cannot tell which line carries which name.
_LABEL_MIN_GAP = 15.0


@dataclass(frozen=True)
class Chart:
    width: int = 720
    height: int = 300
    pad_left: int = 62
    pad_right: int = 96
    pad_top: int = 16
    pad_bottom: int = 34

    @property
    def plot_w(self) -> int:
        return self.width - self.pad_left - self.pad_right

    @property
    def plot_h(self) -> int:
        return self.height - self.pad_top - self.pad_bottom


def _esc(text: object) -> str:
    return html.escape(str(text))


def _pct(value: float, places: int = 2) -> str:
    return f"{value * 100:+.{places}f}%"


def _nice_ticks(lo: float, hi: float, count: int = 5) -> list[float]:
    if hi <= lo:
        return [lo]
    raw = (hi - lo) / count
    magnitude = 10 ** (len(str(int(abs(raw)))) - 1) if abs(raw) >= 1 else 0.1
    step = max(magnitude, round(raw / magnitude) * magnitude)
    start = int(lo / step) * step
    out, v = [], start
    while v <= hi + step * 0.5:
        if v >= lo - step * 0.5:
            out.append(v)
        v += step
    return out or [lo, hi]


# --- NAV chart --------------------------------------------------------------


def nav_chart(runs: list[dict], folds: list[dict]) -> str:
    """Equity curves, with walk-forward test windows shaded behind them.

    The fold bands are the reason this chart exists rather than a table: an
    out-of-sample window that carried the result looks obvious here and is
    invisible in a summary row.
    """
    c = Chart(height=340)
    series = [(r["label"], r["nav"]) for r in runs]
    dates = [d for _, nav in series for d, _ in nav]
    values = [v for _, nav in series for _, v in nav]
    if not values:
        return "<p>no data</p>"

    lo, hi = 0.0, max(values) * 1.05
    xs = sorted({d for d in dates})
    index = {d: i for i, d in enumerate(xs)}

    def px(d: str) -> float:
        return c.pad_left + (index[d] / max(1, len(xs) - 1)) * c.plot_w

    def py(v: float) -> float:
        return c.pad_top + c.plot_h - ((v - lo) / (hi - lo)) * c.plot_h

    parts = [
        f'<svg viewBox="0 0 {c.width} {c.height}" role="img" '
        f'aria-label="NAV over the replay window for each candidate and the baseline" '
        f'class="chart" preserveAspectRatio="xMidYMid meet">'
    ]

    # Fold bands first, behind everything.
    for i, f in enumerate(folds):
        if not f.get("start") or f["start"] not in index or f["end"] not in index:
            continue
        x0, x1 = px(f["start"]), px(f["end"])
        parts.append(
            f'<rect x="{x0:.1f}" y="{c.pad_top}" width="{max(1.0, x1 - x0):.1f}" '
            f'height="{c.plot_h}" class="fold"/>'
        )
        parts.append(
            f'<text x="{(x0 + x1) / 2:.1f}" y="{c.pad_top + 12}" '
            f'class="fold-label" text-anchor="middle">OOS {i}</text>'
        )

    for tick in _nice_ticks(lo, hi):
        y = py(tick)
        parts.append(
            f'<line x1="{c.pad_left}" y1="{y:.1f}" x2="{c.pad_left + c.plot_w}" '
            f'y2="{y:.1f}" class="grid"/>'
        )
        parts.append(
            f'<text x="{c.pad_left - 8}" y="{y + 4:.1f}" class="tick" '
            f'text-anchor="end">{tick / 1000:.0f}k</text>'
        )

    step = max(1, len(xs) // 6)
    for i in range(0, len(xs), step):
        parts.append(
            f'<text x="{px(xs[i]):.1f}" y="{c.height - 10}" class="tick" '
            f'text-anchor="middle">{xs[i][:7]}</text>'
        )

    for slot, (label, nav) in enumerate(series):
        pts = " ".join(f"{px(d):.1f},{py(v):.1f}" for d, v in nav)
        parts.append(
            f'<polyline points="{pts}" class="line s{slot + 1}" fill="none"/>'
        )
        if nav:
            d, v = nav[-1]
            parts.append(
                f'<circle cx="{px(d):.1f}" cy="{py(v):.1f}" r="3.5" class="dot s{slot + 1}"/>'
            )
            # Direct label: identity never rests on colour alone (relief rule).
            parts.append(
                f'<text x="{px(d) + 9:.1f}" y="{py(v) + 4:.1f}" '
                f'class="endlabel s{slot + 1}">{_esc(label)}</text>'
            )

    parts.append(
        f'<line x1="{c.pad_left}" y1="{c.pad_top + c.plot_h}" '
        f'x2="{c.pad_left + c.plot_w}" y2="{c.pad_top + c.plot_h}" class="axis"/>'
    )
    # Crosshair layer: an SVG chart is interactive, so ship the hover.
    parts.append(
        f'<line class="xhair" id="xh" y1="{c.pad_top}" y2="{c.pad_top + c.plot_h}"/>'
    )
    parts.append(
        f'<rect x="{c.pad_left}" y="{c.pad_top}" width="{c.plot_w}" height="{c.plot_h}" '
        f'fill="transparent" id="navhit"/>'
    )
    parts.append("</svg>")
    parts.append(
        "<script>"
        + _NAV_JS
        % {
            "dates": json.dumps(xs),
            "series": json.dumps(
                [
                    {"label": lab, "vals": [v for _, v in nav]}
                    for lab, nav in series
                ]
            ),
            "left": c.pad_left,
            "w": c.plot_w,
        }
        + "</script>"
    )
    return "".join(parts)


_NAV_JS = """
(function(){
  var dates=%(dates)s, series=%(series)s, left=%(left)s, w=%(w)s;
  var wrap=document.getElementById('navwrap'), tip=document.getElementById('navtip'),
      xh=document.getElementById('xh'), hit=document.getElementById('navhit'),
      svg=hit.ownerSVGElement, vb=svg.viewBox.baseVal;
  function at(evt){
    var r=svg.getBoundingClientRect(), scale=vb.width/r.width;
    var vx=(evt.clientX-r.left)*scale;
    var i=Math.round((vx-left)/w*(dates.length-1));
    if(i<0)i=0; if(i>dates.length-1)i=dates.length-1;
    var x=left+i/(dates.length-1)*w;
    xh.setAttribute('x1',x); xh.setAttribute('x2',x); xh.classList.add('on');
    var rows=series.map(function(s,k){
      var v=s.vals[i];
      return '<div><i style="background:var(--s'+(k+1)+')"></i>'+s.label+
             ' <b>'+(v==null?'-':Math.round(v).toLocaleString())+'</b></div>';
    }).join('');
    tip.innerHTML='<div style="color:var(--muted)">'+dates[i]+'</div>'+rows;
    tip.classList.add('on');
    tip.style.left=(x/vb.width*100)+'%%';
    tip.style.top=(r.height*0.42)+'px';
  }
  hit.addEventListener('pointermove',at);
  hit.addEventListener('pointerleave',function(){
    tip.classList.remove('on'); xh.classList.remove('on');
  });
})();
"""


# --- bar chart for a signed statistic ---------------------------------------


def signed_bars(
    rows: list[tuple[str, float, str]], *, unit: str = "", title_id: str = ""
) -> str:
    """Horizontal bars around a zero line. One series, one colour.

    Used for IC and for the spread test: both are signed statistics where the
    question is "is this distinguishable from nothing", so the zero line is the
    subject of the chart and gets the emphasis.
    """
    if not rows:
        return "<p>no data</p>"
    width, row_h, pad_l, pad_r = 640, 42, 92, 132
    height = len(rows) * row_h + 28
    span = max(abs(v) for _, v, _ in rows) * 1.35 or 1.0
    mid = pad_l + (width - pad_l - pad_r) / 2
    half = (width - pad_l - pad_r) / 2

    parts = [
        f'<svg viewBox="0 0 {width} {height}" role="img" class="chart" '
        f'aria-labelledby="{title_id}" preserveAspectRatio="xMidYMid meet">'
    ]
    for i, (label, value, note) in enumerate(rows):
        y = 14 + i * row_h
        w = abs(value) / span * half
        x = mid if value >= 0 else mid - w
        parts.append(
            f'<text x="{pad_l - 12}" y="{y + 19}" class="tick" text-anchor="end">'
            f"{_esc(label)}</text>"
        )
        parts.append(
            f'<rect x="{x:.1f}" y="{y + 6}" width="{max(2.0, w):.1f}" height="16" '
            f'rx="4" class="bar {"pos" if value >= 0 else "neg"}"/>'
        )
        anchor = "start" if value >= 0 else "end"
        tx = mid + w + 10 if value >= 0 else mid - w - 10
        parts.append(
            f'<text x="{tx:.1f}" y="{y + 19}" class="barval" text-anchor="{anchor}">'
            f"{value:+.4f}{unit}</text>"
        )
        parts.append(
            f'<text x="{width - 8}" y="{y + 19}" class="barnote" text-anchor="end">'
            f"{_esc(note)}</text>"
        )
    parts.append(
        f'<line x1="{mid}" y1="8" x2="{mid}" y2="{height - 14}" class="zero"/>'
    )
    parts.append("</svg>")
    return "".join(parts)


# --- universe over time -----------------------------------------------------


def universe_chart(points: list[dict]) -> str:
    if not points:
        return "<p>no data</p>"
    c = Chart(height=210, pad_right=110)
    hi = max(max(p["eligible"], p["dead"]) for p in points) * 1.15

    def px(i: int) -> float:
        return c.pad_left + (i / max(1, len(points) - 1)) * c.plot_w

    def py(v: float) -> float:
        return c.pad_top + c.plot_h - (v / hi) * c.plot_h

    parts = [
        f'<svg viewBox="0 0 {c.width} {c.height}" role="img" class="chart" '
        f'aria-label="Eligible assets and delisted assets carried, over time" '
        f'preserveAspectRatio="xMidYMid meet">'
    ]
    for tick in _nice_ticks(0, hi, 4):
        y = py(tick)
        parts.append(
            f'<line x1="{c.pad_left}" y1="{y:.1f}" x2="{c.pad_left + c.plot_w}" '
            f'y2="{y:.1f}" class="grid"/>'
        )
        parts.append(
            f'<text x="{c.pad_left - 8}" y="{y + 4:.1f}" class="tick" '
            f'text-anchor="end">{tick:.0f}</text>'
        )
    for key, slot, label in (("eligible", 1, "eligible"), ("dead", 2, "delisted held")):
        pts = " ".join(f"{px(i):.1f},{py(p[key]):.1f}" for i, p in enumerate(points))
        parts.append(f'<polyline points="{pts}" class="line s{slot}" fill="none"/>')
        parts.append(
            f'<text x="{px(len(points) - 1) + 9:.1f}" y="{py(points[-1][key]) + 4:.1f}" '
            f'class="endlabel s{slot}">{label}</text>'
        )
    step = max(1, len(points) // 6)
    for i in range(0, len(points), step):
        parts.append(
            f'<text x="{px(i):.1f}" y="{c.height - 10}" class="tick" '
            f'text-anchor="middle">{points[i]["date"][:7]}</text>'
        )
    parts.append(
        f'<line x1="{c.pad_left}" y1="{c.pad_top + c.plot_h}" '
        f'x2="{c.pad_left + c.plot_w}" y2="{c.pad_top + c.plot_h}" class="axis"/>'
    )
    parts.append("</svg>")
    return "".join(parts)


# --- combined equity, and what it cost to get there -------------------------


def _line_panel(
    series: list[tuple[str, list]],
    *,
    height: int,
    fmt,
    ticks_from,
    split: str | None,
    aria: str,
    baseline: float | None = None,
) -> str:
    """One shared helper for both panels so their geometry cannot drift apart.

    They sit above one another sharing an x-axis, so a mismatch in padding or
    date mapping would silently misalign a drawdown from the equity that caused
    it — which is exactly the comparison the pair exists to make.
    """
    c = Chart(height=height, pad_right=118)
    xs = sorted({d for _, pts in series for d, _ in pts})
    if not xs:
        return "<p>no data</p>"
    index = {d: i for i, d in enumerate(xs)}
    values = [v for _, pts in series for _, v in pts]
    lo, hi = ticks_from(min(values), max(values))

    def px(d):
        return c.pad_left + (index[d] / max(1, len(xs) - 1)) * c.plot_w

    def py(v):
        return c.pad_top + c.plot_h - ((v - lo) / (hi - lo)) * c.plot_h

    out = [
        f'<svg viewBox="0 0 {c.width} {c.height}" role="img" class="chart" '
        f'aria-label="{_esc(aria)}" preserveAspectRatio="xMidYMid meet">'
    ]
    for t in _nice_ticks(lo, hi, 4):
        y = py(t)
        out.append(
            f'<line x1="{c.pad_left}" y1="{y:.1f}" x2="{c.pad_left + c.plot_w}" '
            f'y2="{y:.1f}" class="grid"/>'
        )
        out.append(
            f'<text x="{c.pad_left - 8}" y="{y + 4:.1f}" class="tick" '
            f'text-anchor="end">{fmt(t)}</text>'
        )
    if baseline is not None and lo <= baseline <= hi:
        out.append(
            f'<line x1="{c.pad_left}" y1="{py(baseline):.1f}" '
            f'x2="{c.pad_left + c.plot_w}" y2="{py(baseline):.1f}" class="axis"/>'
        )

    # The regime boundary. The whole finding is that the two sides differ.
    if split and split in index:
        x = px(split)
        out.append(
            f'<line x1="{x:.1f}" y1="{c.pad_top}" x2="{x:.1f}" '
            f'y2="{c.pad_top + c.plot_h}" class="split"/>'
        )

    if len(series) > len(SERIES_LIGHT):
        raise ValueError(
            f"{len(series)} series but only {len(SERIES_LIGHT)} validated colours. "
            "Cycling hues would give two series the same colour; drop a series or "
            "extend the palette and re-run the CVD check."
        )

    for slot, (label, pts) in enumerate(series):
        d = " ".join(f"{px(a):.1f},{py(b):.1f}" for a, b in pts)
        out.append(f'<polyline points="{d}" class="line s{slot + 1}" fill="none"/>')
        a, b = pts[-1]
        out.append(
            f'<circle cx="{px(a):.1f}" cy="{py(b):.1f}" r="3.5" class="dot s{slot + 1}"/>'
        )

    # End labels, de-collided. Two series that finish at similar values put their
    # labels in the same place and overprint, which is worse than no label - the
    # reader cannot tell which line is which and may read the wrong series
    # entirely. Nudged apart vertically, with a leader implied by the dot each
    # already has.
    ends = sorted(
        ((py(pts[-1][1]), px(pts[-1][0]), slot, label) for slot, (label, pts) in enumerate(series)),
        key=lambda e: e[0],
    )
    placed: list[float] = []
    for y, x, slot, label in ends:
        target = y
        while any(abs(target - other) < _LABEL_MIN_GAP for other in placed):
            target += _LABEL_MIN_GAP / 2
        target = min(target, c.pad_top + c.plot_h)
        placed.append(target)
        out.append(
            f'<text x="{x + 9:.1f}" y="{target + 4:.1f}" '
            f'class="endlabel s{slot + 1}">{_esc(label)}</text>'
        )

    step = max(1, len(xs) // 6)
    for i in range(0, len(xs), step):
        out.append(
            f'<text x="{px(xs[i]):.1f}" y="{c.height - 10}" class="tick" '
            f'text-anchor="middle">{xs[i][:7]}</text>'
        )
    out.append(
        f'<line x1="{c.pad_left}" y1="{c.pad_top + c.plot_h}" '
        f'x2="{c.pad_left + c.plot_w}" y2="{c.pad_top + c.plot_h}" class="axis"/>'
    )
    out.append("</svg>")
    return "".join(out)


def combined_section(record: dict) -> str:
    """Equity above, drawdown below, sharing an x-axis.

    Both panels are necessary and neither is sufficient. Equity alone says BTC
    won; drawdown alone says the neutral book was far safer. The pair is the
    actual finding, and giving each its own chart on one axis — never two scales
    on one plot — is what keeps the comparison honest.
    """
    series = record["series"]
    eq = [(s["label"], s["equity"]) for s in series]
    dd = [(s["label"], s["drawdown"]) for s in series]
    split = record.get("split")

    equity = _line_panel(
        eq,
        height=300,
        split=split,
        fmt=lambda v: f"{v:.0f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="Growth of one unit: market-neutral book versus buy and hold BTC",
        baseline=1.0,
    )
    draw = _line_panel(
        dd,
        height=190,
        split=split,
        fmt=lambda v: f"{v * 100:.0f}%",
        ticks_from=lambda a, b: (a * 1.08, 0.0),
        aria="Drawdown from peak for each series",
        baseline=0.0,
    )

    rows = "".join(
        f"<tr><td>{_esc(w['name'])}</td><td>{_esc(w['from'])} to {_esc(w['to'])}</td>"
        f"<td class='num'>{_pct(w['strategy'])}</td>"
        f"<td class='num'>{w['sharpe']:.2f}</td>"
        f"<td class='num'>{_pct(w['btc'])}</td>"
        f"<td class='num'>{w['btc_sharpe']:.2f}</td>"
        f"<td>{'strategy' if w['strategy'] > w['btc'] else 'BTC'}</td></tr>"
        for w in record["windows"]
    )
    totals = "".join(
        f"<tr><td>{_esc(s['label'])}</td><td class='num'>{_pct(s['final'])}</td>"
        f"<td class='num'>{s['maxdd'] * 100:.1f}%</td></tr>"
        for s in series
    )

    return (
        '<section><h2>Combined equity, both windows</h2>'
        '<p class="note">Growth of one unit from ' + _esc(record["window"][0]) + ', the two '
        'test windows chained. The vertical rule at ' + _esc(str(split)) + ' is the boundary '
        'between them — left of it is the fresh out-of-sample window, right is the original. '
        '<strong>Both series are indexed to the same base on one axis</strong>; a second '
        'scale would invent a relationship that is not in the data.</p>'
        + equity
        + '<p class="note" style="margin-top:14px">Drawdown from peak, same x-axis. This is '
        'the half the equity curve hides.</p>'
        + draw
        + '<div class="scroll"><table class="data"><thead><tr><th>window</th><th>span</th>'
        '<th class="num">strategy</th><th class="num">Sharpe</th><th class="num">BTC</th>'
        '<th class="num">Sharpe</th><th>winner</th></tr></thead><tbody>' + rows
        + '</tbody></table></div>'
        '<div class="scroll"><table class="data"><thead><tr><th>full period</th>'
        '<th class="num">total return</th><th class="num">max drawdown</th></tr></thead>'
        '<tbody>' + totals + '</tbody></table></div>'
        '<p class="finding"><strong>Read the phase-robustness section below before drawing '
        'anything from this chart.</strong> Every curve here is one rebalance phase - the '
        'Friday one - and the choice of weekday is arbitrary: nothing in the strategy refers '
        'to it. Across the seven possible phases the same strategy ranges from strongly '
        'profitable to loss-making, and Friday is the best of the seven. These curves are '
        'kept because they are what the research actually produced and deleting them would '
        'hide how the error was made, but they describe one draw and not the strategy.</p>'
        '</section>'
    )


def current_section(record: dict) -> str:
    """What the current best version does. The page's reason to exist.

    Everything here is tranched across all seven rebalance phases, which is not
    a presentation choice: a single-phase number for a weekly strategy is one
    draw from a distribution whose spread, on this strategy, ranged from -57% to
    +1177% over the same period. Tranching removes the choice by holding all
    seven sub-books at once, which is also how it would be run.
    """
    eq = [(s["label"], s["equity"]) for s in record["series"]]
    dd = [(s["label"], s["drawdown"]) for s in record["series"]]
    equity = _line_panel(
        eq, height=300, split=record.get("split"), fmt=lambda v: f"{v:.0f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="Growth of one unit: current strategy versus buy and hold BTC",
        baseline=1.0,
    )
    draw = _line_panel(
        dd, height=190, split=record.get("split"), fmt=lambda v: f"{v * 100:.0f}%",
        ticks_from=lambda a, b: (a * 1.08, 0.0),
        aria="Drawdown from peak", baseline=0.0,
    )

    rows = ""
    for d in record["decomposition"]:
        strong = d["name"] == "strategy"
        rows += (
            f'<tr{" class=\'lead\'" if strong else ""}><td>{_esc(d["name"])}</td>'
            f'<td class="num">{d["fresh"]["final"] * 100:.1f}%</td>'
            f'<td class="num">{d["fresh"]["sharpe"]:.2f}</td>'
            f'<td class="num">{d["orig"]["final"] * 100:.1f}%</td>'
            f'<td class="num">{d["orig"]["sharpe"]:.2f}</td>'
            f'<td class="num">{d["orig"]["t"]:.2f}</td>'
            f'<td class="num">{d["combined"] * 100:.1f}%</td></tr>'
        )

    strat = next(d for d in record["decomposition"] if d["name"] == "strategy")
    btc = next(d for d in record["decomposition"] if d["name"] == "BTC buy & hold")

    return (
        '<section><h2>Current result</h2>'
        '<p class="note">Long the names above their Gaussian channel, short the rest of the '
        'eligible and borrowable universe, leg weights tilted by the benchmark\'s own regime '
        'read, capped at twelve positions and 25% a name so the risk gate accepts the plan. '
        '<strong>Tranched across all seven rebalance phases</strong> — seven sub-books, one '
        'per weekday, a seventh of capital each. That is not a smoothing choice: on a single '
        'phase this same strategy ranged from −57% to +1177% over the same period, and '
        'tranching removes an arbitrary parameter rather than averaging away a real one.</p>'
        + equity
        + '<p class="note" style="margin-top:14px">Drawdown from peak, same axis. This is the '
        'half the equity curve hides, and it is where the strategy earns its keep.</p>'
        + draw
        + '<h3>What the return is made of</h3>'
        '<p class="note">The strategy has two independent parts and either could be carrying '
        'it. Run separately, each is weaker than the pair — which is the answer to "is this '
        'just BTC timing in a costume".</p>'
        '<div class="scroll"><table class="data"><thead>'
        '<tr><th rowspan="2">variant</th><th colspan="2">fresh 2019-10..2021-10</th>'
        '<th colspan="3">orig 2021-10..2026-08</th><th rowspan="2" class="num">combined</th></tr>'
        '<tr><th class="num">return</th><th class="num">Sharpe</th>'
        '<th class="num">return</th><th class="num">Sharpe</th><th class="num">t</th></tr>'
        '</thead><tbody>' + rows + '</tbody></table></div>'
        '<p class="finding"><strong>It beats buy-and-hold on the combined window and on '
        f'drawdown, and loses to it in the first window.</strong> {strat["combined"] * 100:.0f}% '
        f'against {btc["combined"] * 100:.0f}%, with a worst drawdown of '
        f'{min(s["maxdd"] for s in record["series"] if s["label"] == "strategy") * 100:.0f}% '
        f'against BTC\'s {min(s["maxdd"] for s in record["series"] if s["label"] != "strategy") * 100:.0f}%. '
        'But BTC wins the fresh window outright on both return and Sharpe, so the case rests '
        'on the second window and on the drawdown, not on a clean sweep. Neither component '
        'explains the whole: timing alone is weak (Sharpe 0.52 and 0.28), selection alone is '
        'moderate, and the pair beats both. That rules out the simplest deflationary story — '
        'this is not a BTC market-timing strategy wearing a long/short costume — without '
        'establishing that what remains is durable.</p></section>'
    )


def phase_section(record: dict) -> str:
    """The seven rebalance phases, and why the headline is tranched.

    A weekly strategy has seven equally valid start days and nothing in this one
    refers to a weekday: shifting the rebalance by three days changes no
    parameter, no rule and no data. A real weekly edge should barely notice. This
    one swings from +63% to +7860% combined, which is why every figure on this
    page holds all seven at once rather than picking.

    The table is sorted by outcome rather than by weekday, because the point is
    the spread and not which day won. The chart shows best, median and worst
    against the tranched book - four series, which is the palette's limit and
    also the most a reader can follow.
    """
    rows = ""
    for r in record["rows"]:
        rows += (
            f'<tr><td class="mono-s">{_esc(r["phase"])}</td>'
            f'<td class="num">{r["fresh"] * 100:.1f}%</td>'
            f'<td class="num">{r["orig"] * 100:.1f}%</td>'
            f'<td class="num">{r["sharpe"]:.2f}</td>'
            f'<td class="num">{r["maxdd"] * 100:.1f}%</td>'
            f'<td class="num">{r["combined"] * 100:.1f}%</td></tr>'
        )
    t = record["tranched"]
    rows += (
        f'<tr class="lead"><td class="mono-s">tranched</td>'
        f'<td class="num muted">—</td><td class="num muted">—</td>'
        f'<td class="num muted">—</td>'
        f'<td class="num">{t["maxdd"] * 100:.1f}%</td>'
        f'<td class="num">{t["final"] * 100:.1f}%</td></tr>'
    )

    chart = _line_panel(
        [(s["label"], s["equity"]) for s in record["series"]],
        height=300, split=record.get("split"), fmt=lambda v: f"{v:.0f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="Best, median and worst rebalance phase against the tranched book",
        baseline=1.0,
    )

    return (
        '<section><h2>Why the numbers above are tranched</h2>'
        '<p class="note">Nothing in this strategy refers to a weekday. Shifting the rebalance '
        'by three days changes no parameter, no rule and no data — only which seven-day '
        'windows the returns are cut into. A real weekly edge should barely notice.</p>'
        '<div class="scroll"><table class="data"><thead>'
        '<tr><th>rebalance day</th><th class="num">fresh</th><th class="num">orig</th>'
        '<th class="num">Sharpe</th><th class="num">maxDD</th>'
        '<th class="num">combined</th></tr></thead>'
        '<tbody>' + rows + '</tbody></table></div>'
        + chart
        + '<p class="finding"><strong>The same strategy, on the same data, returns +63% or '
        '+7860% depending on an arbitrary choice.</strong> That spread is far larger than the '
        'effect of any parameter in the strategy, which is why a single-phase number is not a '
        'result — it is one draw. Holding all seven at once is both the honest measurement and '
        'the better book: the tranched drawdown of '
        f'{record["tranched"]["maxdd"] * 100:.1f}% is shallower than <em>every</em> individual '
        'phase, including the luckiest, because the sub-books are only about a third '
        'correlated and their worst weeks do not coincide. It costs nothing — each tranche '
        'carries the same turnover as a single-phase book.</p>'
        '<p class="note">This is also how the earlier headline result was wrong. Every '
        'validation the strategy passed — a fresh out-of-sample window, walk-forward folds, '
        'plateau sweeps, a label-shuffle null — was run on the Friday phase, the best of the '
        'seven. An unrecognised degree of freedom is not protected against by testing the ones '
        'you recognised.</p></section>'
    )


def fixed_budget_section(record: dict) -> str:
    """The same weekly returns under both equity conventions, on one x-axis.

    A compounding curve answers "what did the account do" and is the only honest
    answer to that question. It is a poor instrument for "is the edge holding
    up", because it multiplies each week's return by however much the account
    happens to have grown — so a decaying edge on a large balance still slopes
    upward, and an early loss is invisible next to a later one of half the
    severity. Applying the identical returns to a constant stake removes the
    balance from the picture and leaves the edge itself: slope IS performance,
    and a flattening slope is a fading signal rather than a smaller account.

    Both are plotted because neither alone is sufficient, and never on two
    y-scales — they are the same quantity under two conventions, so they share
    one axis and the divergence between them is the point.
    """
    comp = [(d, v) for d, v in record["compounded"]]
    fixed = [(d, v) for d, v in record["fixed"]]
    split = record.get("split")

    # Separate panels, not one plot: 64x and 5.7x on a shared scale would flatten
    # the fixed-budget line into the axis and destroy the only thing it shows.
    grown = _line_panel(
        [("compounded", comp)],
        height=250,
        split=split,
        fmt=lambda v: f"{v:.0f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="Compounded equity: each week's return applied to the running balance",
        baseline=1.0,
    )
    flat = _line_panel(
        [("fixed budget", fixed)],
        height=250,
        split=split,
        fmt=lambda v: f"{v:.1f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="Fixed-budget equity: the same returns applied to a constant stake",
        baseline=1.0,
    )

    years = "".join(
        f"<tr><td>{y['year']}</td><td class='num'>{y['pnl'] * 100:+.1f}%</td></tr>"
        for y in record["years"]
    )

    return (
        '<section><h2>Compounding, removed</h2>'
        '<p class="note">The chart above shows what the <em>account</em> did. Position sizes '
        'are fractions of NAV, so every dollar earned is redeployed and the curve is '
        'exponential by construction — which makes it a bad instrument for asking whether the '
        '<em>edge</em> is holding up. The pair below is the same series of weekly returns '
        'under both conventions.</p>'
        '<p class="note"><strong>Compounded</strong> — <code>equity ×= 1 + r</code>. '
        'What the account actually does.</p>'
        + grown
        + '<p class="note" style="margin-top:14px"><strong>Fixed budget</strong> — '
        '<code>equity += r</code>, the identical weekly returns on a constant stake. '
        'Here slope <em>is</em> performance: a straight line means a steady edge, and a '
        'flattening one means a fading edge rather than a smaller account.</p>'
        + flat
        + '<div class="scroll"><table class="data"><thead><tr><th>year</th>'
        '<th class="num">P&amp;L, in units of the fixed stake</th></tr></thead>'
        '<tbody>' + years + '</tbody></table></div>'
        '<p class="finding"><strong>The two lines nearly coincide, and that is the finding.</strong> '
        'On the median rebalance phase the strategy roughly doubles either way — compounding '
        'adds almost nothing over seven years, because compounding only compounds when returns '
        'are consistently positive, and here two of the seven years are negative. The earlier '
        'version of this panel showed a steep exponential and reported a decaying edge; both '
        'were properties of the Friday phase rather than of the strategy. On the median phase '
        'the year-by-year P&amp;L has no trend at all — 2022 is the worst year and 2025 the '
        'best, which is the reverse of the earlier reading. That reversal is worth more than '
        'either result: a story confidently derived from one phase came out backwards on '
        'another, so the fixed-budget view corrects the compounding illusion but cannot rescue '
        'a number measured on a single arbitrary phase.</p></section>'
    )


def candidates_table(cands: list[dict]) -> str:
    """Every candidate, with how much search preceded it.

    The `searched` column is the one that matters and it is why this is a table
    rather than a leaderboard. Sorting by return would put the least trustworthy
    row on top: a result found after eighty-eight configurations on two windows
    is not the same kind of evidence as one hypothesis tested once, and a table
    that does not say so is actively misleading. Rows are therefore in the order
    they were *reached*, not the order they score.
    """
    def cell(v, pct=True, places=1):
        if v is None:
            return '<td class="num muted">—</td>'
        return f'<td class="num">{v * 100:.{places}f}%</td>' if pct else f'<td class="num">{v:.2f}</td>'

    body = ""
    for c in cands:
        # Grade by what SURVIVED, not by how much search preceded it. A result
        # reached after a long search but which a matched null cannot reproduce
        # is different from one that has never been falsification-tested.
        ev = c["evidence"]
        trust = ("ok" if ("clean OOS" in ev or "survives null" in ev)
                 else ("thin" if c["configs"] >= 16 else "ok"))
        body += (
            f'<tr class="shape-{_esc(c["shape"])}">'
            f'<td>{_esc(c["name"])}</td>'
            f'<td class="mono-s">{_esc(c["shape"])}</td>'
            + cell(c["fresh"]) + cell(c["fs"], pct=False) + cell(c["fdd"])
            + cell(c["orig"]) + cell(c["os"], pct=False) + cell(c["odd"])
            + cell(c["combined"])
            + f'<td class="num">{c["configs"]}</td>'
            f'<td><span class="ev ev-{trust}">{_esc(c["evidence"])}</span></td>'
            f'<td class="note-cell">{_esc(c["note"])}</td></tr>'
        )

    return (
        '<section><h2>Every candidate</h2>'
        '<p class="note">In the order they were reached, not the order they score. '
        '<strong>The <em>searched</em> column is the one to read first</strong> — a result '
        'found after eighty-eight configurations on two windows is not the same kind of '
        'evidence as one hypothesis tested once, and sorting by return would put the least '
        'trustworthy row on top.</p>'
        '<div class="scroll"><table class="data cands"><thead>'
        '<tr><th rowspan="2">candidate</th><th rowspan="2">shape</th>'
        '<th colspan="3">fresh 2019-10..2021-10</th>'
        '<th colspan="3">orig 2021-10..2026-08</th>'
        '<th rowspan="2" class="num">combined</th>'
        '<th rowspan="2" class="num">searched</th>'
        '<th rowspan="2">evidence</th><th rowspan="2">note</th></tr>'
        '<tr><th class="num">return</th><th class="num">Sharpe</th><th class="num">maxDD</th>'
        '<th class="num">return</th><th class="num">Sharpe</th><th class="num">maxDD</th></tr>'
        '</thead><tbody>' + body + '</tbody></table></div>'
        '<p class="finding"><strong>None of these rows carries the evidence it appears to, and '
        'the reason is not in this table.</strong> Every long/short row was measured on one '
        'rebalance phase. The strategy turns out to be far more sensitive to that arbitrary '
        'choice than to any parameter here — so a rerun of the identical search on a different '
        'weekday would crown a different winner, and the ordering is largely an artifact. '
        'The <em>searched</em> column was the right instinct pointed at the wrong risk: it '
        'counts configurations while the damage came from a degree of freedom nobody counted, '
        'because nobody thought of it as one. Even the null test inherits the flaw — all 24 '
        'draws were run on the same phase as the real data, so it compared a lucky phase '
        'against a lucky phase. See the phase-robustness section.</p></section>'
    )


# --- page -------------------------------------------------------------------


def _universe_table(points: list[dict]) -> str:
    """The universe chart's values, sampled yearly.

    Every point would be 357 rows of noise; one per year carries the shape a
    reader needs — the eligible count is roughly flat while the delisted tail
    grows, which is the whole argument that this is not a survivor sample.
    """
    if not points:
        return ""
    seen, rows = set(), ""
    for p in points:
        year = p["date"][:4]
        if year in seen:
            continue
        seen.add(year)
        rows += (
            f'<tr><td class="mono-s">{_esc(p["date"])}</td>'
            f'<td class="num">{p["eligible"]}</td>'
            f'<td class="num">{p["considered"]}</td>'
            f'<td class="num">{p["dead"]}</td></tr>'
        )
    last = points[-1]
    rows += (
        f'<tr><td class="mono-s">{_esc(last["date"])}</td>'
        f'<td class="num">{last["eligible"]}</td>'
        f'<td class="num">{last["considered"]}</td>'
        f'<td class="num">{last["dead"]}</td></tr>'
    )
    return (
        '<div class="scroll"><table class="data"><thead><tr><th>first date of year</th>'
        '<th class="num">eligible</th><th class="num">considered</th>'
        '<th class="num">delisted carried</th></tr></thead>'
        f'<tbody>{rows}</tbody></table></div>'
    )


def _tiles(record: dict) -> str:
    d = record["data"]
    cur = record.get("current", {})
    strat = next(
        (x for x in cur.get("decomposition", []) if x["name"] == "strategy"), None
    )
    # Decisions across all seven tranches, which is what is actually replayed.
    n = (strat["fresh"]["n"] + strat["orig"]["n"]) * 7 if strat else 0
    cells = [
        (f"{d['assets']}", "assets in the store", ""),
        (f"{d['bars'] // 1000}k", "daily bars", f"{d['first_bar']} → {d['last_bar']}"),
        (f"{d['delisted']}", "delisted series retained", "not a survivor sample"),
        (f"{n}", "decisions replayed", "7 tranches, weekly each"),
    ]
    return "".join(
        f'<div class="tile"><div class="tile-v">{_esc(v)}</div>'
        f'<div class="tile-l">{_esc(l)}</div>'
        f'<div class="tile-n">{_esc(note)}</div></div>'
        for v, l, note in cells
    )


def _verdict(record: dict) -> str:
    rows = []
    for r in record["runs"]:
        if r["label"] == "baseline":
            continue
        m, s = r["metrics"], r["stressed"]
        base = next(x for x in record["runs"] if x["label"] == "baseline")["metrics"]
        oos = (
            sum(f["return"] for f in r["folds"]) / len(r["folds"]) if r["folds"] else 0.0
        )
        base_oos_run = next(x for x in record["runs"] if x["label"] == "baseline")
        base_oos = (
            sum(f["return"] for f in base_oos_run["folds"]) / len(base_oos_run["folds"])
            if base_oos_run["folds"]
            else 0.0
        )
        criteria = [
            ("positive expectancy after costs", m["total_return"] > 0,
             f"{_pct(m['total_return'])} over {m['n']} rebalances"),
            ("survives 2× slippage", s["total_return"] > 0,
             f"{_pct(s['total_return'])} at 2× vs {_pct(m['total_return'])} at 1×"),
            ("walk-forward beats the baseline", oos > base_oos,
             f"out-of-sample {_pct(oos)} vs baseline {_pct(base_oos)}"),
            ("sample adequate, or it says so", m["n"] >= 60, f"n = {m['n']} (floor 60)"),
        ]
        passed = all(ok for _, ok, _ in criteria)
        body = "".join(
            f'<tr><td><span class="pill {"ok" if ok else "no"}">'
            f'{"PASS" if ok else "FAIL"}</span></td>'
            f"<td>{_esc(name)}</td><td class=\"num\">{_esc(detail)}</td></tr>"
            for name, ok, detail in criteria
        )
        rows.append(
            f'<section class="verdict{" passed" if passed else ""}"><h3>{_esc(r["signal"])} '
            f'<span class="muted">+ {_esc(r["constructor"])}</span></h3>'
            f'<p class="stamp {"ok" if passed else "no"}">'
            f'PHASE 1 GATE: {"PASSED" if passed else "NOT PASSED"}</p>'
            f'<table class="crit"><tbody>{body}</tbody></table></section>'
        )
    return "".join(rows)


def _results_table(record: dict) -> str:
    head = (
        "<tr><th>run</th><th class='num'>n</th><th class='num'>return</th>"
        "<th class='num'>CAGR</th><th class='num'>vol</th><th class='num'>Sharpe</th>"
        "<th class='num'>max DD</th><th class='num'>turnover</th>"
        "<th class='num'>cost bps</th><th class='num'>rejected</th></tr>"
    )
    body = ""
    for r in record["runs"]:
        m = r["metrics"]
        body += (
            f"<tr><td>{_esc(r['label'])}</td><td class='num'>{m['n']}</td>"
            f"<td class='num'>{_pct(m['total_return'])}</td>"
            f"<td class='num'>{m['cagr'] * 100:+.2f}%</td>"
            f"<td class='num'>{m['volatility'] * 100:.1f}%</td>"
            f"<td class='num'>{m['sharpe']:+.2f}</td>"
            f"<td class='num'>{m['max_drawdown'] * 100:.2f}%</td>"
            f"<td class='num'>{m['turnover'] * 100:.2f}%</td>"
            f"<td class='num'>{m['cost_bps']:.0f}</td>"
            f"<td class='num'>{m['rejected']}</td></tr>"
        )
    return f"<div class='scroll'><table class='data'><thead>{head}</thead><tbody>{body}</tbody></table></div>"


def _folds_table(record: dict) -> str:
    head = "<tr><th>run</th><th>window</th><th class='num'>n</th><th class='num'>return</th><th class='num'>Sharpe</th></tr>"
    body = ""
    for r in record["runs"]:
        for f in r["folds"]:
            body += (
                f"<tr><td>{_esc(r['label'])}</td>"
                f"<td>{_esc(f['start'])} → {_esc(f['end'])}</td>"
                f"<td class='num'>{f['n']}</td>"
                f"<td class='num'>{_pct(f['return'])}</td>"
                f"<td class='num'>{f['sharpe']:+.2f}</td></tr>"
            )
    return f"<div class='scroll'><table class='data'><thead>{head}</thead><tbody>{body}</tbody></table></div>"


def render(record: dict) -> str:
    """The whole page, as one self-contained document body."""
    spread_rows = [
        (
            f"{r['horizon']}d",
            r["mean_spread"],
            f"t={r['t_stat']:+.2f} · eff n {r['effective_n']:.0f}"
            + ("" if abs(r["t_stat"]) > 2 else " · not sig."),
        )
        for r in record["spread"]
    ]

    spread_table = "".join(
        f"<tr><td>{r['horizon']}d</td><td class='num'>{r['periods']}</td>"
        f"<td class='num'>{r['effective_n']:.0f}</td>"
        f"<td class='num'>{r['mean_spread'] * 100:+.3f}%</td>"
        f"<td class='num'>{r['t_stat']:+.2f}</td>"
        f"<td class='num'>{r['pct_above'] * 100:.1f}%</td></tr>"
        for r in record["spread"]
    )

    window = record["window"]
    return f"""
<div class="viz-root">
<header>
  <p class="eyebrow">ai-trader · research view</p>
  <h1>Phase 1: two candidates, two failures</h1>
  <p class="sub">Replay {window[0]} → {window[1]} · weekly cadence · generated by
  <code>ai-trader report</code></p>
</header>

<div class="disclosures" role="note">
  <p class="dh">Read before any number below</p>
  <ul>
    <li>Every strategy figure is <strong>tranched across all seven rebalance phases</strong>.
      A single-phase number is one draw: on one phase this strategy returned +1177% over the
      second window and on another −57%, with nothing changed but the weekday.</li>
    <li>The cost model is <strong>uncalibrated</strong> — its impact coefficient is assumed
      rather than fitted to realised fills, so every cost figure carries an unquantified error.</li>
    <li>The fill model crosses the spread and charges commission and <strong>models nothing
      else</strong>: no partial fills, no queue position, no depth. Pessimistic but crude.</li>
    <li>The parameters were selected on one phase before that was understood, so the
      <strong>selection is not yet phase-free even though the measurement is</strong>.</li>
    <li>Every chart here has a table below it carrying the same values.</li>
  </ul>
</div>

{current_section(record["current"]) if record.get("current") else ""}
{phase_section(record["phases"]) if record.get("phases") else ""}
{fixed_budget_section(record["fixed_budget"]) if record.get("fixed_budget") else ""}

<section>
  <h2>What the data is</h2>
  <div class="tiles">{_tiles(record)}</div>
  <p class="note">The delisted count is the load-bearing one. A universe built from
  currently-listed assets is selected for having survived, and it flatters momentum
  specifically — the assets it buys are disproportionately the ones that later died.</p>
</section>

<section>
  <h2>Breakout spread — above band minus below band</h2>
  <p class="note">The signal the current strategy is built on, tested directly rather
  than through a portfolio. Does an asset above its channel outperform one below it?</p>
  {signed_bars(spread_rows, title_id="sp-h")}
  <div class="scroll"><table class="data"><thead><tr><th>horizon</th>
  <th class="num">periods</th><th class="num">eff n</th>
  <th class="num">mean spread</th><th class="num">t</th>
  <th class="num">% above band</th></tr></thead>
  <tbody>{spread_table}</tbody></table></div>
  <p class="finding"><strong>Flat, and negative past a week.</strong> +0.14% at 7 days
  (t = +0.27), −0.57% at 14, −2.40% at 30. This is the strategy's own premise measured
  head-on over the full window, and it does not hold — which sits uneasily beside a
  long/short book built on it returning what it does. The most likely reconciliation is
  that the book is not trading the raw group split: it ranks within each leg, caps
  position count and size, and collects funding on the short side. Those are the parts
  this test does not cover, and the parts that have had the least scrutiny.</p>
</section>

<section>
  <h2>Universe over time</h2>
  <p class="note">Eligible assets per decision date, and how many delisted series the
  snapshot still carries. The second line is what makes the first one honest.</p>
  {universe_chart(record["universe"])}
  {_universe_table(record["universe"])}
</section>

<section>
  <h2>What is not settled</h2>
  <ul class="open">
    <li><strong>The parameters were chosen on a single phase.</strong> Tranching fixed how
      the strategy is measured, not how its tilt scale, regime period and channel length
      were picked. Those sweeps need re-running against the tranched objective before the
      numbers above mean what they appear to.</li>
    <li><strong>No null test on the tranched result.</strong> The original label-shuffle
      null shared the Friday phase with the real data, so it compared a lucky phase against
      a lucky phase. It has to be re-run per phase.</li>
    <li><strong>The spread test and the book disagree.</strong> Resolving that means
      isolating the within-leg ranking, the position caps and the funding carry and
      measuring each separately.</li>
    <li><strong>The short-leg ranking is untested.</strong> Ranking shorts by distance below
      the lower band was introduced to fit the twelve-position limit, not because anything
      measured said it was right.</li>
    <li><strong>BTC still wins the first window</strong> on both return and Sharpe. The case
      rests on the second window and on drawdown.</li>
  </ul>
  <p class="note">The full history of everything tried and rejected — including three
  retracted results — is in <code>docs/phase-1-findings.md</code>. This page is the
  current state, not the log.</p>
</section>

<footer>
  <p>Regenerate: <code>ai-trader report --record research.json --out research.html</code>.
  This page computes nothing — it renders a record the CLI produced, and deleting it would
  cost a convenience and no evidence.</p>
</footer>
</div>
"""


CSS = """
/* An instrument readout, not a SaaS dashboard.
   The subject is a CLI-first system, so the terminal is its native surface:
   every number, ticker and label wears the mono face and prose wears the sans.
   Sections are divided by hairline rules rather than boxed into cards - a
   research record reads as a continuous document, and rounded cards with an
   accent rail is the look every generated dashboard already has. */
.viz-root {
  color-scheme: light;
  --ground: #f9f9f7; --surface: #fcfcfb;
  --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --rule: #e1e0d9; --axis: #c3c2b7; --hair: rgba(11,11,11,0.10);
  --s1: #2a78d6; --s2: #eb6834; --s3: #1baf7a; --s4: #eda100;
  --good: #0ca30c; --critical: #d03b3b;
  --sans: system-ui, -apple-system, "Segoe UI", sans-serif;
  --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  background: var(--ground); color: var(--ink); font-family: var(--sans);
  line-height: 1.6; padding: 40px 24px 72px; max-width: 860px; margin: 0 auto;
  -webkit-font-smoothing: antialiased;
}
@media (prefers-color-scheme: dark) {
  :root:where(:not([data-theme="light"])) .viz-root {
    color-scheme: dark;
    --ground: #0d0d0d; --surface: #1a1a19;
    --ink: #ffffff; --ink-2: #c3c2b7; --muted: #898781;
    --rule: #2c2c2a; --axis: #383835; --hair: rgba(255,255,255,0.10);
    --s1: #3987e5; --s2: #d95926; --s3: #199e70; --s4: #c98500;
  }
}
:root[data-theme="light"] .viz-root {
  color-scheme: light;
  --ground: #f9f9f7; --surface: #fcfcfb;
  --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --rule: #e1e0d9; --axis: #c3c2b7; --hair: rgba(11,11,11,0.10);
  --s1: #2a78d6; --s2: #eb6834; --s3: #1baf7a; --s4: #eda100;
}
:root[data-theme="dark"] .viz-root {
  color-scheme: dark;
  --ground: #0d0d0d; --surface: #1a1a19;
  --ink: #ffffff; --ink-2: #c3c2b7; --muted: #898781;
  --rule: #2c2c2a; --axis: #383835; --hair: rgba(255,255,255,0.10);
  --s1: #3987e5; --s2: #d95926; --s3: #199e70; --s4: #c98500;
}
.viz-root * { box-sizing: border-box; }
.viz-root :focus-visible { outline: 2px solid var(--s1); outline-offset: 2px; }

/* type scale: 11 / 12.5 / 13.5 / 15 / 20 / 32 */
.eyebrow { font-family: var(--mono); color: var(--muted); font-size: 11px;
  letter-spacing: .14em; text-transform: uppercase; margin: 0 0 10px; }
h1 { font-size: 32px; line-height: 1.15; margin: 0 0 8px; font-weight: 600;
  letter-spacing: -.018em; text-wrap: balance; }
h2 { font-size: 20px; margin: 0 0 6px; font-weight: 600; letter-spacing: -.012em; }
h3 { font-family: var(--mono); font-size: 13.5px; margin: 0; font-weight: 600;
  letter-spacing: -.01em; }
.sub { font-family: var(--mono); color: var(--ink-2); margin: 0; font-size: 12.5px; }
section { margin-top: 44px; padding-top: 22px; border-top: 1px solid var(--rule); }
.note { color: var(--ink-2); font-size: 14px; margin: 0 0 18px; max-width: 68ch; }
.finding { font-size: 14px; margin: 18px 0 0; padding: 14px 0 0;
  border-top: 1px solid var(--rule); max-width: 68ch; }
.muted { color: var(--muted); font-weight: 400; }
code { font-family: var(--mono); font-size: .9em; color: var(--ink-2); }

/* Disclosures: a warning label on an instrument. First in the document,
   never collapsible - a caveat behind a toggle is a caveat nobody read. */
.disclosures { margin-top: 30px; padding: 18px 0 18px 18px;
  border-left: 2px solid var(--critical); }
.disclosures .dh { font-family: var(--mono); margin: 0 0 10px; font-weight: 600;
  font-size: 11px; letter-spacing: .14em; text-transform: uppercase;
  color: var(--critical); }
.disclosures ul { margin: 0; padding-left: 16px; display: grid; gap: 6px; }
.disclosures li { font-size: 13.5px; color: var(--ink-2); max-width: 68ch; }

.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 0; margin-bottom: 16px; border-top: 1px solid var(--rule); }
.tile { padding: 16px 18px 16px 0; border-bottom: 1px solid var(--rule); }
.tile-v { font-size: 32px; font-weight: 550; line-height: 1; letter-spacing: -.02em; }
.tile-l { font-family: var(--mono); font-size: 11.5px; color: var(--ink-2);
  margin-top: 7px; letter-spacing: .02em; }
.tile-n { font-size: 12px; color: var(--muted); margin-top: 3px; }

/* The verdict wears a severity stripe so state reads as form, not only colour. */
.verdict { padding: 16px 0 16px 18px; border-left: 3px solid var(--critical);
  margin-bottom: 22px; }
.verdict.passed { border-left-color: var(--good); }
.stamp { font-family: var(--mono); font-size: 12px; font-weight: 600;
  letter-spacing: .12em; margin: 8px 0 12px; color: var(--critical); }
.stamp.ok { color: var(--good); }
.pill { font-family: var(--mono); display: inline-block; min-width: 44px;
  text-align: center; font-size: 10.5px; font-weight: 600; letter-spacing: .08em;
  padding: 2px 6px; border: 1px solid currentColor; }
.pill.ok { color: var(--good); }
.pill.no { color: var(--critical); }

.scroll { overflow-x: auto; }
table { width: 100%; border-collapse: collapse; font-size: 12.5px;
  font-family: var(--mono); }
table.crit { margin-top: 2px; }
table.crit td { padding: 5px 10px 5px 0; border: 0; vertical-align: top; }
table.crit td:first-child { width: 58px; }
table.crit td:nth-child(2) { font-family: var(--sans); font-size: 13.5px; }
.data { margin-top: 0; }
.data th { text-align: left; font-weight: 600; color: var(--muted); padding: 8px 12px 8px 0;
  border-bottom: 1px solid var(--axis); font-size: 10.5px; letter-spacing: .08em;
  text-transform: uppercase; white-space: nowrap; }
.data td { padding: 7px 12px 7px 0; border-bottom: 1px solid var(--rule);
  white-space: nowrap; }
.data tbody tr:last-child td { border-bottom: 0; }
.num { text-align: right; font-variant-numeric: tabular-nums; }
.cands th { text-align: center; }
.cands th[rowspan] { text-align: left; vertical-align: bottom; }
.cands th.num { text-align: right; }
.cands td:first-child { white-space: nowrap; font-weight: 500; }
.mono-s { color: var(--muted); font-size: 11px; }
.note-cell { white-space: normal; min-width: 15rem; color: var(--ink-2);
  font-family: var(--sans); font-size: 12.5px; }
td.muted { color: var(--muted); }
.ev { font-size: 10.5px; padding: 2px 6px; border: 1px solid currentColor;
  white-space: nowrap; }
.ev-ok { color: var(--good); }
.ev-thin { color: var(--ink-2); }
.ev-no { color: var(--critical); }

.chart-wrap { position: relative; }
.chart { width: 100%; height: auto; display: block; }
.grid { stroke: var(--rule); stroke-width: 1; }
.axis, .zero { stroke: var(--axis); stroke-width: 1; }
.tick { fill: var(--muted); font-size: 10px; font-family: var(--mono);
  font-variant-numeric: tabular-nums; }
.line { stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }
.line.s1, .dot.s1 { stroke: var(--s1); }
.line.s2, .dot.s2 { stroke: var(--s2); }
.line.s3, .dot.s3 { stroke: var(--s3); }
.line.s4, .dot.s4 { stroke: var(--s4); }
.dot { fill: var(--surface); stroke-width: 2; }
.endlabel { font-size: 11px; font-family: var(--mono); font-weight: 600; }
.endlabel.s1 { fill: var(--s1); } .endlabel.s2 { fill: var(--s2); }
.endlabel.s3 { fill: var(--s3); }
.endlabel.s4 { fill: var(--s4); }
.fold { fill: var(--rule); opacity: .5; }
.fold-label { fill: var(--muted); font-size: 9px; font-family: var(--mono);
  letter-spacing: .1em; }
.split { stroke: var(--muted); stroke-width: 1; }
.bar { fill: var(--s1); }
.bar.neg { fill: var(--s2); }
.barval { fill: var(--ink); font-size: 11px; font-family: var(--mono);
  font-variant-numeric: tabular-nums; font-weight: 600; }
.barnote { fill: var(--muted); font-size: 10px; font-family: var(--mono); }

/* Crosshair + tooltip. Enhances, never gates: every value is in the table. */
.xhair { stroke: var(--axis); stroke-width: 1; pointer-events: none; opacity: 0; }
.tip { position: absolute; pointer-events: none; opacity: 0; transform: translate(-50%, -100%);
  background: var(--surface); border: 1px solid var(--axis); padding: 7px 10px;
  font-family: var(--mono); font-size: 11px; line-height: 1.5; white-space: nowrap;
  font-variant-numeric: tabular-nums; z-index: 2; }
.tip.on, .xhair.on { opacity: 1; }
.tip b { font-weight: 600; }
.tip i { font-style: normal; display: inline-block; width: 7px; height: 7px;
  margin-right: 6px; vertical-align: middle; }
@media (prefers-reduced-motion: no-preference) {
  .tip, .xhair { transition: opacity .12s ease; }
}
footer { margin-top: 50px; padding-top: 18px; border-top: 1px solid var(--rule);
  color: var(--muted); font-size: 12.5px; max-width: 68ch; }
@media (max-width: 620px) {
  h1 { font-size: 25px; } .viz-root { padding: 26px 16px 52px; }
  .tile-v { font-size: 26px; }
}
"""


def build(record: dict) -> str:
    return f"<style>{CSS}</style>\n{render(record)}"


def write(record_path: Path, out_path: Path) -> Path:
    record = json.loads(record_path.read_text(encoding="utf-8"))
    out_path.write_bytes(build(record).encode("utf-8"))
    return out_path
