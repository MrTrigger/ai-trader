"""Capacity-aware sizing: cap each name by what it can absorb, spill the rest.

The impact model showed the book stops being feasible above ~$300k because the
tail orders exceed the volume available in the hour they trade. That is a sizing
failure, not a signal failure: the model asks for a weight the asset cannot
sustain, and the current construction has no way to say no.

The fix is a waterfall. Each name gets a cap that is the LOWER of the risk limit
and what its own liquidity supports:

    cap_i = min(max_position, participation_limit * hourly_volume_i / NAV)

Desired weights are assigned up to that cap; whatever cannot be placed spills to
the names that still have headroom, in ranking order. When every name currently
held is capped, the book **extends further down the ranking** rather than leaving
capital idle - so a $30k book might hold 22 names and a $3M book 40, holding the
same total gross in smaller, more numerous positions.

Two honest consequences, both reported rather than hidden:

  the marginal names are worse. Extending down the ranking buys capacity with
  signal quality, and the return should decline even where feasibility holds.

  sometimes gross cannot be deployed at all. If the whole candidate list is
  capped out, the book is under-invested and says so, rather than pretending to
  a position it could not fill.
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
ROUND_TRIP, K = 2 * COST / 10_000, 1.0
U = timezone.utc; LAG_H, HOLD_H = 1, 24
MIN_NAMES = 6
Y_IMPACT = 0.3          # mid-range of the literature

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
EDGES = np.linspace(int(len(dates) * 0.4), len(dates), 7).astype(int)

preds = {}
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
print(f"{len(preds):,} predictions", flush=True)

h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
h = h.with_columns((pl.col("close")/pl.col("close").shift(1).over("asset")-1).alias("r1"))
h = h.with_columns(pl.col("r1").rolling_std(24, min_samples=12).over("asset").alias("sig"))
PX, VOL, SIG = {}, {}, {}
for a, t, o, qv, sg in h.select(["asset","ts_utc","open","quote_volume","sig"]).iter_rows():
    PX[(a,t)] = o; VOL[(a,t)] = qv; SIG[(a,t)] = sg
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}

by = {}
for (d, a), (p, v) in preds.items():
    by.setdefault(d, []).append((a, p, v))
DAYS = sorted(by)


def waterfall(items, budget, caps):
    """Assign `budget` of gross across `items` (ordered, with a raw appetite),
    respecting per-name caps and spilling the remainder down the list."""
    w = {}
    remaining = budget
    pool = list(items)
    for _ in range(12):                       # converges quickly; bounded anyway
        open_ = [(a, s) for a, s in pool if caps.get(a, 0) - w.get(a, 0.0) > 1e-9]
        if not open_ or remaining <= 1e-9:
            break
        tot = sum(s for _, s in open_) or 1.0
        placed_any = False
        for a, s in open_:
            want = remaining * s / tot
            room = caps[a] - w.get(a, 0.0)
            take = min(want, room)
            if take > 1e-12:
                w[a] = w.get(a, 0.0) + take
                placed_any = True
        used = sum(w.values())
        remaining = budget - used
        if not placed_any:
            break
    return w, max(0.0, remaining)


def build(rows, nav, participation, entry, max_names, liquidity_aware):
    cand = [(a, p, v) for a, p, v in rows if abs(p) >= K * ROUND_TRIP and v]
    if len(cand) < MIN_NAMES:
        return {}, 0.0
    cand.sort(key=lambda x: -abs(x[1]))
    cand = cand[:max_names]
    L = [(a, abs(p)/v) for a, p, v in cand if p > 0]
    Sh = [(a, abs(p)/v) for a, p, v in cand if p <= 0]
    if len(L) < 2 or len(Sh) < 2:
        return {}, 0.0
    caps = {}
    for a, _, _ in cand:
        cap = MAXPOS
        if liquidity_aware:
            vh = VOL.get((a, entry)) or 0.0
            cap = min(cap, participation * vh / nav) if nav > 0 else cap
        caps[a] = max(cap, 0.0)
    wl, unl = waterfall(L, GROSS * 0.5, caps)
    ws, uns = waterfall(Sh, GROSS * 0.5, caps)
    w = dict(wl)
    for a, x in ws.items():
        w[a] = w.get(a, 0.0) - x
    return w, unl + uns


def run(nav, participation, max_names, liquidity_aware):
    prev, rets, names, parts, undep = {}, [], [], [], []
    for d in DAYS:
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        w, unplaced = build(by[d], nav, participation, entry, max_names, liquidity_aware)
        w = {a: x for a, x in w.items() if abs(x) > 1e-6}
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
        cost = 0.0
        for a in set(w) | set(prev):
            dw = abs(w.get(a, 0.0) - prev.get(a, 0.0))
            if dw <= 0: continue
            cost += dw * COST / 10_000
            vh = VOL.get((a, entry)) or 0.0
            if vh > 0:
                p = dw * nav / vh
                parts.append(p)
                cost += dw * Y_IMPACT * (SIG.get((a, entry)) or 0.02) * math.sqrt(p)
        prev = w
        rets.append(pnl + fp - cost)
        names.append(len(w)); undep.append(unplaced)
    if len(rets) < 100:
        return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rets:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq/pk-1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return dict(ret=eq-1, sharpe=(m*365)/(sd*math.sqrt(365)), dd=dd,
                names=statistics.mean(names),
                p99=sorted(parts)[int(.99*len(parts))] if parts else 0,
                undep=statistics.mean(undep))


print(f"\nimpact Y={Y_IMPACT}; liquidity cap = {{participation}} of the execution hour's volume\n")
print(f"{'NAV':>12}  {'sizing':<24}{'names':>7}{'99th part':>11}{'undeployed':>12}"
      f"{'return':>11}{'Sharpe':>8}{'maxDD':>8}")
for nav in (30_000, 300_000, 3_000_000, 10_000_000):
    for label, aware, part, mx in (
        ("current (cap 25% NAV)", False, 0.0, 24),
        ("liquidity, 5%, <=40", True, 0.05, 40),
        ("liquidity, 5%, <=80", True, 0.05, 80),
        ("liquidity, 2%, <=80", True, 0.02, 80),
    ):
        r = run(nav, part, mx, aware)
        if r:
            print(f"${nav:>11,}  {label:<24}{r['names']:>7.1f}{r['p99']*100:>10.2f}%"
                  f"{r['undep']*100:>11.1f}%{r['ret']*100:>10.1f}%{r['sharpe']:>8.2f}"
                  f"{r['dd']*100:>7.1f}%")
    print()