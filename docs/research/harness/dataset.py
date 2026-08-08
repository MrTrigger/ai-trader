"""Build the (asset, date) feature/target matrix once, for both cadences.

Three things decide whether a learned ranker means anything, and all three are
in here rather than in the model:

**Cross-sectional normalisation.** Every feature is converted to its rank WITHIN
its own date. A model trained on raw levels would spend its capacity learning
that 2021 volatility differs from 2024 volatility - a fact about the calendar,
not about which asset to hold. The book is long/short within a date, so ranks
within a date are the only thing it can act on.

**The target is relative, not absolute.** Forward return minus that date's
cross-sectional mean. Predicting absolute return means predicting the market,
which is the tilt's job; the ranker's job is which names beat which.

**Causality.** Features come from the feature frame at the bar BEFORE the
decision, exactly as the live path reads them. Targets look forward from the
decision. Nothing here can see across its own horizon.

Emitted at daily resolution with both a 1-day and a 7-day forward target, so the
two cadences are trained and tested on the same rows and any difference between
them is the cadence rather than the sample.
"""
import json, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import numpy as np
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, perp_listed_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}

# Everything cheap and already computed, plus a few derived. Deliberately NOT
# pre-selected on the IC screen: that screen has its own selection bias, and a
# regularised model with honest cross-validation is the better filter.
FEATURES = [
    "ret_7", "ret_30", "ret_90", "ret_30_skip_7", "vol_30", "adv_quote",
    "beta_bench", "dist_upper", "dist_lower", "band_width", "gc_regime_slope",
    "breakout_age", "funding_7d", "funding_30d", "vol_ratio", "ret_1",
]

U = timezone.utc
START, END = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)


def funding_sum(asset, day, n):
    t = ftab.get(asset)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))


rows = []
day = START
while day <= END:
    hz = day - timedelta(seconds=cfg.interval_s)
    try:
        members = universe.load(day, root=DATA)
    except FileNotFoundError:
        day += timedelta(days=1); continue
    e = {m.asset for m in members if m.eligible}
    cx = frame.filter((pl.col("ts_utc") == hz) & pl.col("asset").is_in(list(e))
        & (pl.col("bars_available") >= cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30") >= float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null() & pl.col("perp_listed"))
    if cx.height < 12:
        day += timedelta(days=1); continue

    names = cx["asset"].to_list()
    f1 = ic._forward_returns(prices, names, day, 1)
    f7 = ic._forward_returns(prices, names, day, 7)
    t1 = dict(zip(f1["asset"].to_list(), f1["forward_return"].to_list()))
    t7 = dict(zip(f7["asset"].to_list(), f7["forward_return"].to_list()))

    block = []
    for r in cx.iter_rows(named=True):
        a = r["asset"]
        if t1.get(a) is None or t7.get(a) is None:
            continue
        up, lo, cl = r["gc_upper"], r["gc_lower"], r["close"]
        block.append({
            "date": day.date().isoformat(), "asset": a,
            "ret_1": r["ret_1"], "ret_7": r["ret_7"], "ret_30": r["ret_30"],
            "ret_90": r["ret_90"], "ret_30_skip_7": r["ret_30_skip_7"],
            "vol_30": r["vol_30"], "adv_quote": r["adv_quote"],
            "beta_bench": r["beta_bench"],
            "dist_upper": (cl - up) / abs(up) if up else None,
            "dist_lower": (cl - lo) / abs(lo) if lo else None,
            "band_width": (up - lo) / cl if cl else None,
            "gc_regime_slope": r.get("gc_regime_slope"),
            "breakout_age": r["gc_breakout_age"] if r["gc_breakout_age"] is not None else 0,
            "funding_7d": funding_sum(a, day, 7),
            "funding_30d": funding_sum(a, day, 30),
            "vol_ratio": (r["vol_30"] / abs(r["ret_90"])) if r.get("ret_90") else None,
            "y1": t1[a], "y7": t7[a],
        })
    if len(block) >= 12:
        rows.extend(block)
    day += timedelta(days=1)

df = pl.DataFrame(rows)
print(f"{df.height:,} rows, {df['date'].n_unique():,} dates, {df['asset'].n_unique()} assets")

# Rank each feature within its date, scaled to [-1, 1]. Nulls go to the middle
# rather than being dropped: dropping a row loses the whole cross-section entry,
# and the middle is the honest "no information" position.
exprs = []
for f in FEATURES:
    r = pl.col(f).rank("average").over("date")
    n = pl.col(f).is_not_null().sum().over("date")
    exprs.append(
        pl.when(pl.col(f).is_null()).then(0.0)
        .otherwise(2.0 * (r - 1) / pl.max_horizontal(n - 1, pl.lit(1)) - 1.0)
        .alias(f"x_{f}")
    )
# Targets are relative to the date's own cross-section - the ranker predicts who
# beats whom, not where the market goes.
exprs.append((pl.col("y1") - pl.col("y1").mean().over("date")).alias("t1"))
exprs.append((pl.col("y7") - pl.col("y7").mean().over("date")).alias("t7"))
df = df.with_columns(exprs)

out = Path(sys.argv[1])
df.write_parquet(out)
print(f"wrote {out}  ({df.width} columns)")
print("features:", ", ".join(FEATURES))