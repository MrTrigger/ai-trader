"""Market impact, and therefore capacity.

The book replaces ~98% of itself daily and impact has been modelled as zero,
which is the largest unmeasured exposure in the result. Impact cannot be
measured without real fills, but it can be MODELLED, and the useful output is
not a single number - it is the account size at which the edge dies.

The square-root law is the standard form and is what config already assumes:

    impact_bps = Y * sigma * sqrt(Q / V)

Q is the order, V the volume available over the execution window, sigma the
volatility over that window. Doubling size costs less than twice as much,
because the order walks a book that deepens.

Two choices make this honest rather than flattering:

**The window is the hour we actually trade in, not the day.** The book fills one
hour after the signal, so the relevant V is that hour's volume and the relevant
sigma is hourly. Using daily ADV would divide by 24x more liquidity than is
really there and understate impact by roughly 5x.

**Y is swept, not assumed.** Config's 0.10 is on the optimistic edge of the
equity literature, where 0.3-1.0 is typical, and crypto alts are unlikely to be
kinder. Reporting one Y would be picking an answer.

Also reported: participation rate. An order that is a large fraction of an
hour's volume does not merely cost more, it may not fill at all, and a model that
returns a finite cost for an impossible trade is lying quietly.
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
MIN_NAMES, MAX_NAMES = 6, 24

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
print(f"{len(preds):,} out-of-sample predictions", flush=True)

# Hourly volume and volatility AT the execution hour - not a daily average.
h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
h = h.with_columns([
    (pl.col("close") / pl.col("close").shift(1).over("asset") - 1).alias("r1"),
])
h = h.with_columns(
    pl.col("r1").rolling_std(24, min_samples=12).over("asset").alias("sig_h"))
PX, VOL, SIG = {}, {}, {}
for a, t, o, qv, sg in h.select(["asset", "ts_utc", "open", "quote_volume", "sig_h"]).iter_rows():
    PX[(a, t)] = o; VOL[(a, t)] = qv; SIG[(a, t)] = sg
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def build(rows):
    cand = [(a, p, v) for a, p, v in rows if abs(p) >= K * ROUND_TRIP and v]
    if len(cand) < MIN_NAMES: return {}
    cand.sort(key=lambda x: -abs(x[1])); cand = cand[:MAX_NAMES]
    L = [(a, abs(p), v) for a, p, v in cand if p > 0]
    Sh = [(a, abs(p), v) for a, p, v in cand if p <= 0]
    if len(L) < 2 or len(Sh) < 2: return {}
    def side(it):
        raw = {a: e / v for a, e, v in it}; t = sum(raw.values()) or 1.0
        return {a: x / t for a, x in raw.items()}
    w = {}
    for a, x in side(L).items(): w[a] = GROSS * 0.5 * x
    for a, x in side(Sh).items(): w[a] = w.get(a, 0.0) - GROSS * 0.5 * x
    mx = max(abs(v) for v in w.values())
    return {a: v * MAXPOS / mx for a, v in w.items()} if mx > MAXPOS else w


by = {}
for (d, a), (p, v) in preds.items():
    by.setdefault(d, []).append((a, p, v))
DAYS = sorted(by)


def run(nav, Y):
    """One pass at a given account size and impact coefficient."""
    prev, rets, parts, imp_bps = {}, [], [], []
    for d in DAYS:
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

        spread_cost = impact_cost = 0.0
        for a in set(w) | set(prev):
            dw = abs(w.get(a, 0.0) - prev.get(a, 0.0))
            if dw <= 0:
                continue
            spread_cost += dw * COST / 10_000
            q = dw * nav                                  # order notional
            v_h = VOL.get((a, entry)) or 0.0              # volume that hour
            sig = SIG.get((a, entry)) or 0.02
            if v_h <= 0:
                continue
            p = q / v_h
            parts.append(p)
            bps = Y * sig * math.sqrt(p) * 10_000
            imp_bps.append(bps)
            impact_cost += dw * bps / 10_000
        prev = w
        rets.append(pnl + fp - spread_cost - impact_cost)
    if len(rets) < 100:
        return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rets:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return dict(ret=eq-1, sharpe=(m*365)/(sd*math.sqrt(365)), dd=dd,
                part=statistics.median(parts) if parts else 0,
                part99=sorted(parts)[int(.99*len(parts))] if parts else 0,
                imp=statistics.mean(imp_bps) if imp_bps else 0)


print(f"\nimpact = Y * sigma_1h * sqrt(order / volume in the execution hour)")
print(f"config's Y is {cfg.costs.impact_coefficient}; the equity literature is 0.3-1.0\n")
print(f"{'NAV':>12}{'Y':>6}{'return':>11}{'Sharpe':>8}{'maxDD':>8}"
      f"{'med part':>10}{'99th part':>11}{'mean imp':>10}")
for nav in (30_000, 300_000, 3_000_000, 30_000_000):
    for Y in (0.1, 0.3, 1.0):
        r = run(nav, Y)
        if r:
            print(f"${nav:>11,}{Y:>6.1f}{r['ret']*100:>10.1f}%{r['sharpe']:>8.2f}"
                  f"{r['dd']*100:>7.1f}%{r['part']*100:>9.2f}%{r['part99']*100:>10.2f}%"
                  f"{r['imp']:>9.1f}b")
    print()
print("part = order as a share of that hour's volume. Above ~10% an order stops")
print("being a price-taker and the sqrt model understates what it would cost.")