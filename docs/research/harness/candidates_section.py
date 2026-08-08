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
        trust = (
            "no" if c["configs"] >= 88
            else ("thin" if c["configs"] >= 40 else "ok")
        )
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
        '<p class="finding"><strong>The row to trust is <em>market-neutral</em>, not the one '
        'at the bottom.</strong> It is the only line here tested once, on a window that had '
        'never informed it. Everything below it was reached by searching the same two windows, '
        'and by the last row both of them have set parameters — so neither is out-of-sample '
        'any more and the number is a description of this data rather than a prediction about '
        'any other. The bottom row also doubles the drawdown to get there.</p></section>'
    )
