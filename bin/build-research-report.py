#!/usr/bin/env python3
"""The research report: equity and drawdown, bot against BTC and the S&P 500.

Reads the clean walk-forward folds plus reference series and writes one
self-contained HTML page (inline SVG, no external assets - the page must render
on loopback with no network, per the operating rule). Regenerate after every
walk-forward that changes the headline:

    python3 bin/build-research-report.py \
        var/research/wf-honest-perrisk <spx.json> <btc.json> docs/research/report.html
"""
import json, math, statistics, sys, glob, datetime

WF, SPX_JSON, BTC_JSON, OUT = sys.argv[1:5]

# ---- bot: chain per-fold NAV paths into one daily return series -------------
folds = [json.load(open(f)) for f in sorted(glob.glob(f"{WF}/fold-*.json"),
         key=lambda p: int(p.split("-")[-1].split(".")[0]))]
bot = []  # (date, ret)
for f in folds:
    ss = sorted(f["steps"], key=lambda s: s["as_of"])
    prev = None
    for s in ss:
        nav = float(s["nav"])
        if prev:
            bot.append((s["as_of"][:10], nav / prev - 1))
        prev = nav
dates = [d for d, _ in bot]

def yahoo(path):
    r = json.load(open(path))["chart"]["result"][0]
    out = {}
    for t, c in zip(r["timestamp"], r["indicators"]["quote"][0]["close"]):
        if c is not None:
            out[datetime.datetime.fromtimestamp(t, datetime.UTC).date().isoformat()] = c
    return out

spx_px, btc_px = yahoo(SPX_JSON), yahoo(BTC_JSON)

def curve_from_prices(px):
    # forward-fill onto the bot's calendar (SPX has no weekends)
    eq, last, out = 1.0, None, []
    for d in dates:
        p = px.get(d, last)
        if p is None:
            out.append(1.0); continue
        if last: eq *= p / last
        last = p
        out.append(eq)
    return out

def curve_from_rets(rs):
    eq, out = 1.0, []
    for _, r in rs:
        eq *= 1 + r
        out.append(eq)
    return out

series = {
    "TriggerTrader": curve_from_rets(bot),
    "BTC buy & hold": curve_from_prices(btc_px),
    "S&P 500": curve_from_prices(spx_px),
}
dd = {k: [v / max(vs[: i + 1]) - 1 for i, v in enumerate(vs)] for k, vs in series.items()}

def stats(vs):
    rs = [vs[i] / vs[i - 1] - 1 for i in range(1, len(vs)) if vs[i - 1]]
    m, sd = statistics.mean(rs), statistics.stdev(rs)
    yrs = len(rs) / 365.25
    return {"total": vs[-1] - 1, "cagr": vs[-1] ** (1 / yrs) - 1,
            "sharpe": m * 365 / (sd * math.sqrt(365)), "maxdd": min(dd_v := [v / max(vs[:i+1]) - 1 for i, v in enumerate(vs)])}

st = {k: stats(v) for k, v in series.items()}

# ---- svg ---------------------------------------------------------------------
COLORS = {"TriggerTrader": "#3987e5", "BTC buy & hold": "#d95926", "S&P 500": "#199e70"}
W, H, PL, PR, PT, PB = 1120, 330, 56, 176, 14, 26
N = len(dates)

def sx(i): return PL + i * (W - PL - PR) / max(1, N - 1)

def chart(data, fmt, lo=None, hi=None):
    vals = [v for vs in data.values() for v in vs]
    lo = min(vals) if lo is None else lo
    hi = max(vals) if hi is None else hi
    pad = (hi - lo) * 0.05
    lo, hi = lo - pad, hi + pad
    def sy(v): return PT + (hi - v) * (H - PT - PB) / (hi - lo)
    # recessive grid: ~5 ticks
    step = (hi - lo) / 4
    mag = 10 ** math.floor(math.log10(step)); step = math.ceil(step / mag) * mag
    t0 = math.ceil(lo / step) * step
    g, ticks = [], []
    v = t0
    while v <= hi:
        g.append(f'<line x1="{PL}" y1="{sy(v):.1f}" x2="{W-PR}" y2="{sy(v):.1f}" stroke="#232a33" stroke-width="1"/>')
        ticks.append(f'<text x="{PL-8}" y="{sy(v)+4:.1f}" text-anchor="end" class="tick">{fmt(v)}</text>')
        v += step
    paths, labels = [], []
    ends = sorted(((vs[-1], k) for k, vs in data.items()), reverse=True)
    used = []
    for endv, k in ends:
        vs = data[k]
        pts = " ".join(f"{sx(i):.1f},{sy(v):.1f}" for i, v in enumerate(vs))
        paths.append(f'<polyline points="{pts}" fill="none" stroke="{COLORS[k]}" stroke-width="2" stroke-linejoin="round"/>')
        y = sy(endv)
        for u in used:
            if abs(y - u) < 15: y = u + 15
        used.append(y)
        labels.append(f'<circle cx="{W-PR:.0f}" cy="{sy(endv):.1f}" r="3.5" fill="{COLORS[k]}"/>'
                      f'<text x="{W-PR+9}" y="{y+4:.1f}" class="dl" fill="#e6e9ed">{k} <tspan fill="#8b94a1">{fmt(endv)}</tspan></text>')
    # x ticks: years
    xt = []
    seen = set()
    for i, d in enumerate(dates):
        y = d[:4]
        if y not in seen:
            seen.add(y)
            xt.append(f'<text x="{sx(i):.1f}" y="{H-6}" class="tick">{y}</text>')
    return (f'<svg viewBox="0 0 {W} {H}" data-n="{N}">' + "".join(g) + "".join(paths)
            + "".join(labels) + "".join(ticks) + "".join(xt)
            + f'<line class="xh" x1="0" y1="{PT}" x2="0" y2="{H-PB}" stroke="#8b94a1" stroke-width="1" opacity="0"/></svg>')

pct = lambda v: f"{(v-1)*100:+.0f}%" if v >= 0 else f"{v*100:.0f}%"
eq_svg = chart(series, lambda v: f"{(v-1)*100:+.0f}%")
dd_svg = chart(dd, lambda v: f"{v*100:.0f}%", hi=0.0)

fold_rows = "".join(
    f"<tr><td>{i+1}</td><td class='m'>{sorted(f['steps'],key=lambda s:s['as_of'])[0]['as_of'][:10]} → "
    f"{sorted(f['steps'],key=lambda s:s['as_of'])[-1]['as_of'][:10]}</td>"
    f"<td class='n'>{float(f['metrics']['total_return'])*100:+.1f}%</td>"
    f"<td class='n'>{float(f['metrics']['sharpe']):.2f}</td>"
    f"<td class='n'>{float(f['metrics']['max_drawdown'])*100:.1f}%</td></tr>"
    for i, f in enumerate(folds))

stat_cells = "".join(
    f"<div class='card'><div class='who'><span class='sw' style='background:{COLORS[k]}'></span>{k}</div>"
    f"<div class='big'>{v['total']*100:+.1f}%</div>"
    f"<div class='sub'>CAGR {v['cagr']*100:+.1f}% · Sharpe {v['sharpe']:.2f} · maxDD {v['maxdd']*100:.1f}%</div></div>"
    for k, v in st.items())

html = f"""<!doctype html><html><head><meta charset="utf-8">
<title>TriggerTrader research — clean walk-forward</title>
<style>
  body {{ background:#11151b; color:#e6e9ed; font:14px/1.5 "JetBrains Mono","SF Mono",monospace; margin:0; padding:28px 36px; }}
  h1 {{ font-size:17px; margin:0 0 2px; }} h2 {{ font-size:12px; color:#8b94a1; letter-spacing:.14em; margin:30px 0 8px; }}
  .meta {{ color:#8b94a1; font-size:12px; }}
  .cards {{ display:flex; gap:14px; margin:18px 0 4px; flex-wrap:wrap; }}
  .card {{ background:#161b23; border:1px solid #232a33; border-radius:6px; padding:12px 16px; min-width:230px; }}
  .who {{ font-size:12px; color:#8b94a1; display:flex; align-items:center; gap:7px; }}
  .sw {{ width:10px; height:10px; border-radius:2px; display:inline-block; }}
  .big {{ font-size:26px; margin:4px 0 2px; font-variant-numeric:tabular-nums; }}
  .sub {{ font-size:11.5px; color:#8b94a1; }}
  svg {{ width:100%; height:auto; display:block; }}
  .tick {{ font:11px monospace; fill:#5c6570; }} .dl {{ font:12px monospace; }}
  .wrap {{ position:relative; }}
  .tip {{ position:absolute; pointer-events:none; background:#1c232d; border:1px solid #2c3540;
         border-radius:4px; padding:7px 10px; font-size:11.5px; display:none; white-space:nowrap; z-index:2; }}
  table {{ border-collapse:collapse; margin-top:6px; font-size:12.5px; }}
  td,th {{ padding:5px 16px 5px 0; text-align:left; border-bottom:1px solid #1d242d; }}
  th {{ color:#8b94a1; font-weight:400; font-size:11px; letter-spacing:.1em; }}
  .n {{ font-variant-numeric:tabular-nums; text-align:right; }} .m {{ color:#8b94a1; }}
  .note {{ color:#8b94a1; font-size:12px; max-width:75ch; }}
</style></head><body>
<h1>TriggerTrader — clean walk-forward vs benchmarks</h1>
<div class="meta">{dates[0]} → {dates[-1]} · {len(folds)} expanding folds, model retrained per fold, trailing funding, zero leaked dates ·
NET of 4.5bp taker commission + 0.5bp half-spread per fill, 1h execution lag, 10bp round-trip entry floor · impact & funding carry not modelled ·
generated {datetime.date.today().isoformat()} by bin/build-research-report.py</div>
<div class="cards">{stat_cells}</div>
<h2>GROWTH OF $1 (P&amp;L)</h2><div class="wrap" id="c1">{eq_svg}<div class="tip"></div></div>
<h2>DRAWDOWN</h2><div class="wrap" id="c2">{dd_svg}<div class="tip"></div></div>
<h2>FOLDS</h2>
<table><tr><th>#</th><th>TEST WINDOW</th><th>RETURN</th><th>SHARPE</th><th>MAX DD</th></tr>{fold_rows}</table>
<p class="note">The bot series is stitched from six independently trained walk-forward folds: every date is priced by a
model that never saw it. Benchmarks are spot closes (Yahoo), forward-filled onto the bot's calendar for the S&amp;P's
weekends. The previously recorded +875% is retired — its funding features read the future; see
docs/research/harness/README.md.</p>
<script>
const DATES={json.dumps(dates[::7])}; const STEP=7;
const SERIES={json.dumps({k: [round(v,4) for v in vs[::7]] for k, vs in series.items()})};
const DDS={json.dumps({k: [round(v,4) for v in vs[::7]] for k, vs in dd.items()})};
for (const [id, data] of [["c1", SERIES], ["c2", DDS]]) {{
  const wrap=document.getElementById(id), svg=wrap.querySelector("svg"), tip=wrap.querySelector(".tip"),
        xh=wrap.querySelector(".xh"), n=DATES.length;
  svg.addEventListener("mousemove", e => {{
    const r=svg.getBoundingClientRect(), fx=(e.clientX-r.left)/r.width*{W};
    const i=Math.max(0, Math.min(n-1, Math.round((fx-{PL})/({W}-{PL}-{PR})*(n-1))));
    const x={PL}+i*({W}-{PL}-{PR})/(n-1);
    xh.setAttribute("x1",x); xh.setAttribute("x2",x); xh.setAttribute("opacity",.5);
    tip.style.display="block";
    tip.style.left=Math.min(e.clientX-r.left+14, r.width-230)+"px"; tip.style.top=(e.clientY-r.top-10)+"px";
    tip.innerHTML="<b>"+DATES[i]+"</b><br>"+Object.entries(data).map(([k,vs])=>
      k+": "+(id==="c1"?((vs[i]-1)*100).toFixed(1):(vs[i]*100).toFixed(1))+"%").join("<br>");
  }});
  svg.addEventListener("mouseleave", () => {{ tip.style.display="none"; xh.setAttribute("opacity",0); }});
}}
</script></body></html>"""
open(OUT, "w").write(html)
print(f"wrote {OUT}: {len(dates)} days, bot {st['TriggerTrader']['total']*100:+.1f}% / "
      f"BTC {st['BTC buy & hold']['total']*100:+.1f}% / SPX {st['S&P 500']['total']*100:+.1f}%")
