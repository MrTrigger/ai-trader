"""Hourly sequences per (asset, decision date), for the sequence model.

The LSTM's whole claim is that the PATH carries information a snapshot does not.
That claim was untestable while every feature was a daily aggregate; with hourly
bars it is testable, and this builds the input it needs.

Per decision at 00:00 UTC, the preceding 168 hours (one week) of six quantities
that vary hour to hour:

    log return, |return|, log dollar volume, high-low range, close location in
    the bar, and log trade count

Each is cross-sectionally normalised at its own hour, so the model sees "this
asset relative to the market right now" rather than absolute levels that drift
with the calendar. Without that the LSTM spends its capacity learning that 2021
was more volatile than 2024.

The slow features go in separately, concatenated after the LSTM, because they do
not vary within the sequence and feeding a constant down 168 timesteps is waste.
"""
import sys
from datetime import timezone
from pathlib import Path
import numpy as np
import polars as pl
from planner import store

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
DATA = Path("/home/magnus/dev/magnus/ai-trader/data")
SEQ_H = 168

ds = pl.read_parquet(S / "ds4.parquet")
keys = ds.select(["date", "asset"]).unique()
print(f"{ds.height:,} decision rows to build sequences for", flush=True)

h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
h = h.filter((pl.col("close") > 0) & (pl.col("high") > 0) & (pl.col("low") > 0))
h = h.with_columns([
    (pl.col("close") / pl.col("close").shift(1).over("asset")).log().alias("q_ret"),
    (pl.col("quote_volume") + 1).log().alias("q_dv"),
    ((pl.col("high") - pl.col("low")) / pl.col("close")).alias("q_range"),
    pl.when(pl.col("high") > pl.col("low"))
      .then((pl.col("close") - pl.col("low")) / (pl.col("high") - pl.col("low")))
      .otherwise(0.5).alias("q_clv"),
    (pl.col("trades") + 1).log().alias("q_trades"),
])
h = h.with_columns(pl.col("q_ret").abs().alias("q_absret")).drop_nulls(["q_ret"])

QCOLS = ["q_ret", "q_absret", "q_dv", "q_range", "q_clv", "q_trades"]
# Cross-sectional z-score at each HOUR: relative position, not absolute level.
h = h.with_columns([
    ((pl.col(c) - pl.col(c).mean().over("ts_utc"))
     / (pl.col(c).std().over("ts_utc") + 1e-9)).clip(-5, 5).alias(c)
    for c in QCOLS
])

# Dense (asset, hour) -> row index, so a sequence is a slice rather than a join.
h = h.sort(["asset", "ts_utc"])
assets = h["asset"].to_list()
times = h["ts_utc"].to_list()
mat = h.select(QCOLS).to_numpy().astype(np.float32)
pos = {}
for i, (a, t) in enumerate(zip(assets, times)):
    pos[(a, t.replace(tzinfo=None))] = i

import datetime as dt
want = [(d, a) for d, a in keys.iter_rows()]
X = np.zeros((len(want), SEQ_H, len(QCOLS)), dtype=np.float32)
keep = np.zeros(len(want), dtype=bool)
for k, (d, a) in enumerate(want):
    end = dt.datetime.fromisoformat(d)          # 00:00 of the decision date
    i_end = pos.get((a, end))
    if i_end is None or i_end < SEQ_H:
        continue
    # Contiguity: the slice must be the same asset with no gap.
    if assets[i_end - SEQ_H] != a:
        continue
    span = (times[i_end] - times[i_end - SEQ_H]).total_seconds()
    if span != SEQ_H * 3600:
        continue
    X[k] = mat[i_end - SEQ_H:i_end]
    keep[k] = True

print(f"{keep.sum():,} of {len(want):,} have a clean {SEQ_H}h history "
      f"({keep.sum()/len(want)*100:.0f}%)", flush=True)
idx = pl.DataFrame({"date": [w[0] for w in want], "asset": [w[1] for w in want],
                    "row": np.arange(len(want)), "ok": keep})
np.save(S / "seqX.npy", X)
idx.write_parquet(S / "seqidx.parquet")
print(f"wrote seqX.npy {X.shape} ({X.nbytes/1e6:.0f} MB) and seqidx.parquet")