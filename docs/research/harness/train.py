"""Train two rankers - one per cadence - and compare them out of sample.

The comparison the whole exercise is for: a daily book turns over 3.1x as much
and pays roughly 7%/yr in fees against the weekly book's 2.3%. Earlier that
killed daily, but only for the CHANNEL signal, and even then daily had more gross
alpha (1200.8% against 918.8% at zero cost). If a learned ranker predicts
day-to-day ordering better than the channel does, the extra alpha may clear the
extra cost. That is an empirical question and this answers it.

**Purging and embargo are what make it honest.** A 7-day target observed on day t
overlaps every observation from t-6 to t+6. Training on a window that touches the
test window leaks the answer, and a model that has seen the answer scores
beautifully and predicts nothing. Every fold therefore drops `horizon` days of
data on both sides of its test block. Without this the results below are
meaningless, and they will look BETTER without it, which is how the mistake
survives.

Expanding-window folds, never trained on data after the test block, because a
model that saw 2026 while predicting 2021 is not a forecast.
"""
import json, sys
from pathlib import Path
import numpy as np
import polars as pl
import lightgbm as lgb

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
df = pl.read_parquet(SCRATCH / "ds.parquet").sort(["date", "asset"])
XCOLS = [c for c in df.columns if c.startswith("x_")]
dates = df["date"].unique().sort().to_list()
print(f"{df.height:,} rows  {len(dates):,} dates  {len(XCOLS)} features\n")

PARAMS = dict(
    objective="regression", metric="l2", learning_rate=0.03,
    num_leaves=15, min_data_in_leaf=200, feature_fraction=0.7,
    bagging_fraction=0.7, bagging_freq=1, lambda_l2=20.0,
    verbose=-1, num_threads=4,
)
N_FOLDS = 6


def spearman(a, b):
    ra = np.argsort(np.argsort(a)); rb = np.argsort(np.argsort(b))
    ra = ra - ra.mean(); rb = rb - rb.mean()
    d = np.sqrt((ra ** 2).sum() * (rb ** 2).sum())
    return float((ra * rb).sum() / d) if d else 0.0


def run(target, horizon, label):
    """Expanding-window CV with a `horizon`-day purge either side of each test block."""
    preds = {}
    fold_ic, fold_rows = [], []
    n = len(dates)
    first_test = int(n * 0.4)
    bounds = np.linspace(first_test, n, N_FOLDS + 1).astype(int)

    for k in range(N_FOLDS):
        lo, hi = bounds[k], bounds[k + 1]
        test_dates = set(dates[lo:hi])
        # Purge: nothing within `horizon` days of the test block may train.
        train_upto = max(0, lo - horizon)
        train_dates = set(dates[:train_upto])
        tr = df.filter(pl.col("date").is_in(list(train_dates)))
        te = df.filter(pl.col("date").is_in(list(test_dates)))
        if tr.height < 3000 or te.height < 300:
            continue
        model = lgb.train(
            PARAMS,
            lgb.Dataset(tr.select(XCOLS).to_numpy(), label=tr[target].to_numpy()),
            num_boost_round=300,
        )
        p = model.predict(te.select(XCOLS).to_numpy())
        for d, a, v in zip(te["date"].to_list(), te["asset"].to_list(), p):
            preds[(d, a)] = float(v)
        # Per-date IC of the prediction against the realised relative return.
        per = {}
        for d, y, v in zip(te["date"].to_list(), te[target].to_numpy(), p):
            per.setdefault(d, ([], []))
            per[d][0].append(v); per[d][1].append(y)
        ics = [spearman(np.array(x), np.array(y)) for x, y in per.values() if len(x) >= 10]
        fold_ic.append(float(np.mean(ics)))
        fold_rows.append(len(ics))
        print(f"  {label} fold {k+1}: train {tr.height:>6,} rows to {dates[train_upto-1]}, "
              f"test {te.height:>5,} rows {dates[lo]}..{dates[hi-1]}, IC {np.mean(ics):+.4f}")

    allic = np.array(fold_ic)
    tot = sum(fold_rows)
    t = allic.mean() / (allic.std(ddof=1) / np.sqrt(len(allic))) if len(allic) > 1 else 0.0
    print(f"  {label}: mean OOS IC {allic.mean():+.4f} across {len(allic)} folds "
          f"({tot:,} test dates), fold-level t {t:+.2f}\n")
    return preds, allic.mean()


print("=== WEEKLY model: 7-day target, 7-day purge ===")
p7, ic7 = run("t7", 7, "weekly")
print("=== DAILY model: 1-day target, 1-day purge ===")
p1, ic1 = run("t1", 1, "daily")

json.dump({"weekly": {f"{d}|{a}": v for (d, a), v in p7.items()},
           "daily": {f"{d}|{a}": v for (d, a), v in p1.items()},
           "ic": {"weekly": ic7, "daily": ic1}},
          open(SCRATCH / "preds.json", "w"))
print(f"wrote preds.json  weekly IC {ic7:+.4f}  daily IC {ic1:+.4f}")