"""Ablation: target transform, capacity, epochs - judged on DECILE SPREAD.

Two things are being corrected at once.

**The metric.** IC scores the whole cross-section; the book holds deciles 1 and
10. Selecting on IC is what led to preferring a model that ranks the middle
smoothly and the tails poorly. Everything here reports the top-minus-bottom
decile spread, and each tail separately so an imbalance cannot hide inside a
symmetric-looking total.

**The target.** The demeaned return is skewed +2.59 with kurtosis 45, and the top
decile carries 46.5% of each date's squared variation against the bottom's 25.8%.
A correlation loss weights by exactly that, so it spends 1.8x more gradient on
winners than losers - which is the measured imbalance, not a coincidence.
Rank-transforming the target makes every name contribute equally.

Capacity and epochs are swept because the previous run may simply have been too
small: training IC was still climbing at epoch 8, which is not the signature of a
converged model.
"""
import json, math, statistics, sys
from pathlib import Path
import numpy as np
import polars as pl
import torch
import torch.nn as nn

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
idx = pl.read_parquet(S / "seqidx.parquet")
X = np.load(S / "seqX.npy")
ds = ds.join(idx, on=["date", "asset"], how="left").filter(pl.col("ok"))
XC = [c for c in ds.columns if c.startswith("x_")]
Xseq_all = torch.from_numpy(X[ds["row"].to_numpy()])
Xflat_all = torch.from_numpy(ds.select(XC).to_numpy().astype(np.float32))
y_raw = ds["t1"].to_numpy().astype(np.float32)
dates = ds["date"].to_numpy()
uniq = sorted(set(dates.tolist()))
bounds, start = {}, 0
for i in range(1, len(dates) + 1):
    if i == len(dates) or dates[i] != dates[start]:
        bounds[dates[start]] = (start, i); start = i

# Rank-transformed target: within each date, map to [-1, 1] by rank. Equal
# influence per name, so the skew cannot tilt the gradient toward winners.
y_rank = np.zeros_like(y_raw)
for d, (a, b) in bounds.items():
    v = y_raw[a:b]
    r = np.argsort(np.argsort(v)).astype(np.float32)
    y_rank[a:b] = 2.0 * r / max(1, len(v) - 1) - 1.0
Y = {"raw": torch.from_numpy(y_raw), "rank": torch.from_numpy(y_rank)}
TRUE = y_raw


class Ranker(nn.Module):
    def __init__(self, n_seq, n_flat, hidden, layers, drop):
        super().__init__()
        self.lstm = nn.LSTM(n_seq, hidden, num_layers=layers, batch_first=True,
                            dropout=drop if layers > 1 else 0.0)
        self.head = nn.Sequential(nn.Linear(hidden + n_flat, 64), nn.ReLU(),
                                  nn.Dropout(drop), nn.Linear(64, 1))
    def forward(self, seq, flat):
        _, (h, _) = self.lstm(seq)
        return self.head(torch.cat([h[-1], flat], dim=1)).squeeze(-1)


def corr_loss(s, t):
    s = s - s.mean(); t = t - t.mean()
    d = torch.sqrt((s*s).sum() * (t*t).sum()) + 1e-8
    return -(s*t).sum() / d


def evaluate(preds):
    """Decile means of the REAL return, plus IC, over the collected predictions."""
    byd = {}
    for (d, i), v in preds.items():
        byd.setdefault(d, []).append((i, v))
    buckets = [[] for _ in range(10)]
    ics = []
    for d, items in byd.items():
        if len(items) < 20: continue
        items.sort(key=lambda x: x[1])
        n = len(items)
        for j, (i, _) in enumerate(items):
            buckets[min(9, j*10//n)].append(float(TRUE[i]))
        sc = np.array([v for _, v in items]); tv = np.array([TRUE[i] for i, _ in items])
        ra = np.argsort(np.argsort(sc)).astype(float); rb = np.argsort(np.argsort(tv)).astype(float)
        ra -= ra.mean(); rb -= rb.mean()
        dd = math.sqrt((ra**2).sum()*(rb**2).sum())
        if dd: ics.append((ra*rb).sum()/dd)
    m = [statistics.mean(b) for b in buckets]
    return dict(ic=statistics.mean(ics), bot=m[0], top=m[9], spread=m[9]-m[0])


N_FOLDS = 6
n = len(uniq); EDGES = np.linspace(int(n*0.4), n, N_FOLDS+1).astype(int)


def run(target, hidden, layers, epochs, drop=0.3, lr=2e-3):
    yt = Y[target]
    preds = {}
    for k in range(N_FOLDS):
        lo, hi = EDGES[k], EDGES[k+1]
        test_d, train_d = uniq[lo:hi], uniq[:max(0, lo-1)]
        if len(train_d) < 200: continue
        torch.manual_seed(0); np.random.seed(0)
        model = Ranker(Xseq_all.shape[2], Xflat_all.shape[1], hidden, layers, drop)
        opt = torch.optim.Adam(model.parameters(), lr=lr, weight_decay=1e-4)
        for _ in range(epochs):
            model.train()
            for j in np.random.permutation(len(train_d)):
                d = train_d[j]; a, b = bounds[d]
                if b - a < 10: continue
                opt.zero_grad()
                loss = corr_loss(model(Xseq_all[a:b], Xflat_all[a:b]), yt[a:b])
                loss.backward()
                nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                opt.step()
        model.eval()
        with torch.no_grad():
            for d in test_d:
                a, b = bounds[d]
                if b - a < 10: continue
                sc = model(Xseq_all[a:b], Xflat_all[a:b]).numpy()
                for i, v in zip(range(a, b), sc):
                    preds[(d, i)] = float(v)
    return evaluate(preds)


GRID = [
    ("raw target,  h32 L1 e8", "raw", 32, 1, 8),
    ("RANK target, h32 L1 e8", "rank", 32, 1, 8),
    ("RANK target, h64 L1 e16", "rank", 64, 1, 16),
    ("RANK target, h64 L2 e16", "rank", 64, 2, 16),
]
print(f"{'config':<26}{'IC':>9}{'decile 1':>11}{'decile 10':>11}{'spread':>10}")
print(f"{'GBM reference':<26}{0.0643:>9.4f}{-0.295:>10.3f}%{0.276:>10.3f}%{0.571:>9.3f}%")
for label, tgt, h, L, ep in GRID:
    r = run(tgt, h, L, ep)
    print(f"{label:<26}{r['ic']:>9.4f}{r['bot']*100:>10.3f}%{r['top']*100:>10.3f}%"
          f"{r['spread']*100:>9.3f}%", flush=True)