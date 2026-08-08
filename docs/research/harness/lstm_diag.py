"""Why is the LSTM's IC negative? Separate overfitting from a wiring error.

A model that fails to learn scores about zero. This one scored -0.0347 in all six
folds, which is systematic INVERSION and a different thing - it means either the
signs are crossed somewhere, or it is fitting the training set so hard that the
test set anti-correlates.

The discriminating measurement is the training IC:

  strongly positive train, negative test  ->  overfitting
  negative train too                      ->  the loss or the wiring is wrong

Reported per epoch, on one fold, so the trajectory is visible rather than the
endpoint. Also checks the two things most likely to be crossed: that the
sequences line up with their labels, and that the loss actually decreases.
"""
import math
from pathlib import Path
import numpy as np
import polars as pl
import torch
import torch.nn as nn

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
torch.manual_seed(0); np.random.seed(0)

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
idx = pl.read_parquet(S / "seqidx.parquet")
X = np.load(S / "seqX.npy")
ds = ds.join(idx, on=["date", "asset"], how="left").filter(pl.col("ok"))
XC = [c for c in ds.columns if c.startswith("x_")]
rows = ds["row"].to_numpy()

# --- alignment check --------------------------------------------------------
# seqX was built over the UNIQUE (date, asset) pairs; if `row` did not survive
# the join correctly every sequence would belong to the wrong observation.
chk = idx.filter(pl.col("ok")).head(3)
print("alignment spot-check (row index -> the pair it was built for):")
for d, a, r, _ in chk.iter_rows():
    print(f"   row {r:>6}  {d}  {a}")
print(f"   seqX shape {X.shape}, max row referenced {rows.max()}")

Xseq = torch.from_numpy(X[rows])
Xflat = torch.from_numpy(ds.select(XC).to_numpy().astype(np.float32))
y = torch.from_numpy(ds["t1"].to_numpy().astype(np.float32))
dates = ds["date"].to_numpy()
uniq = sorted(set(dates.tolist()))
bounds, start = {}, 0
for i in range(1, len(dates) + 1):
    if i == len(dates) or dates[i] != dates[start]:
        bounds[dates[start]] = (start, i); start = i


class Ranker(nn.Module):
    def __init__(self, n_seq, n_flat, hidden=32):
        super().__init__()
        self.lstm = nn.LSTM(n_seq, hidden, batch_first=True)
        self.head = nn.Sequential(nn.Linear(hidden + n_flat, 48), nn.ReLU(),
                                  nn.Dropout(0.3), nn.Linear(48, 1))
    def forward(self, seq, flat):
        _, (h, _) = self.lstm(seq)
        return self.head(torch.cat([h[-1], flat], dim=1)).squeeze(-1)


def listwise(scores, target):
    p = torch.log_softmax(scores, dim=0)
    q = torch.softmax(target / (target.std() + 1e-8), dim=0)
    return -(q * p).sum()


def spearman(a, b):
    ra = np.argsort(np.argsort(a)).astype(float); rb = np.argsort(np.argsort(b)).astype(float)
    ra -= ra.mean(); rb -= rb.mean()
    d = math.sqrt((ra**2).sum() * (rb**2).sum())
    return float((ra*rb).sum()/d) if d else 0.0


def ic_over(model, dlist):
    model.eval(); out = []
    with torch.no_grad():
        for d in dlist:
            a, b = bounds[d]
            if b - a < 10: continue
            out.append(spearman(model(Xseq[a:b], Xflat[a:b]).numpy(), y[a:b].numpy()))
    model.train()
    return float(np.mean(out))


n = len(uniq); lo = int(n * 0.6)
train_d, test_d = uniq[:lo - 1], uniq[lo:]
print(f"\none fold: {len(train_d)} train dates, {len(test_d)} test dates")

model = Ranker(Xseq.shape[2], Xflat.shape[1])
opt = torch.optim.Adam(model.parameters(), lr=2e-3, weight_decay=1e-4)
print(f"\n{'epoch':>6}{'loss':>10}{'train IC':>11}{'test IC':>10}")
print(f"{0:>6}{'-':>10}{ic_over(model, train_d[:200]):>11.4f}{ic_over(model, test_d):>10.4f}"
      "   (untrained)")
for ep in range(1, 9):
    order = np.random.permutation(len(train_d))
    tot, cnt = 0.0, 0
    for j in order:
        d = train_d[j]
        a, b = bounds[d]
        if b - a < 10: continue
        opt.zero_grad()
        loss = listwise(model(Xseq[a:b], Xflat[a:b]), y[a:b])
        loss.backward()
        nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        opt.step()
        tot += float(loss.detach()); cnt += 1
    print(f"{ep:>6}{tot/cnt:>10.4f}{ic_over(model, train_d[:200]):>11.4f}"
          f"{ic_over(model, test_d):>10.4f}")

print("\nreference: the gradient booster scores +0.0643 test IC on the same data.")