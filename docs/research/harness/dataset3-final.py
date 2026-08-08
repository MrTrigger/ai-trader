"""Decision-point dataset on hourly features, with execution lag in the TARGET.

Two changes from the daily version, and the second matters more:

**Features are hourly-derived.** Path efficiency, semivariance, skew, trade
intensity, jump size and intraday volume concentration have no daily equivalent.

**The target is what a trade would actually have earned.** Previously the label
was the return from the bar open at the decision timestamp - the same instant the
features were computed. The model was therefore trained to predict a return
nobody could capture, and the daily backtest inherited that. Here the label runs
from the open one hour AFTER the decision to the open 24 hours later, which is
the trade a system placing an order on the signal would get.

Training on the realistic target also lets the model learn what is still
predictable after the delay, rather than learning fast-decaying structure and
having it thrown away at execution time.
"""
import json, math, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, universe
from planner.config import Config

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
LAG_H, HOLD_H = 1, 24

hf = pl.read_parquet(SCRATCH / "hf.parquet")
FEATURES = [c for c in hf.columns
            if c not in ("asset", "ts_utc", "open", "high", "low", "close",
                         "quote_volume", "r1")]
print(f"{len(FEATURES)} features from {hf.height:,} hourly rows", flush=True)

perp = borrow.listings(root=DATA)
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}

# Execution prices: open at each hour.
PX = {(a, t): o for a, t, o in hf.select(["asset", "ts_utc", "open"]).iter_rows()}

# Decision rows: the hourly bar at 00:00 carries features from everything before it.
dec = hf.filter((pl.col("ts_utc").dt.hour() == 0)).sort(["ts_utc", "asset"])
print(f"{dec.height:,} decision rows at 00:00", flush=True)

U = timezone.utc
rows = []
for (ts,), g in dec.partition_by("ts_utc", as_dict=True).items():
    day = ts.replace(tzinfo=U) if ts.tzinfo is None else ts
    try:
        members = universe.load(day, root=DATA)
    except FileNotFoundError:
        continue
    elig = {m.asset for m in members if m.eligible}
    entry = day + timedelta(hours=LAG_H)
    exit_ = entry + timedelta(hours=HOLD_H)
    block = []
    for r in g.iter_rows(named=True):
        a = r["asset"]
        if a not in elig:
            continue
        first = perp.get(a)
        if first is None or first > day.date():
            continue
        if r["dv_24h"] is None or r["dv_24h"] < float(cfg.min_dollar_volume):
            continue
        if r["rv_24h"] is None or r["rv_24h"] < float(cfg.min_volatility):
            continue
        p0, p1 = PX.get((a, entry)), PX.get((a, exit_))
        if not p0 or not p1:
            continue
        vals = {f: r[f] for f in FEATURES}
        if any(v is not None and not math.isfinite(v) for v in vals.values()):
            vals = {k: (v if v is None or math.isfinite(v) else None) for k, v in vals.items()}
        t = ftab.get(a, {})
        block.append({"date": day.date().isoformat(), "asset": a, **vals,
                      "funding_1d": t.get(day.date(), 0.0),
                      "y": p1 / p0 - 1})
    if len(block) >= 12:
        rows.extend(block)

df = pl.DataFrame(rows)
FEATURES = FEATURES + ["funding_1d"]
print(f"{df.height:,} rows, {df['date'].n_unique():,} dates, {df['asset'].n_unique()} assets",
      flush=True)

# Rank within date, scaled to [-1, 1]; nulls to the middle.
ex = []
for f in FEATURES:
    r = pl.col(f).rank("average").over("date")
    n = pl.col(f).is_not_null().sum().over("date")
    ex.append(pl.when(pl.col(f).is_null()).then(0.0)
              .otherwise(2.0 * (r - 1) / pl.max_horizontal(n - 1, pl.lit(1)) - 1.0)
              .alias(f"x_{f}"))
ex.append((pl.col("y") - pl.col("y").mean().over("date")).alias("t1"))
df = df.with_columns(ex)
df.write_parquet(sys.argv[1])
print(f"wrote {sys.argv[1]} ({df.width} cols, {len(FEATURES)} features)")