"""Out-of-sample rank IC of the ranker against the 24h target it is trained on.

If this is positive, the signal works at its own horizon and the weekly
rebalance is throwing the edge away. If it is ~zero or negative, the model
itself is the problem and cadence is a side issue.
"""
import json, numpy as np, lightgbm as lgb
from datetime import date
from collections import defaultdict
from scipy.stats import spearmanr

MATRIX = "data/models/training.jsonl"
CUT = date(2022, 9, 17)

with open(MATRIX) as f:
    man = json.loads(next(f))
    rows = [json.loads(l) for l in f if l.strip()]
feats = man["features"]
tr = [r for r in rows if date.fromisoformat(r["date"]) <= CUT]
te = [r for r in rows if date.fromisoformat(r["date"]) > CUT]
print(f"train {len(tr):,} rows  test {len(te):,} rows  {len(feats)} features")

X = np.asarray([[r["features"][n] for n in feats] for r in tr])
y = np.asarray([r["target"] for r in tr])
params = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1)
booster = lgb.train(params, lgb.Dataset(X, label=y), num_boost_round=300)

Xt = np.asarray([[r["features"][n] for n in feats] for r in te])
pred = booster.predict(Xt)

# Cross-sectional rank IC per decision date, then averaged. Pooling all rows
# would mostly measure whether some days are easier than others.
byday = defaultdict(lambda: ([], []))
for r, p in zip(te, pred):
    byday[r["date"]][0].append(p)
    byday[r["date"]][1].append(r["target"])
ics = [spearmanr(p, t).correlation for p, t in byday.values()
       if len(p) >= 10 and np.std(t) > 0]
ics = [i for i in ics if not np.isnan(i)]
ics = np.asarray(ics)
print(f"\n24h horizon, {len(ics)} out-of-sample decision dates")
print(f"  mean rank IC   {ics.mean():+.4f}")
print(f"  median         {np.median(ics):+.4f}")
print(f"  std            {ics.std():.4f}")
print(f"  t-stat         {ics.mean()/(ics.std()/np.sqrt(len(ics))):+.2f}")
print(f"  % days > 0     {(ics > 0).mean():.1%}")