"""At what execution cost does the daily learned book stop winning?

It turns over 264x NAV a year, so the result depends on the cost model far more
than anything measured before. The model charges a flat spread plus commission
and NO impact, which is the assumption most likely to be flattering it: 72% of
NAV a day is a lot of orders, and each one moves the book it trades against.

Cost enters linearly in turnover, so recording gross P&L and turnover once
allows an exact re-price at any rate. Reported against the break-even and
against the weekly book, which pays a fifth as much.
"""
import json, math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import features, ic, store
from planner.bars import mark_discontinuities
from planner.config import Config

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
P = json.load(open(SCRATCH / "preds.json"))
pred = {k: {tuple(kk.split("|")): v for kk, v in P[k].items()} for k in ("weekly", "daily")}
bars = store.read(root=DATA, interval_s=cfg.interval_s)
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
U = timezone.utc


def hf(a, day, n):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))


def ledger(which, step, offset=0, n_side=6):
    table = {}
    for (d, a), v in pred[which].items():
        table.setdefault(d, []).append((a, v))
    days = sorted(table)
    prev, out = {}, []
    i = offset
    while i < len(days):
        d = days[i]
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        ranked = sorted(table[d], key=lambda x: -x[1])
        if len(ranked) < 2 * n_side:
            i += step; continue
        L = [a for a, _ in ranked[:n_side]]; Sh = [a for a, _ in ranked[-n_side:]]
        w = {a: GROSS * 0.5 / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * 0.5 / len(Sh)
        mx = max(abs(v) for v in w.values())
        if mx > MAXPOS: w = {a: v * MAXPOS / mx for a, v in w.items()}
        fwd = ic._forward_returns(prices, L + Sh, day, step)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        if sum(1 for a in w if a in tab) < 2 * n_side * 0.6:
            i += step; continue
        pnl = sum(v * tab[a] for a, v in w.items() if a in tab)
        fp = sum(-v * hf(a, day, step) for a, v in w.items())
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        out.append((d, pnl + fp, turn))
        i += step
    return out


def price(rows, bps, ppy):
    r = [g - t * bps / 10_000 for _, g, t in rows]
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in r:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(r), statistics.stdev(r)
    return eq - 1, (m*ppy)/(sd*math.sqrt(ppy)) if sd else 0.0, dd


daily = ledger("daily", 1)
per = {}
for k in range(7):
    for d, g, t in ledger("weekly", 7, k):
        per.setdefault(datetime.fromisoformat(d).isocalendar()[:2], []).append((d, g, t))
wk = sorted(w for w, v in per.items() if len(v) == 7)
weekly = [(min(x[0] for x in per[w]), statistics.mean(x[1] for x in per[w]),
           statistics.mean(x[2] for x in per[w])) for w in wk]

print(f"{'all-in bps':>11}{'daily ret':>12}{'Sh':>7}{'maxDD':>8}   "
      f"{'weekly ret':>12}{'Sh':>7}{'maxDD':>8}")
for bps in (0, 2, 4, 6.5, 10, 15, 20, 30, 40):
    dr, ds, dd_ = price(daily, bps, 365)
    wr, ws, wd = price(weekly, bps, 52)
    mark = "  <- assumed" if bps == 6.5 else ""
    print(f"{bps:>11.1f}{dr*100:>11.1f}%{ds:>7.2f}{dd_*100:>7.1f}%   "
          f"{wr*100:>11.1f}%{ws:>7.2f}{wd*100:>7.1f}%{mark}")


def breakeven(rows, ppy, target):
    lo, hi = 0.0, 300.0
    for _ in range(60):
        mid = (lo + hi) / 2
        if price(rows, mid, ppy)[0] > target: lo = mid
        else: hi = mid
    return lo


print(f"\nbreak-even vs zero    daily {breakeven(daily,365,0):.1f}bps   "
      f"weekly {breakeven(weekly,52,0):.1f}bps")
print(f"break-even vs BTC     daily {breakeven(daily,365,2.333):.1f}bps"
      f"   (BTC returned 233.3% over the same span)")
print(f"\nturnover  daily {statistics.mean(t for _,_,t in daily)*365*100:,.0f}%/yr"
      f"   weekly {statistics.mean(t for _,_,t in weekly)*52*100:,.0f}%/yr")
print(f"mean holding period, daily book: "
      f"{1/ (statistics.mean(t for _,_,t in daily)/ (GROSS)) :.1f} days")