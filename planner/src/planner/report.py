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
    # Framed so the plot sits on the raised surface rather than the page ground.
    return '<div class="chart-frame">' + "".join(out) + "</div>"


def _params_table(strategy: dict) -> str:
    """Exactly what was run. A backtest number without its parameters is a rumour."""
    rows = "".join(
        f'<tr><td>{_esc(k)}</td><td class="mono-s">{_esc(v)}</td></tr>'
        for k, v in strategy["params"]
    )
    return (
        '<div class="scroll"><table class="data params"><thead>'
        '<tr><th>parameter</th><th>value</th></tr></thead>'
        f'<tbody>{rows}</tbody></table></div>'
    )


def _metric_tiles(m: dict) -> str:
    cells = [
        (f"{m['total_return'] * 100:,.0f}%", "total return", f"{m['years']:.1f} years, compounded"),
        (f"{m['cagr'] * 100:.1f}%", "CAGR", f"vs BTC {m['btc_return'] * 100:,.0f}% total"),
        (f"{m['sharpe']:.2f}", "Sharpe", f"Sortino {m['sortino']:.2f}"),
        (f"{m['max_drawdown'] * 100:.1f}%", "max drawdown", f"vs BTC {m['btc_maxdd'] * 100:.0f}%"),
        (f"{m['calmar']:.2f}", "Calmar", "CAGR / max drawdown"),
        (f"{m['volatility'] * 100:.1f}%", "volatility", "annualised, weekly returns"),
        (f"{m['win_rate'] * 100:.0f}%", "weeks positive", f"{m['weeks']} weeks"),
        (f"{m['t_stat']:.2f}", "t-statistic", "mean weekly return / its error"),
    ]
    # Semantic colour on the two tiles where sign carries meaning, and nowhere
    # else. Colouring every number would make none of them stand out.
    tone = {"total return": "good" if m["total_return"] > 0 else "bad",
            "max drawdown": "bad"}
    return "".join(
        f'<div class="tile {tone.get(lab, "")}"><div class="tile-v">{_esc(v)}</div>'
        f'<div class="tile-l">{_esc(lab)}</div>'
        f'<div class="tile-n">{_esc(note)}</div></div>'
        for v, lab, note in cells
    )


def _equity_section(record: dict) -> str:
    s = record["series"]
    equity = _line_panel(
        [("strategy", s["compounded"]), ("BTC buy & hold", s["btc"])],
        height=300, split=None, fmt=lambda v: f"{v:.0f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="Growth of one unit, compounded, against buy and hold BTC", baseline=1.0,
    )
    draw = _line_panel(
        [("strategy", s["drawdown"]), ("BTC buy & hold", s["btc_drawdown"])],
        height=190, split=None, fmt=lambda v: f"{v * 100:.0f}%",
        ticks_from=lambda a, b: (a * 1.08, 0.0),
        aria="Drawdown from peak", baseline=0.0,
    )
    rows = "".join(
        f'<tr><td class="mono-s">{_esc(y["year"])}</td>'
        f'<td class="num">{y["ret"] * 100:+.1f}%</td></tr>'
        for y in record["yearly"]
    )
    return (
        '<section><h2>Equity, compounded</h2>'
        '<p class="note">Position sizes are fractions of NAV, so every dollar earned is '
        'redeployed. This is what the account does.</p>'
        + equity
        + '<p class="note" style="margin-top:14px">Drawdown from peak, same axis.</p>'
        + draw
        + '<div class="scroll"><table class="data"><thead><tr><th>year</th>'
        '<th class="num">return</th></tr></thead><tbody>' + rows
        + '</tbody></table></div></section>'
    )


def _fixed_section(record: dict) -> str:
    m = record["metrics"]
    chart = _line_panel(
        [("fixed budget", record["series"]["fixed"])],
        height=250, split=None, fmt=lambda v: f"{v:.1f}x",
        ticks_from=lambda a, b: (0.0, b * 1.05),
        aria="The same weekly returns applied to a constant stake", baseline=1.0,
    )
    drag = m["total_return"] - m["fixed_budget_return"]
    return (
        '<section><h2>Equity, fixed budget</h2>'
        '<p class="note">The identical weekly returns applied to a <strong>constant '
        'stake</strong> rather than a growing one. Slope is performance here: a straight line '
        'is a steady edge and a flattening one is a fading edge rather than a smaller '
        'account.</p>'
        + chart
        + '<div class="scroll"><table class="data"><thead><tr><th>convention</th>'
        '<th class="num">result</th></tr></thead><tbody>'
        f'<tr><td>compounded &mdash; <code>equity ×= 1+r</code></td>'
        f'<td class="num">{m["total_return"] * 100:,.1f}%</td></tr>'
        f'<tr><td>fixed budget &mdash; <code>equity += r</code></td>'
        f'<td class="num">{m["fixed_budget_return"] * 100:,.1f}%</td></tr>'
        f'<tr><td>difference</td><td class="num">{drag * 100:,.1f}%</td></tr>'
        '</tbody></table></div></section>'
    )


def _exposure_section(record: dict) -> str:
    """Gross, net, and therefore how much margin the book needs."""
    e = record["exposure"]
    chart = _line_panel(
        [("gross", e["gross"]), ("net", e["net"])],
        height=250, split=None, fmt=lambda v: f"{v:+.1f}x",
        ticks_from=lambda a, b: (min(a * 1.1, -0.1), max(b * 1.1, 0.1)),
        aria="Gross and net exposure as a fraction of NAV", baseline=0.0,
    )
    rows = [
        ("gross exposure, mean", f"{e['mean_gross']:.2f}x NAV"),
        ("gross exposure, peak", f"{e['max_gross']:.2f}x NAV"),
        ("net exposure, most long", f"{e['max_net_long']:+.2f}x NAV"),
        ("net exposure, most short", f"{e['max_net_short']:+.2f}x NAV"),
        ("net exposure, mean absolute", f"{e['mean_abs_net']:.2f}x NAV"),
        ("largest single position", f"{e['max_name']:.2f}x NAV"),
    ]
    body = "".join(
        f'<tr><td>{_esc(k)}</td><td class="num">{_esc(v)}</td></tr>' for k, v in rows
    )
    return (
        '<section><h2>Exposure and margin</h2>'
        '<p class="note">Gross is the sum of absolute weights &mdash; the capital actually '
        'deployed. Net is their signed sum, which is the directional bet. <strong>Gross never '
        'exceeds the configured target, so this book is never levered above 1×</strong>; the '
        'short leg is the only part requiring margin, and it is funded by perpetuals rather '
        'than borrowed spot.</p>'
        + chart
        + '<div class="scroll"><table class="data"><thead><tr><th>measure</th>'
        '<th class="num">value</th></tr></thead><tbody>' + body + '</tbody></table></div>'
        f'<p class="finding">Net exposure reaches the gross figure in both directions, which '
        'means that at maximum tilt one leg is empty and the book is entirely directional. '
        'That is the intended behaviour of the regime tilt and it is bounded &mdash; gross is '
        'unchanged, so no leverage is introduced &mdash; but a book described as '
        '&ldquo;market-neutral&rdquo; is not neutral in those weeks.</p></section>'
    )


def _stats_section(record: dict) -> str:
    st, m = record["stats"], record["metrics"]
    rows = [
        ("long names held, mean", f"{st['mean_long_names']:.1f}"),
        ("short names held, mean", f"{st['mean_short_names']:.1f}"),
        ("turnover per rebalance", f"{st['mean_turnover'] * 100:.0f}% of NAV"),
        ("weeks flat (all tranches)", f"{st['flat_weeks']} of {m['weeks']}"),
        ("weeks partly flat", f"{st['partial_weeks']} of {m['weeks']}"),
        ("regime read up", f"{st['pct_up_regime'] * 100:.0f}% of weeks"),
        ("regime read down", f"{st['pct_down_regime'] * 100:.0f}% of weeks"),
        ("best week", f"{m['best_week'] * 100:+.1f}%"),
        ("worst week", f"{m['worst_week'] * 100:+.1f}%"),
    ]
    attrib = [
        ("long leg", st["from_long"]),
        ("short leg", st["from_short"]),
        ("funding received", st["total_funding"]),
        ("trading costs", -st["total_cost"]),
    ]
    body = "".join(f'<tr><td>{_esc(k)}</td><td class="num">{_esc(v)}</td></tr>' for k, v in rows)
    att = "".join(
        f'<tr><td>{_esc(k)}</td><td class="num">{v * 100:+.1f}%</td></tr>' for k, v in attrib
    )
    return (
        '<section><h2>How the book behaved</h2>'
        '<div class="scroll"><table class="data"><thead><tr><th>statistic</th>'
        '<th class="num">value</th></tr></thead><tbody>' + body + '</tbody></table></div>'
        '<h3>Where the return came from</h3>'
        '<p class="note">Sum of each component across every week, before compounding, so '
        'they add to the fixed-budget result rather than the compounded one.</p>'
        '<div class="scroll"><table class="data"><thead><tr><th>component</th>'
        '<th class="num">contribution</th></tr></thead><tbody>' + att + '</tbody></table></div>'
        f'<p class="finding">The long leg contributes roughly three times the short leg, and '
        f'<strong>funding received almost exactly cancels trading costs</strong> '
        f'({st["total_funding"] * 100:+.1f}% against {-st["total_cost"] * 100:.1f}%). The '
        f'short leg therefore pays for the book\'s turnover and contributes '
        f'{st["from_short"] * 100:.0f}% on top; without shorts this would be a long-only '
        'book paying its own costs out of returns.</p></section>'
    )


def render(record: dict) -> str:
    """One backtest run: what was run, what it produced, and how it behaved."""
    s, m, w = record["strategy"], record["metrics"], record["window"]
    return f"""
<div class="viz-root">
<header>
  <p class="eyebrow">ai-trader · backtest</p>
  <h1>{_esc(s["name"])}</h1>
  <p class="sub">{_esc(w[0])} → {_esc(w[1])} · {m["weeks"]} weekly rebalances ·
  generated by <code>ai-trader report</code></p>
</header>

<div class="disclosures" role="note">
  <p class="dh">Read before any number below</p>
  <ul>
    <li>The cost model is <strong>uncalibrated</strong> — its impact coefficient is assumed
      rather than fitted to realised fills, so every cost figure carries an unquantified error.</li>
    <li>The fill model crosses the spread and charges commission and <strong>models nothing
      else</strong>: no partial fills, no queue position, no depth.</li>
    <li>Parameters were selected on this same data. <strong>No result below is
      out-of-sample</strong>; it describes this history rather than predicting another.</li>
    <li>Every chart has a table below it carrying the same values.</li>
  </ul>
</div>

<section>
  <h2>What was run</h2>
  {_params_table(s)}
</section>

<section>
  <h2>Headline</h2>
  <div class="tiles">{_metric_tiles(m)}</div>
</section>

{_equity_section(record)}
{_fixed_section(record)}
{_exposure_section(record)}
{_stats_section(record)}

<footer>
  <p>Regenerate: <code>ai-trader report --record backtest.json --out research.html</code>.
  This page computes nothing — it renders a record the CLI produced.</p>
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
/* The document itself, not just our container. Without these the page sits in
   a white gutter on a dark ground, because the body keeps the UA canvas colour
   while .viz-root paints only its own 860px column. */
html { background: var(--page-bg, #0d0d0d); }
body { margin: 0; background: transparent; min-height: 100vh; }
@media (prefers-color-scheme: light) {
  html:where(:not([data-theme="dark"])) { --page-bg: #f9f9f7; }
}
html[data-theme="light"] { --page-bg: #f9f9f7; }
html[data-theme="dark"] { --page-bg: #0d0d0d; }

.viz-root * { box-sizing: border-box; }

/* Quirks mode used to strip colour from unstyled cells; a doctype fixes that,
   but stating it here means a future shell change cannot silently undo it. */
.data td { color: var(--ink); }
.data td.num { font-variant-numeric: tabular-nums; }

/* Give charts a defined box so a viewBox can never collapse to the
   replaced-element default height. */
.chart { min-height: 120px; }

/* --- polish ---------------------------------------------------------------
   The page is a readout, so the work goes into hierarchy and rhythm rather
   than ornament: one accent, hairline rules instead of cards, and numerals
   that line up in columns. */

/* Charts sit on the raised surface so the plot area reads as an instrument
   panel rather than floating on the page ground. */
.chart-frame { background: var(--surface); border: 1px solid var(--rule);
  padding: 14px 10px 6px; margin: 0 0 4px; }

/* Tiles: a quiet grid, with the number doing all the talking. */
.tiles { display: grid; gap: 1px; background: var(--rule);
  border: 1px solid var(--rule); grid-template-columns: repeat(auto-fit, minmax(148px, 1fr)); }
.tile { background: var(--surface); padding: 16px 16px 14px; }
.tile-v { font-variant-numeric: tabular-nums; }
.tile.good .tile-v { color: var(--good); }
.tile.bad .tile-v { color: var(--critical); }

/* Tables: zebra-free, but a hover row so the eye can track across a wide one. */
.data tbody tr:hover td { background: var(--hair); }
.data th.num, .data td.num { text-align: right; }
.params td:first-child { font-family: var(--sans); font-size: 13px;
  color: var(--ink-2); white-space: normal; }
.params td:last-child { color: var(--ink); }

/* Section rhythm. A rule above each heading rather than a box around it. */
section { margin-top: 46px; padding-top: 26px; border-top: 1px solid var(--rule); }
section:first-of-type { border-top: 0; }
h2 { font-size: 19px; font-weight: 600; letter-spacing: -.01em; margin: 0 0 4px;
  text-wrap: balance; }
h3 { font-size: 14px; font-weight: 600; margin: 30px 0 6px;
  font-family: var(--mono); letter-spacing: .01em; }

/* The disclosure block is the one place colour is allowed to raise its voice. */
.disclosures { border-left: 3px solid var(--critical); padding: 2px 0 2px 18px;
  margin: 26px 0 6px; }

@media (prefers-reduced-motion: reduce) {
  * { transition: none !important; animation: none !important; }
}
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
    """A complete HTML document, not a fragment.

    This used to emit a bare body fragment on the assumption that a publisher
    would supply the shell. Nothing publishes it — it is served from loopback and
    opened from file:// — so the browser was left to infer a document, which puts
    it in **quirks mode**, and quirks mode broke the page in two ways that looked
    unrelated:

    - Tables do not inherit colour from their ancestors, so any `<td>` without an
      explicit colour reset to black. On a dark ground the parameter names were
      invisible while the cells that happened to carry a colour class were fine.
    - `height: auto` on an SVG with a viewBox collapses to the replaced-element
      default, so every chart rendered as a strip of axis labels with no plot.

    A doctype fixes both, and there is no cost: an explicit `<head>` also carries
    the viewport and colour-scheme hints the page wants anyway.
    """
    title = _esc(record.get("strategy", {}).get("name", "backtest"))
    return (
        "<!doctype html>\n"
        '<html lang="en">\n<head>\n'
        '<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        '<meta name="color-scheme" content="dark light">\n'
        f"<title>{title} · ai-trader</title>\n"
        f"<style>{CSS}</style>\n"
        "</head>\n<body>\n"
        f"{render(record)}\n"
        "</body>\n</html>\n"
    )


def write(record_path: Path, out_path: Path) -> Path:
    record = json.loads(record_path.read_text(encoding="utf-8"))
    out_path.write_bytes(build(record).encode("utf-8"))
    return out_path
