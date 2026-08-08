"""Same model, trained on the objective the book is actually judged by.

The listwise loss rewards ordering the whole cross-section, but the portfolio is
long six names and short six names: rank 14 against rank 17 earns nothing, and
the loss weights it as heavily as rank 1. It is also cost-blind, which matters
when the book turns over 322x NAV a year, and its softmax over returns lets one
+50% day dominate the gradient.

This trains on the portfolio instead. Scores become weights, weights become a
net-of-cost return, and the objective is that return series' own Sharpe:

    w_t      = softmax-style long/short weights from the scores, gross-capped
    r_t      = w_t . y_t  -  cost * |w_t - w_{t-1}|
    loss     = -mean(r) / std(r)

Three properties that the ranking loss does not have:

  the extremes matter most, because a soft top-k concentrates weight there and
  the middle of the book gets almost none;
  turnover is priced, so a jittery ranking is penalised for being jittery;
  the objective is scale-aware - it knows that a small edge is not worth the
  spread, which a pure ordering can never express.

Dates are processed in order within each epoch so that w_{t-1} is the real
previous book, not a shuffled one. Sharpe is computed over the epoch's whole
return series rather than per batch, because a per-date Sharpe is undefined.
"""
import json, math
from pathlib import Path
import numpy as np
import polars as pl
import torch
import torch.nn as nn

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
torch.manual_seed(0); np.random.seed(0)

GROSS, COST_BPS, N_SIDE = 0.80, 5.0, 6

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
idx = pl.read_parquet(S / "seqidx.parquet")
X = np.load(S / "seqX.npy")
ds = ds.join(idx, on=["date", "asset"], how="left").filter(pl.col("ok"))
XC = [c for c in ds.columns if c.startswith("x_")]
Xseq = torch.from_numpy(X[ds["row"].to_numpy()])
Xflat = torch.from_numpy(ds.select(XC).to_numpy().astype(np.float32))
y = torch.from_numpy(ds["t1"].to_numpy().astype(np.float32))
dates = ds["date"].to_numpy()
assets = ds["asset"].to_list()
uniq = sorted(set(dates.tolist()))
bounds, start = {}, 0
for i in range(1, len(dates) + 1):
    if i == len(dates) or dates[i] != dates[start]:
        bounds[dates[start]] = (start, i); start = i
print(f"{len(ds):,} rows  {len(uniq):,} dates  flat {Xflat.shape[1]}", flush=True)


class Ranker(nn.Module):
    def __init__(self, n_seq, n_flat, hidden=32):
        super().__init__()
        self.lstm = nn.LSTM(n_seq, hidden, batch_first=True)
        self.head = nn.Sequential(nn.Linear(hidden + n_flat, 48), nn.ReLU(),
                                  nn.Dropout(0.3), nn.Linear(48, 1))

    def forward(self, seq, flat):
        _, (h, _) = self.lstm(seq)
        return self.head(torch.cat([h[-1], flat], dim=1)).squeeze(-1)


def weights_from(scores, temp=2.0):
    """Soft long/short book: concentrated at the extremes, gross-capped, dollar-neutral.

    A hard top-k has no gradient, so the long side is a softmax over the scores
    and the short side a softmax over their negation. Temperature controls how
    concentrated the book is; at temp -> 0 it approaches an equal-weight top-k.
    """
    n = scores.shape[0]
    k = min(N_SIDE, max(2, n // 4))
    z = (scores - scores.mean()) / (scores.std() + 1e-8)
    wl = torch.softmax(z * temp, dim=0)
    ws = torch.softmax(-z * temp, dim=0)
    w = (wl - ws) * (GROSS / 2.0)
    # Cap any single name, then renormalise gross so the cap cannot inflate it.
    w = torch.clamp(w, -0.25, 0.25)
    g = w.abs().sum()
    return w * (GROSS / g) if float(g) > GROSS else w


def portfolio_sharpe(model, date_list, train=True):
    """Run the book across dates in order and return its Sharpe, differentiably."""
    rets, prev = [], {}
    for d in date_list:
        a, b = bounds[d]
        if b - a < 12:
            continue
        s = model(Xseq[a:b], Xflat[a:b])
        w = weights_from(s)
        r = (w * y[a:b]).sum()
        names = assets[a:b]
        # Turnover against the actual previous book, so a stable ranking is
        # rewarded for being stable.
        turn = torch.zeros((), dtype=torch.float32)
        cur = {}
        for j, nm in enumerate(names):
            cur[nm] = w[j]
            turn = turn + torch.abs(w[j] - prev.get(nm, torch.zeros(())))
        for nm, pw in prev.items():
            if nm not in cur:
                turn = turn + torch.abs(pw)
        rets.append(r - turn * COST_BPS / 10_000)
        prev = {k: v.detach() for k, v in cur.items()}
    if not rets:
        return None
    R = torch.stack(rets)
    return R.mean() / (R.std() + 1e-8)


def spearman(a, b):
    ra = np.argsort(np.argsort(a)).astype(float); rb = np.argsort(np.argsort(b)).astype(float)
    ra -= ra.mean(); rb -= rb.mean()
    d = math.sqrt((ra**2).sum() * (rb**2).sum())
    return float((ra*rb).sum()/d) if d else 0.0


N_FOLDS, HORIZON, EPOCHS, CHUNK = 6, 1, 6, 60
n = len(uniq); edges = np.linspace(int(n*0.4), n, N_FOLDS+1).astype(int)
preds, fold_ic, fold_sh = {}, [], []

for k in range(N_FOLDS):
    lo, hi = edges[k], edges[k+1]
    test_d, train_d = uniq[lo:hi], uniq[:max(0, lo-HORIZON)]
    if len(train_d) < 200:
        continue
    model = Ranker(Xseq.shape[2], Xflat.shape[1])
    opt = torch.optim.Adam(model.parameters(), lr=1e-3, weight_decay=1e-4)
    for ep in range(EPOCHS):
        model.train()
        # Contiguous chunks, in date order: turnover needs a real predecessor.
        starts = list(range(0, len(train_d) - CHUNK, CHUNK))
        np.random.shuffle(starts)
        for st in starts:
            opt.zero_grad()
            sh = portfolio_sharpe(model, train_d[st:st+CHUNK])
            if sh is None:
                continue
            (-sh).backward()
            nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            opt.step()
    model.eval()
    ics = []
    with torch.no_grad():
        for d in test_d:
            a, b = bounds[d]
            if b - a < 10:
                continue
            s = model(Xseq[a:b], Xflat[a:b]).numpy()
            ics.append(spearman(s, y[a:b].numpy()))
            for i, sc in zip(range(a, b), s):
                preds[(dates[i], assets[i])] = float(sc)
        oos = portfolio_sharpe(model, test_d)
    fold_ic.append(float(np.mean(ics))); fold_sh.append(float(oos) * math.sqrt(365))
    print(f"  fold {k+1}: IC {np.mean(ics):+.4f}   OOS Sharpe {float(oos)*math.sqrt(365):+.2f}",
          flush=True)

a = np.array(fold_ic)
t = a.mean()/(a.std(ddof=1)/math.sqrt(len(a))) if len(a) > 1 else 0
print(f"\nLSTM + portfolio-Sharpe objective: IC {a.mean():+.4f}  t {t:+.2f}  "
      f"mean fold Sharpe {np.mean(fold_sh):+.2f}")
json.dump({"daily": {f"{d}|{x}": v for (d, x), v in preds.items()}},
          open(S / "preds_lstm_pf.json", "w"))
print("wrote preds_lstm_pf.json")