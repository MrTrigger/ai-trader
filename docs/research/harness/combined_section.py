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

    for slot, (label, pts) in enumerate(series):
        d = " ".join(f"{px(a):.1f},{py(b):.1f}" for a, b in pts)
        out.append(f'<polyline points="{d}" class="line s{slot + 1}" fill="none"/>')
        a, b = pts[-1]
        out.append(
            f'<circle cx="{px(a):.1f}" cy="{py(b):.1f}" r="3.5" class="dot s{slot + 1}"/>'
        )
        out.append(
            f'<text x="{px(a) + 9:.1f}" y="{py(b) + 4:.1f}" '
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
        '<p class="finding"><strong>Absolute return says BTC; risk-adjusted says the neutral '
        'book.</strong> BTC ends 279 points higher and spent the 2022 bear more than 74% '
        'below its peak. The neutral book never lost more than 22.5%, and beat BTC outright '
        'in the window where BTC went nowhere. Which of those is the better outcome is a '
        'question about what the account is for, not one the data answers.</p></section>'
    )
