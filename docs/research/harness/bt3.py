"""Backtest the hourly-feature model, and isolate what each change bought.

Two things changed at once - features moved from daily to hourly, and the target
moved from "return starting at the signal instant" to "return starting an hour
later". IC cannot compare them because IC is measured against whichever target
was used. The backtest can, because both are priced against the same trade.

Three rows:
  daily features, 0-lag target     the previous model, traded at a 1h lag anyway
  hourly features, 1h-lag target   trained on the trade it will actually make
  BTC                              the bar
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import store
from planner.config import Config

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)   # now 4.5 + 0.5
U = timezone.utc
LAG_H, HOLD_H = 1, 24

hourly = store.read(root=DATA, interval_s=3600).select(["asset", "ts_utc", "open"])
PX = {(a, t): o for a, t, o in hourly.iter_rows()}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def load(fn, key):
    P = json.load(open(SCRATCH / fn))
    return {tuple(k.split("|")): v for k, v in P[key].items()}


def run(table, n_side=6):
    by = {}
    for (d, a), v in table.items():
        by.setdefault(d, []).append((a, v))
    prev, rows = {}, []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        ranked = sorted(by[d], key=lambda x: -x[1])
        if len(ranked) < 2 * n_side:
            continue
        L = [a for a, _ in ranked[:n_side]]; Sh = [a for a, _ in ranked[-n_side:]]
        w = {a: GROSS * 0.5 / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * 0.5 / len(Sh)
        mx = max(abs(v) for v in w.values())
        if mx > MAXPOS: w = {a: v * MAXPOS / mx for a, v in w.items()}
        got = {}
        for a in w:
            p0 = PX.get((a, entry)); p1 = PX.get((a, entry + timedelta(hours=HOLD_H)))
            if p0 and p1: got[a] = p1 / p0 - 1
        if len(got) < 2 * n_side * 0.6:
            continue
        pnl = sum(w[a] * r for a, r in got.items())
        fp = sum(-v * ftab.get(a, {}).get(day.date(), 0.0) for a, v in w.items())
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        rows.append((d, pnl + fp - turn * COST / 10_000))
    return rows


def stats(rows, ppy=365):
    r = [x for _, x in rows]
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in r:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(r), statistics.stdev(r)
    return len(r), eq - 1, (m*ppy)/(sd*math.sqrt(ppy)), dd, m/(sd/math.sqrt(len(r)))


print(f"all-in {COST}bps, {LAG_H}h fill lag, {HOLD_H}h hold\n")
print(f"{'model':<34}{'n':>6}{'return':>11}{'Sharpe':>9}{'maxDD':>8}{'t':>7}")
for label, fn, key in [
    ("daily features, 0-lag target", "preds2.json", "daily"),
    ("hourly features, 1h-lag target", "preds3.json", "daily"),
]:
    s = stats(run(load(fn, key)))
    print(f"{label:<34}{s[0]:>6}{s[1]*100:>10.1f}%{s[2]:>9.2f}{s[3]*100:>7.1f}%{s[4]:>7.2f}")

# BTC on the same dates.
d0 = sorted(load("preds3.json", "daily"))[0][0]
d1 = sorted(load("preds3.json", "daily"))[-1][0]
day = datetime.fromisoformat(d0).replace(tzinfo=U)
end = datetime.fromisoformat(d1).replace(tzinfo=U)
b, last = [], None
while day <= end:
    px = PX.get(("BTC", day + timedelta(hours=LAG_H)))
    if px and last: b.append(px / last - 1)
    if px: last = px
    day += timedelta(days=1)
eq, pk, dd = 1.0, 1.0, 0.0
for x in b:
    eq *= 1 + x; pk = max(pk, eq); dd = min(dd, eq/pk - 1)
m, sd = statistics.mean(b), statistics.stdev(b)
print(f"{'BTC buy & hold':<34}{len(b):>6}{(eq-1)*100:>10.1f}%"
      f"{(m*365)/(sd*math.sqrt(365)):>9.2f}{dd*100:>7.1f}%")