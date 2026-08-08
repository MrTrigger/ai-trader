"""A shared LSTM over hourly sequences, benchmarked against the gradient booster.

Architecture follows the shape that makes sense for cross-sectional ranking:

    168h x 6 hourly features  ->  shared LSTM  ->  last hidden state  --,
                                                                        +-> MLP -> score
    58 slow cross-sectional features  ---------------------------------'

One LSTM for every asset, not one per asset. No coin-ID embedding: with 213
assets and 2,353 dates an identity embedding is an invitation to memorise which
coins did well, which is the past rather than a prediction.

Trained with a LISTWISE loss inside each date - softmax over the day's scores
against softmax over the day's returns - because the objective is an ordering,
not a level. A regression loss would spend capacity on predicting return
magnitude, which the portfolio never uses.

Identical folds, identical purge and identical target as the booster, so the
comparison is the model and nothing else.
"""
import json, math, sys
from pathlib import Path
import numpy as np
import polars as pl
import torch
import torch.nn as nn

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
torch.manual_seed(0)
np.random.seed(0)

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
idx = pl.read_parquet(S / "seqidx.parquet")
X = np.load(S / "seqX.npy")
ds = ds.join(idx, on=["date", "asset"], how="left").filter(pl.col("ok"))
XC = [c for c in ds.columns if c.startswith("x_")]
rows = ds["row"].to_numpy()
Xseq = torch.from_numpy(X[rows])
Xflat = torch.from_numpy(ds.select(XC).to_numpy().astype(np.float32))
y = torch.from_numpy(ds["t1"].to_numpy().astype(np.float32))
dates = ds["date"].to_numpy()
uniq = sorted(set(dates.tolist()))
print(f"{len(ds):,} rows  {len(uniq):,} dates  seq {Xseq.shape[1]}x{Xseq.shape[2]}  "
      f"flat {Xflat.shape[1]}", flush=True)

# Row ranges per date, so a batch is a whole cross-section.
bounds = {}
start = 0
for i in range(1, len(dates) + 1):
    if i == len(dates) or dates[i] != dates[start]:
        bounds[dates[start]] = (start, i)
        start = i


class Ranker(nn.Module):
    def __init__(self, n_seq, n_flat, hidden=32):
        super().__init__()
        self.lstm = nn.LSTM(n_seq, hidden, num_layers=1, batch_first=True)
        self.head = nn.Sequential(
            nn.Linear(hidden + n_flat, 48), nn.ReLU(), nn.Dropout(0.3),
            nn.Linear(48, 1),
        )

    def forward(self, seq, flat):
        _, (h, _) = self.lstm(seq)
        return self.head(torch.cat([h[-1], flat], dim=1)).squeeze(-1)


def listwise(scores, target, tau=1.0):
    """Cross-entropy between the score softmax and the return softmax, per date."""
    p = torch.log_softmax(scores / tau, dim=0)
    q = torch.softmax(target / (target.std() + 1e-8), dim=0)
    return -(q * p).sum()


def spearman(a, b):
    ra = np.argsort(np.argsort(a)).astype(float)
    rb = np.argsort(np.argsort(b)).astype(float)
    ra -= ra.mean(); rb -= rb.mean()
    d = math.sqrt((ra**2).sum() * (rb**2).sum())
    return float((ra*rb).sum()/d) if d else 0.0


N_FOLDS, HORIZON, EPOCHS = 6, 1, 12
n = len(uniq)
first_test = int(n * 0.4)
edges = np.linspace(first_test, n, N_FOLDS + 1).astype(int)
preds, fold_ic = {}, []

for k in range(N_FOLDS):
    lo, hi = edges[k], edges[k + 1]
    test_d = uniq[lo:hi]
    train_d = uniq[:max(0, lo - HORIZON)]
    if len(train_d) < 200:
        continue
    model = Ranker(Xseq.shape[2], Xflat.shape[1])
    opt = torch.optim.Adam(model.parameters(), lr=2e-3, weight_decay=1e-4)
    for ep in range(EPOCHS):
        model.train()
        order = np.random.permutation(len(train_d))
        tot = 0.0
        for j in order:
            d = train_d[j]
            a, b = bounds[d]
            if b - a < 10:
                continue
            opt.zero_grad()
            s = model(Xseq[a:b], Xflat[a:b])
            loss = listwise(s, y[a:b])
            loss.backward()
            nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step()
            tot += float(loss)
    model.eval()
    ics = []
    with torch.no_grad():
        for d in test_d:
            a, b = bounds[d]
            if b - a < 10:
                continue
            s = model(Xseq[a:b], Xflat[a:b]).numpy()
            yy = y[a:b].numpy()
            ics.append(spearman(s, yy))
            for i, sc in zip(range(a, b), s):
                preds[(dates[i], ds["asset"][i])] = float(sc)
    fold_ic.append(float(np.mean(ics)))
    print(f"  fold {k+1}: train {len(train_d)} dates, test {len(test_d)}, "
          f"IC {np.mean(ics):+.4f}", flush=True)

arr = np.array(fold_ic)
t = arr.mean() / (arr.std(ddof=1) / math.sqrt(len(arr))) if len(arr) > 1 else 0.0
print(f"\nLSTM  mean OOS IC {arr.mean():+.4f}  across {len(arr)} folds  "
      f"fold-level t {t:+.2f}")
print(f"GBM   mean OOS IC +0.0643  fold-level t +9.92   (same folds, same target)")
json.dump({"daily": {f"{d}|{a}": v for (d, a), v in preds.items()}},
          open(S / "preds_lstm.json", "w"))
print("wrote preds_lstm.json")