"""Which reward should the booster be fit on?

A: demean(ret)        - the documented target, what the pipeline ships today.
B: demean(ret / vol)  - the risk-adjusted reward remembered as the late change.
                        Never committed, so it is re-measured, not trusted.

Both are fit per expanding fold (same boundaries and 2-day purge as
bin/walk-forward.sh) and judged out-of-sample on the SAME yardsticks:
  - rank IC against demeaned return (comparable to the record's 0.0511/0.0643)
  - top-minus-bottom decile spread of demeaned return (what a book extracts)
  - the same spread with 1/vol weights (what the risk-adjusted BOOK extracts)

NB if B wins and gets ported: its score is already per-unit-risk, so the
construction must stop dividing by vol or it divides twice.
"""
import json, sys
import numpy as np, lightgbm as lgb
from datetime import date, timedelta
from collections import defaultdict
from scipy.stats import spearmanr

MATRIX = "data/models/training.jsonl"
START, END, FOLDS = date(2022, 9, 18), date(2026, 7, 30), 6
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1)

with open(MATRIX) as f:
    man = json.loads(next(f))
    rows = [json.loads(l) for l in f if l.strip()]
feats = man["features"]
print(f"{len(rows):,} rows, {len(feats)} features")

for r in rows:
    r["d"] = date.fromisoformat(r["date"])
# risk-adjusted reward, demeaned within date over the rows that have vol
byday = defaultdict(list)
for r in rows:
    byday[r["d"]].append(r)
for d, rs in byday.items():
    withvol = [r for r in rs if r.get("vol")]
    if withvol:
        m = np.mean([r["raw_ret"] / r["vol"] for r in withvol])
        for r in withvol:
            r["target_ra"] = r["raw_ret"] / r["vol"] - m

span = (END - START).days
block = span // FOLDS
X = lambda rs: np.asarray([[r["features"][n] for n in feats] for r in rs])

def fit_predict(train, test, key):
    tr = [r for r in train if key in r]
    b = lgb.train(PARAMS, lgb.Dataset(X(tr), label=np.asarray([r[key] for r in tr])), 300)
    return b.predict(X(test))

def judge(test, pred):
    ics, spreads, rspreads = [], [], []
    per = defaultdict(list)
    for r, p in zip(test, pred):
        per[r["d"]].append((p, r["target"], r.get("vol")))
    for d, xs in per.items():
        if len(xs) < 10: continue
        p = np.array([x[0] for x in xs]); t = np.array([x[1] for x in xs])
        if t.std() == 0: continue
        ic = spearmanr(p, t).correlation
        if not np.isnan(ic): ics.append(ic)
        k = max(1, len(xs) // 10)
        o = np.argsort(p)
        spreads.append(t[o[-k:]].mean() - t[o[:k]].mean())
        v = np.array([x[2] if x[2] else np.nan for x in xs])
        if not np.isnan(v).any():
            rv = t / v
            rspreads.append(rv[o[-k:]].mean() - rv[o[:k]].mean())
    ics, spreads = np.array(ics), np.array(spreads)
    t_ic = ics.mean() / (ics.std() / np.sqrt(len(ics))) if len(ics) > 2 else 0.0
    return ics.mean(), t_ic, spreads.mean() * 1e4, (np.mean(rspreads) if rspreads else np.nan)

print(f"\n{'fold':<5}{'reward':<14}{'OOS IC':>8}{'t':>7}{'decile spread':>15}{'per-risk spr':>13}")
agg = {"A": ([], [], []), "B": ([], [], [])}
for i in range(FOLDS):
    a = START + timedelta(days=i * block)
    b = END if i == FOLDS - 1 else START + timedelta(days=(i + 1) * block - 1)
    cutoff = a - timedelta(days=2)
    train = [r for r in rows if r["d"] <= cutoff]
    test = [r for r in rows if a <= r["d"] <= b]
    for label, key in (("A", "target"), ("B", "target_ra")):
        pred = fit_predict(train, test, key)
        ic, t, spr, rspr = judge(test, pred)
        agg[label][0].append(ic); agg[label][1].append(spr); agg[label][2].append(rspr)
        name = "demean(ret)" if label == "A" else "demean(r/vol)"
        print(f"{i+1:<5}{name:<14}{ic:>8.4f}{t:>7.2f}{spr:>13.1f}bp{rspr:>13.3f}")

print("\nmeans:")
for label, name in (("A", "demean(ret)"), ("B", "demean(r/vol)")):
    ic, spr, rspr = (np.nanmean(v) for v in agg[label])
    print(f"  {name:<14} IC {ic:+.4f}   spread {spr:.1f}bp   per-risk {rspr:+.3f}")