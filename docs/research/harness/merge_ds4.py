"""Merge the daily and hourly feature sets rather than choosing between them.

Replacing the daily features with hourly ones cost most of the edge: Sharpe fell
from 2.33 to 0.74. The reason is that the strongest features in the IC screen are
SLOW - 30-day volatility, 20-day median turnover, 90-day beta - and the hourly
windows built here top out at 168 hours, so swapping one set for the other threw
away the long-horizon information and kept only the short.

They answer different questions and the model should see both: the slow features
say which assets are structurally attractive, the fast ones say what is happening
right now.

Target is the hourly set's - the 1h-lagged 24h return, which is the trade the
book actually makes.
"""
from pathlib import Path
import polars as pl

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
d2 = pl.read_parquet(S / "ds2.parquet")   # daily-derived features
d3 = pl.read_parquet(S / "ds3.parquet")   # hourly-derived features + honest target

slow = [c for c in d2.columns if c.startswith("x_")]
fast = [c for c in d3.columns if c.startswith("x_")]
print(f"slow (daily-derived) {len(slow)}   fast (hourly-derived) {len(fast)}")

# Keep the hourly set's target: it is the one measured against a tradeable fill.
merged = d3.join(d2.select(["date", "asset"] + slow), on=["date", "asset"], how="inner")
print(f"merged {merged.height:,} rows, {merged['date'].n_unique():,} dates, "
      f"{len(slow) + len(fast)} features")

# Any feature that is constant within a date carries no cross-sectional
# information and only costs the model capacity.
drop = []
for c in slow + fast:
    if merged.select(pl.col(c).std().over("date").mean())[0, 0] in (None, 0.0):
        drop.append(c)
if drop:
    merged = merged.drop(drop)
    print(f"dropped {len(drop)} degenerate: {drop}")

merged.write_parquet(S / "ds4.parquet")
print(f"wrote ds4.parquet ({merged.width} cols)")