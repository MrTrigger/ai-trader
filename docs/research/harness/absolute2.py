"""Rank by expected trade return, but separate the RANKING from the EXPOSURE.

The list is built as described: for each asset the better of going long
(expected = +pred) or short (expected = -pred), so its expected return is |pred|
and its direction is sign(pred). Rank on that, take the top N, and one row can
be long while the next is short.

The first attempt let the resulting book lean wherever the signs happened to
fall, and it produced a -53.7% drawdown against BTC's -53.1% - the book had
become a market call. That may be a fault of the EXPOSURE rather than of the
RANKING, and the two are separable:

  lean       gross split by how many names fall on each side. What was tested.
  neutral    same names, same directions, but long and short gross forced equal.
  conviction same names, dollar-neutral, weights within each side proportional
             to |pred| so the strongest expectations get the most capital.

If the ranking is good and only the exposure was wrong, `neutral` recovers. If
`neutral` also fails, the ranking itself is the problem.

Also measures how correlated the predictions are across assets on a given day.
A relative model predicts who beats whom; an absolute model predicts the market
plus who beats whom, and if the market term dominates then every prediction
moves together and |pred| ranks by beta rather than by opportunity.
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
U = timezone.utc; LAG_H, HOLD_H = 1, 24

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"]).with_columns(
    pl.col("y").alias("t_abs"))
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)


def walk(target):
    preds, n = {}, len(dates)
    edges = np.linspace(int(n * 0.4), n, 7).astype(int)
    for k in range(6):
        lo, hi = edges[k], edges[k + 1]
        tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo - 1)]))
        te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
        if tr.height < 3000 or te.height < 300:
            continue
        m = lgb.train(PARAMS, lgb.Dataset(tr.select(XC).to_numpy(),
                                          label=tr[target].to_numpy()), num_boost_round=300)
        for d, a, v in zip(te["date"].to_list(), te["asset"].to_list(),
                           m.predict(te.select(XC).to_numpy())):
            preds[(d, a)] = float(v)
    return preds


hourly = store.read(root=DATA, interval_s=3600).select(["asset", "ts_utc", "open"])
PX = {(a, t): o for a, t, o in hourly.iter_rows()}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def book(mode, ranked, n_take=12):
    """`ranked` is [(asset, predicted_return)]. Direction is the sign."""
    top = sorted(ranked, key=lambda x: -abs(x[1]))[:n_take]
    L = [(a, abs(v)) for a, v in top if v > 0]
    Sh = [(a, abs(v)) for a, v in top if v <= 0]
    if not L or not Sh:
        return {}
    w = {}
    if mode == "lean":
        tot = len(L) + len(Sh)
        gl, gs = GROSS * len(L) / tot, GROSS * len(Sh) / tot
        for a, _ in L: w[a] = gl / len(L)
        for a, _ in Sh: w[a] = w.get(a, 0.0) - gs / len(Sh)
    elif mode == "neutral":
        for a, _ in L: w[a] = GROSS * 0.5 / len(L)
        for a, _ in Sh: w[a] = w.get(a, 0.0) - GROSS * 0.5 / len(Sh)
    else:  # conviction-weighted within each side, still dollar-neutral
        sl = sum(v for _, v in L) or 1.0
        ss = sum(v for _, v in Sh) or 1.0
        for a, v in L: w[a] = GROSS * 0.5 * v / sl
        for a, v in Sh: w[a] = w.get(a, 0.0) - GROSS * 0.5 * v / ss
    mx = max((abs(x) for x in w.values()), default=0)
    if mx > MAXPOS:
        w = {a: x * MAXPOS / mx for a, x in w.items()}
    return w


def backtest(preds, mode, n_take=12):
    by = {}
    for (d, a), v in preds.items():
        by.setdefault(d, []).append((a, v))
    prev, rows, nets = {}, [], []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        w = book(mode, by[d], n_take)
        if not w:
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
        prev = w; nets.append(sum(w.values()))
        rows.append(pnl + fp - turn * COST / 10_000)
    if len(rows) < 50:
        return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rows:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rows), statistics.stdev(rows)
    return len(rows), eq - 1, (m*365)/(sd*math.sqrt(365)), dd, statistics.mean(nets)


print("training...", flush=True)
P = {"absolute": walk("t_abs"), "relative": walk("t1")}

print(f"\n{'target':<10}{'exposure':<12}{'take':>6}{'n':>6}{'return':>11}{'Sharpe':>8}"
      f"{'maxDD':>8}{'mean net':>10}")
for tgt in ("absolute", "relative"):
    for mode in ("lean", "neutral", "conviction"):
        for take in (12, 20):
            r = backtest(P[tgt], mode, take)
            if r:
                print(f"{tgt:<10}{mode:<12}{take:>6}{r[0]:>6}{r[1]*100:>10.1f}%"
                      f"{r[2]:>8.2f}{r[3]*100:>7.1f}%{r[4]:>+10.3f}")

# Why the absolute target behaves as it does.
print("\nhow much do the predictions move together on a given day?")
for tgt in ("absolute", "relative"):
    by = {}
    for (d, a), v in P[tgt].items():
        by.setdefault(d, []).append(v)
    means = [statistics.mean(v) for v in by.values() if len(v) >= 10]
    within = [statistics.pstdev(v) for v in by.values() if len(v) >= 10]
    print(f"  {tgt:<9} sd of the DAILY MEAN prediction {statistics.pstdev(means)*100:.3f}%"
          f"   median WITHIN-day sd {statistics.median(within)*100:.3f}%")
print("  A large daily-mean sd relative to the within-day sd means the model is")
print("  mostly forecasting the market, so |pred| ranks by beta, not opportunity.")
print("\nBTC over the same span: +219.3%, Sharpe 0.88, maxDD -53.1%")