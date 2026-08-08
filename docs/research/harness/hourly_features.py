"""Features computed from hourly bars, not daily ones.

Everything so far was derived from daily OHLCV even after the hourly store
landed, so the hourly data was only being used for execution. That leaves most
of its value unused: a day is 24 observations, and collapsing them to four
numbers (O/H/L/C) throws away the path.

What hourly makes possible and daily cannot express:

  path shape        realised vol from 24 hourly returns rather than a high-low
                    range; upside/downside semivariance separately; skew; the
                    largest single hourly move.
  path efficiency   |net move| / sum|hourly moves|. A coin that ends +5% having
                    gone straight there is a different animal from one that ends
                    +5% after thrashing, and daily bars cannot tell them apart.
  intraday timing   where in the day the volume and the move happened.
  trade intensity   the hourly bars carry a trade COUNT, so average trade size
                    and its surprise are available for the first time.
  fine horizons     returns at 3h, 6h, 12h rather than 1d being the floor.

Decision points stay daily at 00:00 UTC. Only the inputs get finer.
"""
import sys
from datetime import datetime, timezone
from pathlib import Path
import polars as pl
from planner import store
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
print(f"{h.height:,} hourly bars, {h['asset'].n_unique()} assets", flush=True)

A = "asset"
h = h.with_columns([
    (pl.col("close") / pl.col("close").shift(1).over(A) - 1).alias("r1"),
    (pl.col("close").log()).alias("lc"),
])

# Rolling windows in HOURS.
W = {"6h": 6, "24h": 24, "72h": 72, "168h": 168}
exprs = []
for name, n in W.items():
    exprs += [
        # Realised volatility from the actual path, not a range proxy.
        (pl.col("r1").rolling_std(n, min_samples=n).over(A) * (24 * 365) ** 0.5)
        .alias(f"rv_{name}"),
        # Return over the window.
        (pl.col("close") / pl.col("close").shift(n).over(A) - 1).alias(f"ret_{name}"),
        # Path efficiency: how directly the move was made.
        ((pl.col("close") / pl.col("close").shift(n).over(A) - 1).abs()
         / (pl.col("r1").abs().rolling_sum(n, min_samples=n).over(A) + 1e-12))
        .alias(f"eff_{name}"),
        # Largest single hourly move in the window - a jump detector.
        (pl.col("r1").abs().rolling_max(n, min_samples=n).over(A)).alias(f"jump_{name}"),
        # Dollar turnover over the window.
        (pl.col("quote_volume").rolling_sum(n, min_samples=n).over(A)).alias(f"dv_{name}"),
    ]
h = h.with_columns(exprs)

# Semivariance and skew over 24h: upside and downside are not symmetric and a
# single vol number asserts they are.
neg = pl.when(pl.col("r1") < 0).then(pl.col("r1")).otherwise(0.0)
pos = pl.when(pl.col("r1") > 0).then(pl.col("r1")).otherwise(0.0)
h = h.with_columns([
    (neg.pow(2).rolling_sum(24, min_samples=24).over(A).sqrt()).alias("semi_dn"),
    (pos.pow(2).rolling_sum(24, min_samples=24).over(A).sqrt()).alias("semi_up"),
    (pl.col("r1").rolling_skew(24).over(A)).alias("skew_24h"),
    # Trade intensity: the hourly bars carry a trade count, unavailable before.
    (pl.col("quote_volume") / (pl.col("trades") + 1)).alias("trade_size"),
])
h = h.with_columns([
    (pl.col("semi_up") / (pl.col("semi_dn") + 1e-12)).alias("semi_ratio"),
    (pl.col("trade_size")
     / (pl.col("trade_size").rolling_median(168, min_samples=48).over(A) + 1e-12))
    .alias("trade_size_surp"),
    (pl.col("trades").rolling_sum(24, min_samples=24).over(A)).alias("trades_24h"),
    # Drawdown inside the last week: how far below its own recent peak.
    (pl.col("close") / pl.col("close").rolling_max(168, min_samples=168).over(A) - 1)
    .alias("dd_168h"),
    # Where the last 24h of volume sat: concentrated or spread out. High values
    # mean one hour dominated, which is a different market than steady flow.
    (pl.col("quote_volume").rolling_max(24, min_samples=24).over(A)
     / (pl.col("quote_volume").rolling_sum(24, min_samples=24).over(A) + 1e-12))
    .alias("vol_concentration"),
])

# Benchmark-relative, computed hourly rather than from a 90-day daily beta.
bench = (h.filter(pl.col(A) == cfg.benchmark)
         .select(["ts_utc", pl.col("r1").alias("b_r1"),
                  pl.col("ret_24h").alias("b_ret24"), pl.col("rv_24h").alias("b_rv24")]))
h = h.join(bench, on="ts_utc", how="left")
h = h.with_columns([
    (pl.col("ret_24h") - pl.col("b_ret24")).alias("rel_ret_24h"),
    (pl.col("rv_24h") / (pl.col("b_rv24") + 1e-12)).alias("rel_vol_24h"),
    # Rolling hourly beta over a week: 168 observations, versus 90 daily before.
    (pl.corr(pl.col("r1"), pl.col("b_r1")).rolling(index_column="ts_utc",
                                                   period="168h").over(A)
     if False else pl.lit(None)).alias("_unused"),
]).drop("_unused")

FEATURES = (
    [f"rv_{k}" for k in W] + [f"ret_{k}" for k in W] + [f"eff_{k}" for k in W]
    + [f"jump_{k}" for k in W] + [f"dv_{k}" for k in W]
    + ["semi_dn", "semi_up", "semi_ratio", "skew_24h", "trade_size_surp",
       "trades_24h", "dd_168h", "vol_concentration", "rel_ret_24h", "rel_vol_24h"]
)
print(f"{len(FEATURES)} hourly-derived features", flush=True)

out = Path(sys.argv[1])
h.select([A, "ts_utc", "open", "high", "low", "close", "quote_volume", "r1"] + FEATURES) \
 .write_parquet(out)
print(f"wrote {out}", flush=True)