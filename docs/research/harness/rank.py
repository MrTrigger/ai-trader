"""Does the leaderboard ordering carry any information?

"Hold the top X regardless of side" only beats "hold everything that qualifies"
if position 1 is genuinely better than position 8. The current rankings -
breakout recency on the long side, distance below the lower band on the short
side - were invented to fit a twelve-position cap, not because anything measured
said they predicted returns. If they are noise, concentrating on the top of the
list adds variance and subtracts diversification for nothing.

Tested directly: for every rebalance date, sort each leg by its own ranking, cut
into quintiles, and measure the forward 7-day return of each. A real ordering
shows monotone quintiles. Noise shows a flat line with a large spread.

Also tests the ranking the proposal actually implies - |distance from the
channel| across BOTH sides at once, which is the closest thing this signal has
to a conviction score.
"""
import math, statistics, sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
STEP = 7
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, perp_listed_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])

U = timezone.utc
S, E = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)

# (rank value, forward return) pairs, per ranking scheme.
long_recency, short_depth, conviction = [], [], []
day = S
while day <= E:
    hz = day - timedelta(seconds=cfg.interval_s)
    try:
        members = universe.load(day, root=DATA)
    except FileNotFoundError:
        day += timedelta(days=STEP); continue
    e = {m.asset for m in members if m.eligible}
    cx = frame.filter((pl.col("ts_utc") == hz) & pl.col("asset").is_in(list(e))
        & (pl.col("bars_available") >= cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30") >= float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null() & pl.col("perp_listed"))
    if cx.height < 10:
        day += timedelta(days=STEP); continue
    fwd = ic._forward_returns(prices, cx["asset"].to_list(), day, STEP)
    tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))

    for r in cx.iter_rows(named=True):
        a = r["asset"]
        if a not in tab or tab[a] is None:
            continue
        ret = tab[a]
        up, lo, cl = r["gc_upper"], r["gc_lower"], r["close"]
        if r["gc_breakout_age"] is not None:
            long_recency.append((int(r["gc_breakout_age"]), ret))
            # Conviction: how far ABOVE the upper band, as a fraction of it.
            conviction.append(((cl - up) / abs(up), ret, +1))
        else:
            if lo:
                short_depth.append(((cl - lo) / abs(lo), ret))
                conviction.append(((cl - lo) / abs(lo) - 1.0, ret, -1))
    day += timedelta(days=STEP)


def quintiles(pairs, label, reverse=False):
    """Sort by rank value and report the forward return of each fifth."""
    pairs = sorted(pairs, key=lambda p: p[0], reverse=reverse)
    n = len(pairs) // 5
    print(f"\n{label}   ({len(pairs):,} observations)")
    print(f"  {'quintile':<22}{'rank value':>14}{'fwd 7d return':>16}")
    means = []
    for i, name in enumerate(("Q1 (top of list)", "Q2", "Q3", "Q4", "Q5 (bottom)")):
        block = pairs[i*n:(i+1)*n] if i < 4 else pairs[4*n:]
        m = statistics.mean(r for _, r, *_ in block)
        means.append(m)
        print(f"  {name:<22}{statistics.median(v for v, *_ in block):>14.3f}{m*100:>15.2f}%")
    spread = means[0] - means[-1]
    allr = [r for _, r, *_ in pairs]
    sd = statistics.pstdev(allr)
    se = sd * math.sqrt(2 / n)
    print(f"  Q1 - Q5 spread      {spread*100:+.2f}%   se {se*100:.2f}%   t {spread/se:+.2f}")
    return spread / se


t1 = quintiles(long_recency, "LONG leg, ranked by breakout recency (freshest first)")
t2 = quintiles(short_depth, "SHORT leg, ranked by distance below lower band (deepest first)")
t3 = quintiles([(abs(v), r * s) for v, r, s in conviction],
               "BOTH sides, ranked by |distance from band| (signed return)", reverse=True)

print("\n" + "=" * 64)
print("A |t| above 2 would mean the ordering predicts something.")
for name, t in (("long recency", t1), ("short depth", t2), ("conviction", t3)):
    print(f"  {name:<16} t = {t:+.2f}   {'informative' if abs(t) > 2 else 'NOT distinguishable from noise'}")