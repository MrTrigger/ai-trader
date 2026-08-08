"""Does the LSTM's higher IC become higher P&L?

IC and return are not the same thing. A model can rank the middle of the
cross-section better - lifting IC - while being no better at the extremes, which
is all the book trades. Both models therefore go through the identical
construction: risk-adjusted sizing, a threshold at one round-trip cost, the
liquidity cap and waterfall, a one-hour fill lag, and measured costs.

Also blends them. Two models with genuinely different errors should combine to
beat either, and if the blend adds nothing then they are seeing the same thing
by different means.
"""
import json, math, statistics
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
MIN_NAMES, MAX_NAMES, PART = 6, 40, 0.05
NAV, Y_IMPACT = 30_000, 0.3

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
VOLMAP = {(d, a): v for d, a, v in ds.select(["date", "asset", "rv_24h"]).iter_rows()}

# GBM predictions on the same folds.
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
EDGES = np.linspace(int(len(dates) * 0.4), len(dates), 7).astype(int)
gbm = {}
for k in range(6):
    lo, hi = EDGES[k], EDGES[k + 1]
    tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo - 1)]))
    te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
    if tr.height < 3000 or te.height < 300: continue
    m = lgb.train(PARAMS, lgb.Dataset(tr.select(XC).to_numpy(), label=tr["t1"].to_numpy()),
                  num_boost_round=300)
    for d, a, v in zip(te["date"].to_list(), te["asset"].to_list(),
                       m.predict(te.select(XC).to_numpy())):
        gbm[(d, a)] = float(v)

lstm = {tuple(k.split("|")): v for k, v in json.load(open(S / "preds_lstm.json"))["daily"].items()}
print(f"GBM {len(gbm):,} preds   LSTM {len(lstm):,} preds   "
      f"overlap {len(set(gbm) & set(lstm)):,}", flush=True)

h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
h = h.with_columns((pl.col("close")/pl.col("close").shift(1).over("asset")-1).alias("r1"))
h = h.with_columns(pl.col("r1").rolling_std(24, min_samples=12).over("asset").alias("sig"))
PX, VOL, SIG = {}, {}, {}
for a, t, o, qv, sg in h.select(["asset","ts_utc","open","quote_volume","sig"]).iter_rows():
    PX[(a,t)] = o; VOL[(a,t)] = qv; SIG[(a,t)] = sg
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def zscore(vals):
    m = statistics.mean(vals); sd = statistics.pstdev(vals) or 1.0
    return [(v - m) / sd for v in vals]


def waterfall(items, budget, caps):
    w, remaining = {}, budget
    for _ in range(12):
        open_ = [(a, s) for a, s in items if caps.get(a, 0) - w.get(a, 0.0) > 1e-9]
        if not open_ or remaining <= 1e-9: break
        tot = sum(s for _, s in open_) or 1.0
        moved = False
        for a, s in open_:
            take = min(remaining * s / tot, caps[a] - w.get(a, 0.0))
            if take > 1e-12:
                w[a] = w.get(a, 0.0) + take; moved = True
        remaining = budget - sum(w.values())
        if not moved: break
    return w


def run(preds):
    by = {}
    for (d, a), v in preds.items():
        rv = VOLMAP.get((d, a))
        if rv: by.setdefault(d, []).append((a, v, rv))
    prev, rets = {}, []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        rows = sorted(by[d], key=lambda x: -abs(x[1]))
        cand = [r for r in rows if abs(r[1]) >= K * ROUND_TRIP][:MAX_NAMES] or rows[:MAX_NAMES]
        L = [(a, abs(p)/v) for a, p, v in cand if p > 0]
        Sh = [(a, abs(p)/v) for a, p, v in cand if p <= 0]
        if len(L) < 2 or len(Sh) < 2:
            turn = sum(abs(x) for x in prev.values()); prev = {}
            if turn: rets.append(-turn * COST / 10_000)
            continue
        caps = {}
        for a, _, _ in cand:
            vh = VOL.get((a, entry)) or 0.0
            caps[a] = max(min(MAXPOS, PART * vh / NAV), 0.0)
        w = waterfall(L, GROSS*0.5, caps)
        for a, x in waterfall(Sh, GROSS*0.5, caps).items():
            w[a] = w.get(a, 0.0) - x
        w = {a: x for a, x in w.items() if abs(x) > 1e-6}
        if not w: continue
        got = {}
        for a in w:
            p0 = PX.get((a, entry)); p1 = PX.get((a, entry + timedelta(hours=HOLD_H)))
            if p0 and p1: got[a] = p1/p0 - 1
        if len(got) < max(4, len(w)*0.6): continue
        pnl = sum(w[a]*r for a, r in got.items())
        fp = sum(-v * ftab.get(a, {}).get(day.date(), 0.0) for a, v in w.items())
        cost = 0.0
        for a in set(w) | set(prev):
            dw = abs(w.get(a, 0.0) - prev.get(a, 0.0))
            if dw <= 0: continue
            cost += dw * COST / 10_000
            vh = VOL.get((a, entry)) or 0.0
            if vh > 0:
                cost += dw * Y_IMPACT * (SIG.get((a, entry)) or 0.02) * math.sqrt(dw*NAV/vh)
        prev = w
        rets.append(pnl + fp - cost)
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rets:
        eq *= 1+v; pk = max(pk, eq); dd = min(dd, eq/pk-1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return len(rets), eq-1, (m*365)/(sd*math.sqrt(365)), dd, m/(sd/math.sqrt(len(rets)))


# Blend: z-score each model within the date so neither dominates by scale.
both = set(gbm) & set(lstm)
byd = {}
for (d, a) in both:
    byd.setdefault(d, []).append(a)
blend = {}
for d, assets in byd.items():
    zg = zscore([gbm[(d, a)] for a in assets])
    zl = zscore([lstm[(d, a)] for a in assets])
    for a, g, l in zip(assets, zg, zl):
        blend[(d, a)] = 0.5*g + 0.5*l
# Put the blend back on the GBM's scale so the cost threshold means the same thing.
sc = statistics.pstdev([gbm[k] for k in both]) or 1.0
blend = {k: v*sc for k, v in blend.items()}

print(f"\nsame construction for all three: risk-adjusted sizing, {K:g}x round-trip")
print(f"threshold, {PART:.0%} liquidity cap with waterfall, {LAG_H}h fill lag, NAV ${NAV:,}\n")
print(f"{'model':<24}{'n':>6}{'return':>11}{'Sharpe':>8}{'maxDD':>8}{'t':>7}")
for label, p in (("GBM", {k: gbm[k] for k in both}),
                 ("LSTM", {k: lstm[k] for k in both}),
                 ("blend 50/50", blend)):
    r = run(p)
    print(f"{label:<24}{r[0]:>6}{r[1]*100:>10.1f}%{r[2]:>8.2f}{r[3]*100:>7.1f}%{r[4]:>7.2f}")

# How different are they really?
rho = []
for d, assets in byd.items():
    if len(assets) < 10: continue
    zg = zscore([gbm[(d, a)] for a in assets]); zl = zscore([lstm[(d, a)] for a in assets])
    n = len(zg)
    rho.append(sum(x*y for x, y in zip(zg, zl))/n)
print(f"\nmean within-date correlation between the two models: {statistics.mean(rho):+.3f}")
print("Low correlation means they make different errors and a blend should help.")