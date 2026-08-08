"""Is rank 1 actually better than rank 13, or is the ordering noise?

"Use the best configuration" is only sound if the ranking carries information.
Two things decide that:

**How precisely is any single Sharpe measured?** Over 1,353 daily observations
the standard error of an annualised Sharpe near 2.5 is not small. If it exceeds
the spread of the whole search, the ranking is sorting noise.

**How much do two configurations differ, given they share the same data?** Their
errors are highly correlated - same days, same universe, same predictions - so
the difference is measured far more precisely than either level. That paired
comparison is the right test, and it is the one that decides whether switching
is justified.

The maximum of N draws is also a biased estimator of the next result even when
every configuration is genuinely identical: with 72 draws and this spread, the
expected maximum sits well above the expected value of any one of them. Picking
the top is therefore choosing the most inflated number in the set.
"""
import math, statistics
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
P = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
         min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
         bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
E = np.linspace(int(len(dates)*0.4), len(dates), 7).astype(int)
preds = {}
for k in range(6):
    lo, hi = E[k], E[k+1]
    tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo-1)]))
    te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
    if tr.height < 3000 or te.height < 300: continue
    m = lgb.train(P, lgb.Dataset(tr.select(XC).to_numpy(), label=tr["t1"].to_numpy()),
                  num_boost_round=300)
    for d, a, v, rv in zip(te["date"].to_list(), te["asset"].to_list(),
                           m.predict(te.select(XC).to_numpy()), te["rv_24h"].to_list()):
        preds[(d, a)] = (float(v), float(rv) if rv else None)
h = store.read(root=DATA, interval_s=3600).sort(["asset","ts_utc"])
h = h.with_columns((pl.col("close")/pl.col("close").shift(1).over("asset")-1).alias("r1"))
h = h.with_columns(pl.col("r1").rolling_std(24, min_samples=12).over("asset").alias("sig"))
PX, VOL, SIG = {}, {}, {}
for a,t,o,qv,sg in h.select(["asset","ts_utc","open","quote_volume","sig"]).iter_rows():
    PX[(a,t)] = o; VOL[(a,t)] = qv; SIG[(a,t)] = sg
fund = pl.read_parquet(DATA/"funding"/"binance_um.parquet")
ftab = {(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k,v in fund.partition_by("asset", as_dict=True).items()}
by = {}
for (d,a),(p,v) in preds.items(): by.setdefault(d, []).append((a,p,v))


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


def series(sizing, k_cost, max_names, liq):
    prev, out = {}, {}
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U); entry = day+timedelta(hours=LAG_H)
        rows = sorted(by[d], key=lambda x: -abs(x[1]))
        cand = [r for r in rows if abs(r[1]) >= k_cost*ROUND_TRIP and r[2]][:max_names]
        L = [(a,p,v) for a,p,v in cand if p > 0]; Sh = [(a,p,v) for a,p,v in cand if p <= 0]
        if len(L) < 2 or len(Sh) < 2:
            t = sum(abs(x) for x in prev.values()); prev = {}
            if t: out[d] = -t*COST/10_000
            continue
        def ap(items):
            if sizing == "equal": return [(a,1.0) for a,_,_ in items]
            if sizing == "conviction": return [(a,abs(p)) for a,p,_ in items]
            if sizing == "risk-adj": return [(a,abs(p)/v) for a,p,v in items]
            return [(a,1.0/v) for a,_,v in items]
        caps = {}
        for a,_,_ in cand:
            c = MAXPOS
            if liq: c = min(c, 0.05*(VOL.get((a,entry)) or 0.0)/NAV)
            caps[a] = max(c, 0.0)
        w = waterfall(ap(L), GROSS*0.5, caps)
        for a,x in waterfall(ap(Sh), GROSS*0.5, caps).items(): w[a] = w.get(a,0.0)-x
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
        prev = w; out[d] = pnl+fp-c
    return out


def sharpe(r):
    m, sd = statistics.mean(r), statistics.stdev(r)
    return (m*365)/(sd*math.sqrt(365))


def se_sharpe(r):
    """Lo (2002): SE of an annualised Sharpe from n iid observations."""
    n = len(r); s = statistics.mean(r)/statistics.stdev(r)
    return math.sqrt((1 + 0.5*s*s)/n) * math.sqrt(365)


best = series("inv-vol", 0.0, 40, True)      # rank 1
ship = series("risk-adj", 1.0, 24, True)     # rank 13
common = sorted(set(best) & set(ship))
b = [best[d] for d in common]; s_ = [ship[d] for d in common]
print(f"{len(common):,} common days\n")
print(f"{'':<22}{'Sharpe':>9}{'SE':>8}")
print(f"{'rank 1  inv-vol/40':<22}{sharpe(b):>9.2f}{se_sharpe(b):>8.2f}")
print(f"{'rank 13 shipped':<22}{sharpe(s_):>9.2f}{se_sharpe(s_):>8.2f}")

mb, ms = statistics.mean(b), statistics.mean(s_)
sb, ss = statistics.pstdev(b), statistics.pstdev(s_)
rho = sum((x-mb)*(y-ms) for x, y in zip(b, s_))/len(b)/(sb*ss)
print(f"\ncorrelation of their daily returns: {rho:.3f}")

# Paired bootstrap on the DIFFERENCE - the shared noise cancels, which is why
# this is far more precise than comparing two standalone Sharpes.
rng = np.random.default_rng(0)
diffs = []
arr_b, arr_s = np.array(b), np.array(s_)
for _ in range(4000):
    i = rng.integers(0, len(b), len(b))
    xb, xs = arr_b[i], arr_s[i]
    diffs.append((xb.mean()/xb.std())*math.sqrt(365) - (xs.mean()/xs.std())*math.sqrt(365))
diffs = np.array(diffs)
lo, hi = np.percentile(diffs, [2.5, 97.5])
print(f"\nSharpe difference (rank 1 minus shipped): {sharpe(b)-sharpe(s_):+.2f}")
print(f"  95% bootstrap interval [{lo:+.2f}, {hi:+.2f}]")
print(f"  probability rank 1 is genuinely better: {(diffs > 0).mean()*100:.0f}%")

# What the maximum of 72 draws is worth when nothing genuinely differs.
sd_cfg = 0.39
exp_max = sd_cfg * (2*math.log(72))**0.5
print(f"\nif all 72 configurations were truly identical and differed only by noise,")
print(f"the expected maximum would still sit about {exp_max:.2f} Sharpe above the mean.")
print(f"The observed spread (min 1.59, max 2.73) is {2.73-1.59:.2f} wide.")