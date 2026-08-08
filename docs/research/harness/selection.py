"""How much of Sharpe 2.60 is the strategy, and how much is having looked twenty times?

A true holdout is no longer available - every window has been inspected - so the
next best thing is to measure the SEARCH rather than pretend it did not happen.

The shipped configuration was chosen across a space of construction decisions:
how to size, whether to threshold, how many names to hold, whether to cap by
liquidity. Running that whole space and looking at the distribution of outcomes
answers the question directly. If every configuration lands between 2.3 and 2.7,
the choice barely mattered and the number is close to honest. If they range from
0.5 to 2.7, the chosen one is the top of a wide draw and regression toward the
middle should be expected.

Reported as a percentile, because "the best of N" is a biased estimator of the
next result and the size of that bias is what the spread tells us.

Model predictions are held fixed throughout - this measures the CONSTRUCTION
search, not the modelling search, so it is a lower bound on the total selection
effect. Feature sets, targets, horizons and model families were also searched.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import numpy as np
import polars as pl
import lightgbm as lgb
from planner import store
from planner.config import Config

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
DATA = Path("/home/magnus/dev/magnus/ai-trader/data")
cfg = Config.load(Path("/home/magnus/dev/magnus/ai-trader/config/default.toml"))
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
ROUND_TRIP = 2 * COST / 10_000
U = timezone.utc; LAG_H, HOLD_H = 1, 24
NAV, Y_IMPACT = 30_000, 0.3

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
EDGES = np.linspace(int(len(dates) * 0.4), len(dates), 7).astype(int)
preds, folds = {}, {}
for k in range(6):
    lo, hi = EDGES[k], EDGES[k + 1]
    tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo - 1)]))
    te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
    if tr.height < 3000 or te.height < 300: continue
    m = lgb.train(PARAMS, lgb.Dataset(tr.select(XC).to_numpy(), label=tr["t1"].to_numpy()),
                  num_boost_round=300)
    for d, a, v, rv in zip(te["date"].to_list(), te["asset"].to_list(),
                           m.predict(te.select(XC).to_numpy()), te["rv_24h"].to_list()):
        preds[(d, a)] = (float(v), float(rv) if rv else None)
        folds[d] = k
print(f"{len(preds):,} predictions across 6 folds", flush=True)

h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
h = h.with_columns((pl.col("close")/pl.col("close").shift(1).over("asset")-1).alias("r1"))
h = h.with_columns(pl.col("r1").rolling_std(24, min_samples=12).over("asset").alias("sig"))
PX, VOL, SIG = {}, {}, {}
for a, t, o, qv, sg in h.select(["asset","ts_utc","open","quote_volume","sig"]).iter_rows():
    PX[(a,t)] = o; VOL[(a,t)] = qv; SIG[(a,t)] = sg
fund = pl.read_parquet(DATA/"funding"/"binance_um.parquet")
ftab = {(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k,v in fund.partition_by("asset", as_dict=True).items()}
by = {}
for (d,a),(p,v) in preds.items():
    by.setdefault(d, []).append((a,p,v))


def waterfall(items, budget, caps):
    w, rem = {}, budget
    for _ in range(12):
        op = [(a,s) for a,s in items if caps.get(a,0)-w.get(a,0.0) > 1e-9]
        if not op or rem <= 1e-9: break
        tot = sum(s for _,s in op) or 1.0
        moved = False
        for a,s in op:
            take = min(rem*s/tot, caps[a]-w.get(a,0.0))
            if take > 1e-12: w[a] = w.get(a,0.0)+take; moved = True
        rem = budget - sum(w.values())
        if not moved: break
    return w


def run(sizing, k_cost, max_names, liq):
    prev, rets, per_fold = {}, [], {}
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U); entry = day+timedelta(hours=LAG_H)
        rows = sorted(by[d], key=lambda x: -abs(x[1]))
        cand = [r for r in rows if abs(r[1]) >= k_cost*ROUND_TRIP and r[2]][:max_names]
        L = [(a,p,v) for a,p,v in cand if p > 0]
        Sh = [(a,p,v) for a,p,v in cand if p <= 0]
        if len(L) < 2 or len(Sh) < 2:
            t = sum(abs(x) for x in prev.values()); prev = {}
            if t: rets.append((d, -t*COST/10_000))
            continue
        def appetite(items):
            if sizing == "equal": return [(a, 1.0) for a,_,_ in items]
            if sizing == "conviction": return [(a, abs(p)) for a,p,_ in items]
            if sizing == "risk-adj": return [(a, abs(p)/v) for a,p,v in items]
            return [(a, 1.0/v) for a,_,v in items]
        caps = {}
        for a,_,_ in cand:
            c = MAXPOS
            if liq:
                vh = VOL.get((a,entry)) or 0.0
                c = min(c, 0.05*vh/NAV)
            caps[a] = max(c, 0.0)
        w = waterfall(appetite(L), GROSS*0.5, caps)
        for a,x in waterfall(appetite(Sh), GROSS*0.5, caps).items(): w[a] = w.get(a,0.0)-x
        w = {a:x for a,x in w.items() if abs(x) > 1e-6}
        if not w: continue
        got = {}
        for a in w:
            p0 = PX.get((a,entry)); p1 = PX.get((a,entry+timedelta(hours=HOLD_H)))
            if p0 and p1: got[a] = p1/p0-1
        if len(got) < max(4, len(w)*0.6): continue
        pnl = sum(w[a]*r for a,r in got.items())
        fp = sum(-v*ftab.get(a,{}).get(day.date(),0.0) for a,v in w.items())
        c = 0.0
        for a in set(w)|set(prev):
            dw = abs(w.get(a,0.0)-prev.get(a,0.0))
            if dw <= 0: continue
            c += dw*COST/10_000
            vh = VOL.get((a,entry)) or 0.0
            if vh > 0: c += dw*Y_IMPACT*(SIG.get((a,entry)) or 0.02)*math.sqrt(dw*NAV/vh)
        prev = w; rets.append((d, pnl+fp-c))
    if len(rets) < 200: return None
    r = [x for _, x in rets]
    m, sd = statistics.mean(r), statistics.stdev(r)
    sh = (m*365)/(sd*math.sqrt(365))
    for d, x in rets: per_fold.setdefault(folds.get(d, -1), []).append(x)
    fs = {}
    for k, v in sorted(per_fold.items()):
        if len(v) > 30:
            mm, ss = statistics.mean(v), statistics.stdev(v)
            fs[k] = (mm*365)/(ss*math.sqrt(365))
    return sh, fs


SIZINGS = ("equal", "conviction", "risk-adj", "inv-vol")
KS = (0.0, 1.0, 2.0)
NAMES = (12, 24, 40)
LIQ = (False, True)
results = []
for s_ in SIZINGS:
    for k_ in KS:
        for n_ in NAMES:
            for l_ in LIQ:
                r = run(s_, k_, n_, l_)
                if r: results.append((r[0], s_, k_, n_, l_, r[1]))
results.sort(reverse=True)
print(f"\n{len(results)} configurations in the construction search\n")
print(f"{'rank':>5}{'Sharpe':>8}  {'sizing':<12}{'k':>4}{'names':>7}{'liq':>5}")
for i, (sh, s_, k_, n_, l_, _) in enumerate(results[:5], 1):
    print(f"{i:>5}{sh:>8.2f}  {s_:<12}{k_:>4.0f}{n_:>7}{'yes' if l_ else 'no':>5}")
print("   ...")
for i, (sh, s_, k_, n_, l_, _) in enumerate(results[-3:], len(results)-2):
    print(f"{i:>5}{sh:>8.2f}  {s_:<12}{k_:>4.0f}{n_:>7}{'yes' if l_ else 'no':>5}")

vals = sorted(sh for sh, *_ in results)
def pct(p): return vals[int(p*(len(vals)-1))]
print(f"\ndistribution across the search:")
print(f"  min {vals[0]:.2f}   25th {pct(.25):.2f}   median {pct(.5):.2f}   "
      f"75th {pct(.75):.2f}   max {vals[-1]:.2f}")
print(f"  mean {statistics.mean(vals):.2f}   sd {statistics.stdev(vals):.2f}")

SHIPPED = ("risk-adj", 1.0, 24, True)
for sh, s_, k_, n_, l_, fs in results:
    if (s_, k_, n_, l_) == SHIPPED:
        rank = sorted(vals, reverse=True).index(sh) + 1
        print(f"\nthe shipped configuration ({s_}, k={k_:.0f}, {n_} names, liquidity cap):")
        print(f"  Sharpe {sh:.2f}, rank {rank} of {len(results)}, "
              f"{(1 - (rank-1)/len(results))*100:.0f}th percentile")
        print(f"  excess over the median configuration: {sh - pct(.5):+.2f} Sharpe")
        print(f"  per fold: " + "  ".join(f"f{k+1} {v:.2f}" for k, v in sorted(fs.items())))
        break