"""Fast features, built from the data actually held, and screened.

The existing set is fourteen slow aggregates and two fast ones, which is why the
sequence-structure question could not be answered from it. Everything below
changes materially day to day and is derivable from daily OHLCV plus funding -
no new data source required.

What is NOT here, and cannot be, matters just as much: order book (spread, depth,
imbalance), trade prints, open interest, and any intraday bar. Those are where
genuinely short-horizon signal lives, and the store has daily OHLCV only. So this
screen establishes whether fast structure exists in what we HAVE, not whether it
exists.
"""
import math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s).sort(["asset", "ts_utc"])

pc = pl.col("close").shift(1).over("asset")
pv = pl.col("quote_volume").shift(1).over("asset")
tr = pl.max_horizontal(pl.col("high") - pl.col("low"),
                       (pl.col("high") - pc).abs(), (pl.col("low") - pc).abs())

fast = bars.with_columns([
    # Where today opened relative to yesterday's close - overnight repricing.
    ((pl.col("open") - pc) / pc).alias("f_gap"),
    # How wide the day was, scaled by price.
    ((pl.col("high") - pl.col("low")) / pl.col("close")).alias("f_range"),
    # Where the close sat inside the day's range. 1 = closed on the high.
    pl.when(pl.col("high") > pl.col("low"))
      .then((pl.col("close") - pl.col("low")) / (pl.col("high") - pl.col("low")))
      .otherwise(0.5).alias("f_clv"),
    # Today's turnover against its own recent normal - a volume surprise.
    (pl.col("quote_volume") /
     pl.col("quote_volume").rolling_median(20, min_samples=10).over("asset")).alias("f_volsurp"),
    # Today's true range against its recent normal - a volatility surprise.
    (tr / tr.rolling_mean(20, min_samples=10).over("asset")).alias("f_trsurp"),
    # Amihud illiquidity: how much the price moved per dollar traded.
    ((pl.col("close") / pc - 1).abs() / pl.col("quote_volume")).alias("f_amihud"),
    # Two- and three-day returns, short enough to still vary daily.
    (pl.col("close") / pl.col("close").shift(2).over("asset") - 1).alias("f_ret2"),
    (pl.col("close") / pl.col("close").shift(3).over("asset") - 1).alias("f_ret3"),
    # Acceleration: is the recent move speeding up or fading?
    ((pl.col("close") / pc - 1) -
     (pl.col("close").shift(1).over("asset") / pl.col("close").shift(2).over("asset") - 1)
     ).alias("f_accel"),
    # Turnover change, a cruder liquidity impulse than the surprise ratio.
    (pl.col("quote_volume") / pv - 1).alias("f_dvol"),
])

FAST = ["f_gap", "f_range", "f_clv", "f_volsurp", "f_trsurp", "f_amihud",
        "f_ret2", "f_ret3", "f_accel", "f_dvol"]

print("lag-1 autocorrelation of the new features")
for f in FAST:
    sub = fast.select(["asset", f]).drop_nulls()
    lag = sub.with_columns(pl.col(f).shift(1).over("asset").alias("_l")).drop_nulls()
    a, b = lag[f].to_list(), lag["_l"].to_list()
    ma, mb = statistics.mean(a), statistics.mean(b)
    sa, sb = statistics.pstdev(a), statistics.pstdev(b)
    rho = sum((x-ma)*(y-mb) for x, y in zip(a, b)) / len(a) / (sa*sb) if sa and sb else 0
    print(f"  {f:<12}{rho:>9.4f}")

# --- IC screen, 1-day horizon, the horizon a fast feature would serve --------
frame = features.build(bars, benchmark=cfg.benchmark, perp_listed_from=borrow.listings(root=DATA))
frame = frame.join(fast.select(["asset", "ts_utc"] + FAST), on=["asset", "ts_utc"], how="left")
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])

U = timezone.utc
per = {f: [] for f in FAST}
day, END = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
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
        & pl.col("perp_listed"))
    if cx.height < 12:
        day += timedelta(days=1); continue
    fwd = ic._forward_returns(prices, cx["asset"].to_list(), day, 1)
    tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
    rows = [r for r in cx.iter_rows(named=True) if tab.get(r["asset"]) is not None]
    if len(rows) < 12:
        day += timedelta(days=1); continue
    for f in FAST:
        xs = [(r[f], tab[r["asset"]]) for r in rows if r.get(f) is not None]
        if len(xs) >= 12:
            rho = ic.spearman([a for a, _ in xs], [b for _, b in xs])
            if rho is not None:
                per[f].append(rho)
    day += timedelta(days=1)

print(f"\nIC at the 1-DAY horizon (non-overlapping, so t is honest)")
print(f"{'feature':<12}{'periods':>9}{'mean IC':>10}{'t':>9}   verdict")
res = []
for f in FAST:
    v = per[f]
    if len(v) < 100: continue
    m, sd = statistics.mean(v), statistics.stdev(v)
    t = m / (sd / math.sqrt(len(v)))
    res.append((abs(t), f, len(v), m, t))
for _, f, n, m, t in sorted(res, reverse=True):
    verdict = "SIGNAL" if abs(t) > 3 else ("weak" if abs(t) > 2 else "noise")
    print(f"{f:<12}{n:>9}{m:>+10.4f}{t:>+9.2f}   {verdict}")