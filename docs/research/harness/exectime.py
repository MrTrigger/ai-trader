"""Does the result survive not being filled at the exact instant of the signal?

Features come from the bar closing at day t's open, and the backtest fills at
day t's open. Information and execution share a timestamp, so the book trades at
a price that is simultaneous with the last data it saw. Nothing real works that
way: a signal computed at 00:00 is filled some minutes later, at a price that has
already moved - and at a 0.9-day holding period there is little time for a good
decision to outrun a bad fill.

With daily bars the delay cannot be modelled at five-minute resolution, so this
brackets it instead:

  open       fill at the bar open. What the backtest assumes, and the optimistic bound.
  typical    fill at (H+L+C)/3, a crude stand-in for "sometime during the day".
  close      fill at the close, a full day after the signal. The pessimistic bound.

Entry and exit use the same convention, so each row is a consistent world rather
than a mix. A strategy that only works in the first column is a strategy that
depends on being infinitely fast.
"""
import json, math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import store
from planner.bars import mark_discontinuities
from planner.config import Config

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)

P = json.load(open(SCRATCH / "preds2.json"))
pred = {tuple(k.split("|")): v for k, v in P["daily"].items()}
predw = {tuple(k.split("|")): v for k, v in P["weekly"].items()}

bars = mark_discontinuities(store.read(root=DATA, interval_s=cfg.interval_s))
bars = bars.with_columns(
    ((pl.col("high") + pl.col("low") + pl.col("close")) / 3).alias("typical"))
PX = {}
for r in bars.select(["asset", "ts_utc", "mark_open", "typical", "close"]).iter_rows(named=True):
    PX[(r["asset"], r["ts_utc"].date())] = (r["mark_open"], r["typical"], r["close"])
COLS = {"open": 0, "typical": 1, "close": 2}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
U = timezone.utc


def fwd(asset, d0, step, col):
    a = PX.get((asset, d0)); b = PX.get((asset, d0 + timedelta(days=step)))
    if not a or not b: return None
    p0, p1 = a[COLS[col]], b[COLS[col]]
    if not p0 or not p1: return None
    return p1 / p0 - 1


def hf(a, day, n):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))


def run(table, step, col, offset=0, n_side=6):
    by = {}
    for (d, a), v in table.items():
        by.setdefault(d, []).append((a, v))
    days = sorted(by)
    prev, rows = {}, []
    i = offset
    while i < len(days):
        d = days[i]
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        ranked = sorted(by[d], key=lambda x: -x[1])
        if len(ranked) < 2 * n_side:
            i += step; continue
        L = [a for a, _ in ranked[:n_side]]; Sh = [a for a, _ in ranked[-n_side:]]
        w = {a: GROSS * 0.5 / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * 0.5 / len(Sh)
        mx = max(abs(v) for v in w.values())
        if mx > MAXPOS: w = {a: v * MAXPOS / mx for a, v in w.items()}
        got = {a: fwd(a, day.date(), step, col) for a in w}
        ok = {a: r for a, r in got.items() if r is not None}
        if len(ok) < 2 * n_side * 0.6:
            i += step; continue
        pnl = sum(w[a] * r for a, r in ok.items())
        fp = sum(-v * hf(a, day, step) for a, v in w.items())
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        rows.append((d, pnl + fp - turn * COST / 10_000))
        i += step
    return rows


def stats(rows, ppy):
    r = [x for _, x in rows]
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in r:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(r), statistics.stdev(r)
    return eq - 1, (m*ppy)/(sd*math.sqrt(ppy)) if sd else 0, dd, m/(sd/math.sqrt(len(r)))


print("fill convention applied to BOTH entry and exit\n")
print(f"{'fill at':<12}{'daily ret':>12}{'Sh':>7}{'maxDD':>8}{'t':>7}   "
      f"{'weekly ret':>12}{'Sh':>7}{'maxDD':>8}")
for col in ("open", "typical", "close"):
    dr, ds, dd_, dt = stats(run(pred, 1, col), 365)
    per = {}
    for k in range(7):
        for d, r in run(predw, 7, col, k):
            per.setdefault(datetime.fromisoformat(d).isocalendar()[:2], []).append((d, r))
    wk = sorted(x for x, v in per.items() if len(v) == 7)
    wrows = [(min(y[0] for y in per[x]), statistics.mean(y[1] for y in per[x])) for x in wk]
    wr, ws, wd, _ = stats(wrows, 52)
    mark = "  <- assumed" if col == "open" else ""
    print(f"{col:<12}{dr*100:>11.1f}%{ds:>7.2f}{dd_*100:>7.1f}%{dt:>7.2f}   "
          f"{wr*100:>11.1f}%{ws:>7.2f}{wd*100:>7.1f}%{mark}")
print("\nBTC over the same span returned 233.3% at Sharpe 0.90.")