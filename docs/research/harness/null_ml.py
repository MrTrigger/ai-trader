"""Can this result be produced from noise? Two nulls, both keeping everything else.

Sharpe 2.70 was selected across roughly twenty configurations on one
out-of-sample window. That is exactly the situation where a number looks
established and is not, and this project has already retracted four results that
looked solid until the right instrument was applied.

**Null A - shuffle the predictions.** Permute the model's scores within each
date. Same dates, same universe, same weights machinery, same costs, same
turnover profile - only WHICH asset gets which score is randomised. If the book
still performs, the ranking contributes nothing and the return is coming from the
construction (dollar-neutral, vol-weighted, cost-thresholded), which would be a
finding about portfolio maths rather than about prediction.

**Null B - shuffle the training labels.** Permute the target within each training
date, retrain the model on that, and evaluate it against the REAL test returns.
The model now has nothing to learn, so any apparent out-of-sample edge is leakage
or an artifact of the cross-validation itself. This is the stronger test: it
catches faults in the harness, not just in the signal.

Null B is the one that would invalidate the whole line of work, so it gets the
same fold structure, purge and hyperparameters as the real run - changed in one
respect only.
"""
import math, statistics, sys
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
ROUND_TRIP, K = 2 * COST / 10_000, 1.0
U = timezone.utc; LAG_H, HOLD_H = 1, 24
MIN_NAMES, MAX_NAMES = 6, 24

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
EDGES = np.linspace(int(len(dates) * 0.4), len(dates), 7).astype(int)

hourly = store.read(root=DATA, interval_s=3600).select(["asset", "ts_utc", "open"])
PX = {(a, t): o for a, t, o in hourly.iter_rows()}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def train_predict(shuffle_labels=False, seed=0):
    rng = np.random.default_rng(seed)
    out = {}
    for k in range(6):
        lo, hi = EDGES[k], EDGES[k + 1]
        tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo - 1)]))
        te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
        if tr.height < 3000 or te.height < 300:
            continue
        lab = tr["t1"].to_numpy().copy()
        if shuffle_labels:
            # Permute WITHIN each date: the target's distribution is preserved,
            # its cross-sectional information is destroyed.
            d = tr["date"].to_numpy()
            start = 0
            for i in range(1, len(d) + 1):
                if i == len(d) or d[i] != d[start]:
                    rng.shuffle(lab[start:i]); start = i
        m = lgb.train(PARAMS, lgb.Dataset(tr.select(XC).to_numpy(), label=lab),
                      num_boost_round=300)
        for dt, a, v, rv in zip(te["date"].to_list(), te["asset"].to_list(),
                                m.predict(te.select(XC).to_numpy()), te["rv_24h"].to_list()):
            out[(dt, a)] = (float(v), float(rv) if rv else None)
    return out


def build(rows):
    thr = K * ROUND_TRIP
    cand = [(a, p, v) for a, p, v in rows if abs(p) >= thr and v]
    if len(cand) < MIN_NAMES:
        return {}
    cand.sort(key=lambda x: -abs(x[1]))
    cand = cand[:MAX_NAMES]
    L = [(a, abs(p), v) for a, p, v in cand if p > 0]
    Sh = [(a, abs(p), v) for a, p, v in cand if p <= 0]
    if len(L) < 2 or len(Sh) < 2:
        return {}
    def side(items):
        raw = {a: e / v for a, e, v in items}
        t = sum(raw.values()) or 1.0
        return {a: x / t for a, x in raw.items()}
    w = {}
    for a, x in side(L).items(): w[a] = GROSS * 0.5 * x
    for a, x in side(Sh).items(): w[a] = w.get(a, 0.0) - GROSS * 0.5 * x
    mx = max(abs(v) for v in w.values())
    return {a: v * MAXPOS / mx for a, v in w.items()} if mx > MAXPOS else w


def backtest(preds):
    by = {}
    for (d, a), (p, v) in preds.items():
        by.setdefault(d, []).append((a, p, v))
    prev, rets = {}, []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        w = build(by[d])
        if not w:
            turn = sum(abs(x) for x in prev.values()); prev = {}
            if turn: rets.append(-turn * COST / 10_000)
            continue
        got = {}
        for a in w:
            p0 = PX.get((a, entry)); p1 = PX.get((a, entry + timedelta(hours=HOLD_H)))
            if p0 and p1: got[a] = p1 / p0 - 1
        if len(got) < max(4, len(w) * 0.6):
            continue
        pnl = sum(w[a] * r for a, r in got.items())
        fp = sum(-v * ftab.get(a, {}).get(day.date(), 0.0) for a, v in w.items())
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        rets.append(pnl + fp - turn * COST / 10_000)
    if len(rets) < 100:
        return None
    eq = 1.0
    for v in rets: eq *= 1 + v
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return eq - 1, (m * 365) / (sd * math.sqrt(365))


real_preds = train_predict()
real_ret, real_sh = backtest(real_preds)
print(f"REAL   return {real_ret*100:>9.1f}%   Sharpe {real_sh:>5.2f}\n", flush=True)

print("NULL A - predictions permuted within each date (20 draws)")
a_sh = []
rng = np.random.default_rng(7)
by_date = {}
for (d, a), pv in real_preds.items():
    by_date.setdefault(d, []).append((a, pv))
for i in range(20):
    shuffled = {}
    for d, items in by_date.items():
        vals = [pv for _, pv in items]
        order = rng.permutation(len(vals))
        for (a, _), j in zip(items, order):
            shuffled[(d, a)] = vals[j]
    r = backtest(shuffled)
    if r: a_sh.append(r[1])
    if i < 3 or i == 19:
        print(f"   draw {i+1:>2}: Sharpe {r[1]:+.2f}", flush=True)
print(f"   null A: median {statistics.median(a_sh):+.2f}  max {max(a_sh):+.2f}  "
      f"beat by real: {sum(1 for x in a_sh if x >= real_sh)}/{len(a_sh)}")

print("\nNULL B - training labels permuted within date, retrained (10 draws)")
b_sh = []
for i in range(10):
    p = train_predict(shuffle_labels=True, seed=100 + i)
    r = backtest(p)
    if r: b_sh.append(r[1])
    print(f"   draw {i+1:>2}: Sharpe {r[1]:+.2f}   return {r[0]*100:+.1f}%", flush=True)
print(f"   null B: median {statistics.median(b_sh):+.2f}  max {max(b_sh):+.2f}")

better = sum(1 for x in b_sh if x >= real_sh)
print(f"\nreal Sharpe {real_sh:.2f}")
print(f"  null A (ranking is noise)      beat it {sum(1 for x in a_sh if x >= real_sh)}/{len(a_sh)}"
      f"  -> p = {(sum(1 for x in a_sh if x >= real_sh)+1)/(len(a_sh)+1):.3f}")
print(f"  null B (learned from noise)    beat it {better}/{len(b_sh)}"
      f"  -> p = {(better+1)/(len(b_sh)+1):.3f}")