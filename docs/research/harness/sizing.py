"""How should capital be split across the leaderboard?

Three requirements, and they pull against each other:

  1. A name whose expected edge cannot pay its own round-trip cost should not be
     held at all - the capital does more good elsewhere.
  2. Capital should prefer higher expectancy, rather than being spread equally
     over everything that qualifies.
  3. It must stay diversified. Weighting by raw |prediction| satisfies (2) and
     destroys (3): predictions scale with volatility, so raw conviction
     concentrates into the most volatile names. That variant returned +1124% and
     drew down 51.5%.

The resolution to (2) versus (3) is to size by expectancy PER UNIT OF RISK rather
than by raw expectancy - |pred| / vol - which is the mean-variance form and
naturally declines to overweight a name merely for being volatile.

Diversification is reported, not assumed: EFFECTIVE N is 1 / sum(w_i^2) over the
normalised absolute weights. Twelve equally weighted names give 12; twelve names
where one holds most of the book give something near 1.

Entry threshold is expressed as a multiple of the round-trip cost, so it means
"only trade when the edge is worth k times what it costs to get in and out"
rather than an arbitrary number.
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
ROUND_TRIP = 2 * COST / 10_000
U = timezone.utc; LAG_H, HOLD_H = 1, 24
MIN_NAMES, MAX_NAMES = 6, 24

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)

preds, n = {}, len(dates)
edges = np.linspace(int(n * 0.4), n, 7).astype(int)
for k in range(6):
    lo, hi = edges[k], edges[k + 1]
    tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo - 1)]))
    te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
    if tr.height < 3000 or te.height < 300:
        continue
    m = lgb.train(PARAMS, lgb.Dataset(tr.select(XC).to_numpy(), label=tr["t1"].to_numpy()),
                  num_boost_round=300)
    for d, a, v, rv in zip(te["date"].to_list(), te["asset"].to_list(),
                           m.predict(te.select(XC).to_numpy()), te["rv_24h"].to_list()):
        preds[(d, a)] = (float(v), float(rv) if rv else None)

hourly = store.read(root=DATA, interval_s=3600).select(["asset", "ts_utc", "open"])
PX = {(a, t): o for a, t, o in hourly.iter_rows()}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def build(rows, sizing, k_cost):
    """rows: [(asset, pred, vol)]. Returns weights, dollar-neutral."""
    thr = k_cost * ROUND_TRIP
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
        if sizing == "equal":
            raw = {a: 1.0 for a, _, _ in items}
        elif sizing == "conviction":
            raw = {a: e for a, e, _ in items}
        elif sizing == "risk-adj":
            raw = {a: e / v for a, e, v in items}
        else:                                  # inverse-vol, expectancy ignored
            raw = {a: 1.0 / v for a, _, v in items}
        tot = sum(raw.values()) or 1.0
        return {a: x / tot for a, x in raw.items()}

    w = {}
    for a, x in side(L).items(): w[a] = GROSS * 0.5 * x
    for a, x in side(Sh).items(): w[a] = w.get(a, 0.0) - GROSS * 0.5 * x
    mx = max(abs(v) for v in w.values())
    if mx > MAXPOS:
        w = {a: v * MAXPOS / mx for a, v in w.items()}
    return w


def run(sizing, k_cost):
    by = {}
    for (d, a), (p, v) in preds.items():
        by.setdefault(d, []).append((a, p, v))
    prev, rets, counts, effn, mx = {}, [], [], [], []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        w = build(by[d], sizing, k_cost)
        if not w:
            # Nothing clears the bar: hold nothing, and pay to get out.
            turn = sum(abs(x) for x in prev.values()); prev = {}
            if turn: rets.append(-turn * COST / 10_000)
            counts.append(0)
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
        counts.append(len(w))
        g = sum(abs(x) for x in w.values()) or 1.0
        shares = [abs(x) / g for x in w.values()]
        effn.append(1.0 / sum(s * s for s in shares))
        mx.append(max(abs(x) for x in w.values()))
    if len(rets) < 100:
        return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rets:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return dict(n=len(rets), ret=eq - 1, sharpe=(m*365)/(sd*math.sqrt(365)), dd=dd,
                names=statistics.mean(counts), flat=sum(1 for c in counts if c == 0),
                effn=statistics.mean(effn) if effn else 0,
                maxw=statistics.mean(mx) if mx else 0)


print(f"round-trip cost {ROUND_TRIP*10000:.1f}bp; threshold = k x that\n")
print(f"{'sizing':<12}{'k':>5}{'return':>11}{'Sharpe':>8}{'maxDD':>8}"
      f"{'names':>7}{'eff N':>7}{'max w':>7}{'flat':>6}")
for sizing in ("equal", "conviction", "risk-adj", "inv-vol"):
    for k in (0, 1, 2, 4):
        r = run(sizing, k)
        if r:
            print(f"{sizing:<12}{k:>5}{r['ret']*100:>10.1f}%{r['sharpe']:>8.2f}"
                  f"{r['dd']*100:>7.1f}%{r['names']:>7.1f}{r['effn']:>7.1f}"
                  f"{r['maxw']:>7.3f}{r['flat']:>6}")
print("\nBTC over the same span: +219.3%, Sharpe 0.88, maxDD -53.1%")
print("eff N = 1/sum(w^2): the number of equally weighted names the book behaves like.")