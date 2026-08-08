"""How fast do we have to be? Now measurable, at hourly resolution.

The daily backtest could only bracket this: fill at the bar open (Sharpe 2.56)
or fill at the close (0.30), 24 hours apart, with the truth somewhere inside.
Hourly bars turn the bracket into a curve.

Everything is held constant except the fill. Same model, same predictions, same
features, same holding period - only the delay between the timestamp the signal
was computed on and the price actually paid. A signal formed on data through
00:00 is filled at the open of 00:00 + lag, and exited 24 hours after that.

The shape of the curve is the finding. A strategy whose edge is intact at 1h and
gone at 4h is a real but latency-bound strategy. One that dies by 1h was never
tradeable.
"""
import json, math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import store
from planner.config import Config

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
U = timezone.utc

P = json.load(open(SCRATCH / "preds2.json"))
daily_pred = {tuple(k.split("|")): v for k, v in P["daily"].items()}
weekly_pred = {tuple(k.split("|")): v for k, v in P["weekly"].items()}

hourly = store.read(root=DATA, interval_s=3600).select(["asset", "ts_utc", "open"])
PX = {(a, t): o for a, t, o in hourly.iter_rows()}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def hf(a, day, n):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))


def fwd(asset, entry_ts, hold_h):
    p0 = PX.get((asset, entry_ts))
    p1 = PX.get((asset, entry_ts + timedelta(hours=hold_h)))
    if not p0 or not p1:
        return None
    return p1 / p0 - 1


def run(table, step_days, lag_h, hold_h, offset=0, n_side=6):
    by = {}
    for (d, a), v in table.items():
        by.setdefault(d, []).append((a, v))
    days = sorted(by)
    prev, rows = {}, []
    i = offset
    while i < len(days):
        d = days[i]
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=lag_h)
        ranked = sorted(by[d], key=lambda x: -x[1])
        if len(ranked) < 2 * n_side:
            i += step_days; continue
        L = [a for a, _ in ranked[:n_side]]; Sh = [a for a, _ in ranked[-n_side:]]
        w = {a: GROSS * 0.5 / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * 0.5 / len(Sh)
        mx = max(abs(v) for v in w.values())
        if mx > MAXPOS: w = {a: v * MAXPOS / mx for a, v in w.items()}
        got = {a: fwd(a, entry, hold_h) for a in w}
        ok = {a: r for a, r in got.items() if r is not None}
        if len(ok) < 2 * n_side * 0.6:
            i += step_days; continue
        pnl = sum(w[a] * r for a, r in ok.items())
        fp = sum(-v * hf(a, day, max(1, hold_h // 24)) for a, v in w.items())
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        rows.append((d, pnl + fp - turn * COST / 10_000))
        i += step_days
    return rows


def stats(rows, ppy):
    r = [x for _, x in rows]
    if len(r) < 20: return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in r:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(r), statistics.stdev(r)
    return eq - 1, (m*ppy)/(sd*math.sqrt(ppy)) if sd else 0, dd, m/(sd/math.sqrt(len(r)))


print("DAILY model: signal on data through 00:00, filled `lag` hours later,")
print("held 24h from the fill.\n")
print(f"{'lag':>6}{'return':>12}{'Sharpe':>9}{'maxDD':>9}{'t':>7}")
for lag in (0, 1, 2, 4, 8, 12, 24):
    s = stats(run(daily_pred, 1, lag, 24), 365)
    if s:
        mark = "  <- what the daily backtest assumed" if lag == 0 else ""
        print(f"{lag:>5}h{s[0]*100:>11.1f}%{s[1]:>9.2f}{s[2]*100:>8.1f}%{s[3]:>7.2f}{mark}")

print("\nWEEKLY model, tranched over 7 phases, held 168h from the fill.\n")
print(f"{'lag':>6}{'return':>12}{'Sharpe':>9}{'maxDD':>9}{'t':>7}")
for lag in (0, 1, 4, 12, 24):
    per = {}
    for k in range(7):
        for d, r in run(weekly_pred, 7, lag, 168, k):
            per.setdefault(datetime.fromisoformat(d).isocalendar()[:2], []).append((d, r))
    wk = sorted(x for x, v in per.items() if len(v) == 7)
    rows = [(min(y[0] for y in per[x]), statistics.mean(y[1] for y in per[x])) for x in wk]
    s = stats(rows, 52)
    if s:
        print(f"{lag:>5}h{s[0]*100:>11.1f}%{s[1]:>9.2f}{s[2]*100:>8.1f}%{s[3]:>7.2f}")
print("\nBTC buy & hold over the same span: +233.3%, Sharpe 0.90, maxDD -51.8%")