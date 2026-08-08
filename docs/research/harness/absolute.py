"""Should the long/short split float, driven by an absolute-return forecast?

The book is pinned at six long and six short because the score predicts RELATIVE
return - forward return minus the day's cross-sectional mean - so its sign says
"beats the average", not "goes up". Taking the top thirty of that leaderboard is
a long-only book of the least-bad assets, which is the failure already diagnosed
for the channel: a relative signal expressed as an absolute book.

For a floating split the model has to forecast the ABSOLUTE return, which is a
strictly harder problem because it contains the market's own direction. Whether
that is worth it is an empirical question, so both targets are trained on the
same features, same folds, same purge:

  relative   y - mean(y) per date.  The current model.
  absolute   y.  Sign is meaningful, so the book can lean.

and four portfolio rules are compared on the resulting scores:

  6L/6S fixed       the current book
  topN by |score|   the proposal: strongest convictions, direction from the sign
  sign-split        long everything predicted up, short everything predicted
                    down, however lopsided that turns out
  sign-split capped the same, but never more than 12 names
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

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
# `y` is the raw 1h-lagged 24h return; `t1` is it demeaned per date.
ds = ds.with_columns(pl.col("y").alias("t_abs"))
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


def book(rule, ranked):
    """rule -> {asset: weight}. `ranked` is [(asset, score)] high to low."""
    if rule == "6L/6S fixed":
        if len(ranked) < 12: return {}
        L = [a for a, _ in ranked[:6]]; Sh = [a for a, _ in ranked[-6:]]
    elif rule == "top12 by |score|":
        top = sorted(ranked, key=lambda x: -abs(x[1]))[:12]
        L = [a for a, v in top if v > 0]; Sh = [a for a, v in top if v <= 0]
    elif rule == "sign-split":
        L = [a for a, v in ranked if v > 0]; Sh = [a for a, v in ranked if v <= 0]
    else:  # sign-split, capped at 12 by conviction
        top = sorted(ranked, key=lambda x: -abs(x[1]))[:12]
        L = [a for a, v in top if v > 0]; Sh = [a for a, v in top if v <= 0]
    if not L and not Sh:
        return {}
    w = {}
    # Gross split in proportion to how many names each side has, so the book
    # leans when the forecast leans instead of being forced to 50/50.
    tot = len(L) + len(Sh)
    gl = GROSS * (len(L) / tot) if tot else 0
    gs = GROSS * (len(Sh) / tot) if tot else 0
    for a in L: w[a] = gl / len(L)
    for a in Sh: w[a] = w.get(a, 0.0) - gs / len(Sh)
    mx = max((abs(v) for v in w.values()), default=0)
    if mx > MAXPOS:
        w = {a: v * MAXPOS / mx for a, v in w.items()}
    return w


def backtest(preds, rule):
    by = {}
    for (d, a), v in preds.items():
        by.setdefault(d, []).append((a, v))
    prev, rows, nets = {}, [], []
    for d in sorted(by):
        day = datetime.fromisoformat(d).replace(tzinfo=U)
        entry = day + timedelta(hours=LAG_H)
        ranked = sorted(by[d], key=lambda x: -x[1])
        w = book(rule, ranked)
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
        prev = w
        nets.append(sum(w.values()))
        rows.append(pnl + fp - turn * COST / 10_000)
    if len(rows) < 50:
        return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for v in rows:
        eq *= 1 + v; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rows), statistics.stdev(rows)
    return (len(rows), eq - 1, (m*365)/(sd*math.sqrt(365)), dd,
            statistics.mean(nets), statistics.mean(abs(x) for x in nets))


print("training both targets on identical folds...", flush=True)
P = {"relative": walk("t1"), "absolute": walk("t_abs")}

print(f"\n{'target':<10}{'rule':<20}{'n':>6}{'return':>11}{'Sharpe':>8}{'maxDD':>8}"
      f"{'mean net':>10}{'|net|':>8}")
for tgt, preds in P.items():
    for rule in ("6L/6S fixed", "top12 by |score|", "sign-split", "sign-split capped"):
        r = backtest(preds, rule)
        if r:
            print(f"{tgt:<10}{rule:<20}{r[0]:>6}{r[1]*100:>10.1f}%{r[2]:>8.2f}"
                  f"{r[3]*100:>7.1f}%{r[4]:>+10.3f}{r[5]:>8.3f}")
print("\nBTC over the same span: +219.3%, Sharpe 0.88, maxDD -53.1%")