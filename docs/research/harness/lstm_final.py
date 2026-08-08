"""Train the winning LSTM variant, save its predictions, and re-derive the blend.

Runs only if the ablation shows the rank target repairs the short side. Two
things the earlier blend result got wrong and this fixes:

**The blend weight was assumed, not fitted.** 50/50 was picked because it is the
obvious split, not because anything measured said so. If one model is stronger at
the tails the optimal weight is not 0.5, and the weight should be swept.

**It was selected on P&L over the same window everything else was.** The sweep is
reported in full rather than as a winner, so the reader can see how flat or sharp
the optimum is - a peak that only exists at one weight is a fitted artifact, the
same trap as the rebalance phase.

Target transform is taken from the command line so this does not silently
re-litigate the ablation's conclusion.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import numpy as np
import polars as pl
import lightgbm as lgb
import torch
import torch.nn as nn

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
DATA = Path("/home/magnus/dev/magnus/ai-trader/data")
cfg = __import__("planner.config", fromlist=["Config"]).Config.load(
    Path("/home/magnus/dev/magnus/ai-trader/config/default.toml"))
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
ROUND_TRIP, K = 2 * COST / 10_000, 1.0
U = timezone.utc; LAG_H, HOLD_H = 1, 24
MIN_NAMES, MAX_NAMES, PART, NAV, Y_IMPACT = 6, 40, 0.05, 30_000, 0.3
TARGET = sys.argv[1] if len(sys.argv) > 1 else "rank"

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
idx = pl.read_parquet(S / "seqidx.parquet")
Xall = np.load(S / "seqX.npy")
ds = ds.join(idx, on=["date", "asset"], how="left").filter(pl.col("ok"))
XC = [c for c in ds.columns if c.startswith("x_")]
Xseq = torch.from_numpy(Xall[ds["row"].to_numpy()])
Xflat = torch.from_numpy(ds.select(XC).to_numpy().astype(np.float32))
y_raw = ds["t1"].to_numpy().astype(np.float32)
dates = ds["date"].to_numpy(); assets = ds["asset"].to_list()
uniq = sorted(set(dates.tolist()))
bounds, st = {}, 0
for i in range(1, len(dates) + 1):
    if i == len(dates) or dates[i] != dates[st]:
        bounds[dates[st]] = (st, i); st = i
y_rank = np.zeros_like(y_raw)
for d, (a, b) in bounds.items():
    v = y_raw[a:b]
    r = np.argsort(np.argsort(v)).astype(np.float32)
    y_rank[a:b] = 2.0 * r / max(1, len(v) - 1) - 1.0
yt = torch.from_numpy(y_rank if TARGET == "rank" else y_raw)
print(f"target={TARGET}  {len(ds):,} rows  {len(uniq):,} dates", flush=True)


class Ranker(nn.Module):
    def __init__(self, ns, nf, h=32):
        super().__init__()
        self.lstm = nn.LSTM(ns, h, batch_first=True)
        self.head = nn.Sequential(nn.Linear(h + nf, 64), nn.ReLU(),
                                  nn.Dropout(0.3), nn.Linear(64, 1))
    def forward(self, s, f):
        _, (hh, _) = self.lstm(s)
        return self.head(torch.cat([hh[-1], f], dim=1)).squeeze(-1)


def corr(s, t):
    s = s - s.mean(); t = t - t.mean()
    return -(s*t).sum() / (torch.sqrt((s*s).sum()*(t*t).sum()) + 1e-8)


EDGES = np.linspace(int(len(uniq)*0.4), len(uniq), 7).astype(int)
lstm_p = {}
for k in range(6):
    lo, hi = EDGES[k], EDGES[k+1]
    tr, te = uniq[:max(0, lo-1)], uniq[lo:hi]
    if len(tr) < 200: continue
    torch.manual_seed(0); np.random.seed(0)
    model = Ranker(Xseq.shape[2], Xflat.shape[1])
    opt = torch.optim.Adam(model.parameters(), lr=2e-3, weight_decay=1e-4)
    for _ in range(8):
        model.train()
        for j in np.random.permutation(len(tr)):
            a, b = bounds[tr[j]]
            if b - a < 10: continue
            opt.zero_grad()
            corr(model(Xseq[a:b], Xflat[a:b]), yt[a:b]).backward()
            nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step()
    model.eval()
    with torch.no_grad():
        for d in te:
            a, b = bounds[d]
            if b - a < 10: continue
            for i, v in zip(range(a, b), model(Xseq[a:b], Xflat[a:b]).numpy()):
                lstm_p[(dates[i], assets[i])] = float(v)
    print(f"  fold {k+1}/6", flush=True)
json.dump({"daily": {f"{d}|{a}": v for (d, a), v in lstm_p.items()}},
          open(S / f"preds_lstm_{TARGET}.json", "w"))

ds2 = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
dates2 = ds2["date"].unique().sort().to_list()
P = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
         min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
         bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
E2 = np.linspace(int(len(dates2)*0.4), len(dates2), 7).astype(int)
gbm_p = {}
for k in range(6):
    lo, hi = E2[k], E2[k+1]
    tr = ds2.filter(pl.col("date").is_in(dates2[:max(0, lo-1)]))
    te = ds2.filter(pl.col("date").is_in(dates2[lo:hi]))
    if tr.height < 3000 or te.height < 300: continue
    m = lgb.train(P, lgb.Dataset(tr.select(XC).to_numpy(), label=tr["t1"].to_numpy()),
                  num_boost_round=300)
    for d, a, v in zip(te["date"].to_list(), te["asset"].to_list(),
                       m.predict(te.select(XC).to_numpy())):
        gbm_p[(d, a)] = float(v)

VOLM = {(d, a): v for d, a, v in ds2.select(["date","asset","rv_24h"]).iter_rows()}
h = __import__("planner.store", fromlist=["read"]).read(root=DATA, interval_s=3600)
h = h.sort(["asset","ts_utc"]).with_columns(
    (pl.col("close")/pl.col("close").shift(1).over("asset")-1).alias("r1"))
h = h.with_columns(pl.col("r1").rolling_std(24, min_samples=12).over("asset").alias("sig"))
PX, VOL, SIG = {}, {}, {}
for a, t, o, qv, sg in h.select(["asset","ts_utc","open","quote_volume","sig"]).iter_rows():
    PX[(a,t)] = o; VOL[(a,t)] = qv; SIG[(a,t)] = sg
fund = pl.read_parquet(DATA/"funding"/"binance_um.parquet")
ftab = {(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k,v in fund.partition_by("asset", as_dict=True).items()}


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


def backtest(preds):
    by = {}
    for (d,a), v in preds.items():
        rv = VOLM.get((d,a))
        if rv: by.setdefault(d, []).append((a, v, rv))
    prev, rets = {}, []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U); entry = day+timedelta(hours=LAG_H)
        rows = sorted(by[d], key=lambda x: -abs(x[1]))
        cand = [r for r in rows if abs(r[1]) >= K*ROUND_TRIP][:MAX_NAMES] or rows[:MAX_NAMES]
        L = [(a, abs(p)/v) for a,p,v in cand if p > 0]
        Sh = [(a, abs(p)/v) for a,p,v in cand if p <= 0]
        if len(L) < 2 or len(Sh) < 2:
            t = sum(abs(x) for x in prev.values()); prev = {}
            if t: rets.append(-t*COST/10_000)
            continue
        caps = {a: max(min(MAXPOS, PART*(VOL.get((a,entry)) or 0.0)/NAV), 0.0)
                for a,_,_ in cand}
        w = waterfall(L, GROSS*0.5, caps)
        for a,x in waterfall(Sh, GROSS*0.5, caps).items(): w[a] = w.get(a,0.0)-x
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
        prev = w; rets.append(pnl+fp-c)
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rets:
        eq *= 1+v; pk = max(pk,eq); dd = min(dd, eq/pk-1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return len(rets), eq-1, (m*365)/(sd*math.sqrt(365)), dd


both = set(gbm_p) & set(lstm_p)
byd = {}
for (d,a) in both: byd.setdefault(d, []).append(a)
sc_g = statistics.pstdev([gbm_p[k] for k in both]) or 1.0

def blend(wt):
    out = {}
    for d, aa in byd.items():
        g = [gbm_p[(d,a)] for a in aa]; l = [lstm_p[(d,a)] for a in aa]
        mg, sg = statistics.mean(g), statistics.pstdev(g) or 1.0
        ml, sl = statistics.mean(l), statistics.pstdev(l) or 1.0
        for a, x, y in zip(aa, g, l):
            out[(d,a)] = ((1-wt)*(x-mg)/sg + wt*(y-ml)/sl) * sc_g
    return out

print(f"\n{'weight on LSTM':>16}{'n':>7}{'return':>11}{'Sharpe':>8}{'maxDD':>8}")
for wt in (0.0, 0.2, 0.35, 0.5, 0.65, 0.8, 1.0):
    r = backtest(blend(wt))
    tag = "  (pure GBM)" if wt == 0 else ("  (pure LSTM)" if wt == 1 else "")
    print(f"{wt:>16.2f}{r[0]:>7}{r[1]*100:>10.1f}%{r[2]:>8.2f}{r[3]*100:>7.1f}%{tag}", flush=True)