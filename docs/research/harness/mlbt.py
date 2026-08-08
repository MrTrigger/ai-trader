"""Backtest both learned rankers, on their out-of-sample predictions only.

The comparison is the one that was posed: a daily book turns over roughly three
times as much and pays for it, so the question is whether a model that predicts
day-to-day ordering earns more than the extra fees take.

Only the walk-forward TEST blocks are traded. Nothing here is priced on a
prediction the model made about data it had trained on, which is why the window
starts in 2022-09 rather than 2019 - shorter, but genuinely out of sample, which
the channel results never were.

Market-neutral, no regime tilt. The tilt is a separate claim and mixing it in
would make it impossible to say which part earned the return.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure)
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)   # 4.5 + 2 = 6.5bps

P = json.load(open(SCRATCH / "preds.json"))
pred = {k: {tuple(kk.split("|")): v for kk, v in P[k].items()} for k in ("weekly", "daily")}

bars = store.read(root=DATA, interval_s=cfg.interval_s)
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BTC_PX = {r["ts_utc"]: r["mark_open"] for r in
          prices.filter(pl.col("asset") == cfg.benchmark).iter_rows(named=True)}
U = timezone.utc


def hf(a, day, n):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))


def by_date(which):
    out = {}
    for (d, a), v in pred[which].items():
        out.setdefault(d, []).append((a, v))
    return out


def run(which, step, start_offset=0, n_side=6):
    """Long the top `n_side` by prediction, short the bottom `n_side`."""
    table = by_date(which)
    days = sorted(table)
    prev, rows = {}, []
    i = start_offset
    while i < len(days):
        d = days[i]
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        ranked = sorted(table[d], key=lambda x: -x[1])
        if len(ranked) < 2 * n_side:
            i += step; continue
        L = [a for a, _ in ranked[:n_side]]
        Sh = [a for a, _ in ranked[-n_side:]]
        w = {a: GROSS * 0.5 / len(L) for a in L}
        for a in Sh:
            w[a] = w.get(a, 0.0) - GROSS * 0.5 / len(Sh)
        if max(abs(v) for v in w.values()) > MAXPOS:
            scale = MAXPOS / max(abs(v) for v in w.values())
            w = {a: v * scale for a, v in w.items()}
        fwd = ic._forward_returns(prices, L + Sh, day, step)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        if sum(1 for a in w if a in tab) < 2 * n_side * 0.6:
            i += step; continue
        pnl = sum(v * tab[a] for a, v in w.items() if a in tab)
        # Perp long pays funding, perp short receives it.
        fp = sum(-v * hf(a, day, step) for a, v in w.items())
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        rows.append((d, pnl + fp - turn * COST / 10_000, turn))
        i += step
    return rows


def stats(rows, ppy):
    r = [x for _, x, _ in rows]
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in r:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(r), statistics.stdev(r)
    turn = statistics.mean(t for _, _, t in rows)
    return {"n": len(r), "final": eq - 1, "sharpe": (m*ppy)/(sd*math.sqrt(ppy)) if sd else 0,
            "maxdd": dd, "t": m/(sd/math.sqrt(len(r))), "turn_yr": turn * ppy,
            "cost_yr": turn * ppy * COST / 10_000, "first": rows[0][0], "last": rows[-1][0]}


# Weekly: tranche across all seven phases, as the shipped book does.
per = {}
for k in range(7):
    for d, r, t in run("weekly", 7, k):
        per.setdefault(datetime.fromisoformat(d).isocalendar()[:2], []).append((d, r, t))
wk = sorted(w for w, v in per.items() if len(v) == 7)
weekly_rows = [(min(x[0] for x in per[w]), statistics.mean(x[1] for x in per[w]),
                statistics.mean(x[2] for x in per[w])) for w in wk]
W = stats(weekly_rows, 52)
D = stats(run("daily", 1), 365)

# BTC over the same span, weekly.
first = datetime.fromisoformat(W["first"]).replace(tzinfo=U)
last = datetime.fromisoformat(W["last"]).replace(tzinfo=U)
b, d0, last_px = [], first, None
while d0 <= last:
    px = BTC_PX.get(d0)
    if px is not None and last_px is not None:
        b.append(px / last_px - 1)
    if px is not None: last_px = px
    d0 += timedelta(days=7)
beq, bpk, bdd = 1.0, 1.0, 0.0
for x in b:
    beq *= 1 + x; bpk = max(bpk, beq); bdd = min(bdd, beq / bpk - 1)
bm, bsd = statistics.mean(b), statistics.stdev(b)

print(f"out-of-sample only: {W['first']} .. {W['last']}\n")
print(f"{'':<26}{'n':>6}{'return':>10}{'Sharpe':>8}{'maxDD':>8}{'t':>7}{'turnover/yr':>13}{'cost/yr':>9}")
print(f"{'ML weekly (tranched)':<26}{W['n']:>6}{W['final']*100:>9.1f}%{W['sharpe']:>8.2f}"
      f"{W['maxdd']*100:>7.1f}%{W['t']:>7.2f}{W['turn_yr']*100:>12,.0f}%{W['cost_yr']*100:>8.1f}%")
print(f"{'ML daily':<26}{D['n']:>6}{D['final']*100:>9.1f}%{D['sharpe']:>8.2f}"
      f"{D['maxdd']*100:>7.1f}%{D['t']:>7.2f}{D['turn_yr']*100:>12,.0f}%{D['cost_yr']*100:>8.1f}%")
print(f"{'BTC buy & hold':<26}{len(b):>6}{(beq-1)*100:>9.1f}%"
      f"{(bm*52)/(bsd*math.sqrt(52)):>8.2f}{bdd*100:>7.1f}%")

print("\nwhat the daily book would need to justify its extra cost:")
print(f"  extra fees {(D['cost_yr']-W['cost_yr'])*100:+.1f}%/yr")
gross_w = W["final"]; gross_d = D["final"]
print(f"  it returned {gross_d*100:.1f}% against weekly's {gross_w*100:.1f}%")
json.dump({"weekly": W, "daily": D}, open(SCRATCH / "mlbt.json", "w"), default=str)