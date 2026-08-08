"""What does the COMBINED tranched book actually hold?

Each tranche is a legal plan: <=12 names, <=25% a name, gross <=0.80. The risk
gate approves them one at a time. But the portfolio a person actually holds is
the union of seven overlapping tranches, and the gate never sees that. If the
tranches pick different names the union could be 84 positions; if they pick the
same names the union is ~12 and the only thing that differs is entry timing.

Measured on real dates: distinct names held, and the largest combined weight in
any single name once the seven books are summed.
"""
import statistics
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, store, universe
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure)
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
CAP, SCALE, MIN_LEG, STEP = 0.5, 8.0, 3, 7
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}


def book_on(day):
    """The weights one tranche holds if it rebalanced on `day`."""
    hz = day - timedelta(seconds=cfg.interval_s)
    b = BENCH.get(hz); t = 0.0
    if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
        sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
        t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
    try:
        members = universe.load(day, root=DATA)
    except FileNotFoundError:
        return {}
    e = {m.asset for m in members if m.eligible}
    cx = frame.filter((pl.col("ts_utc") == hz) & pl.col("asset").is_in(list(e))
        & (pl.col("bars_available") >= cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30") >= float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())
    L = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("gc_breakout_age")["asset"].to_list()
    Sh = (cx.filter(pl.col("gc_breakout_age").is_null() & pl.col("shortable"))
          .with_columns(((pl.col("close") - pl.col("gc_lower")) / pl.col("gc_lower")).alias("_d"))
          .sort("_d"))["asset"].to_list()
    if len(L) < MIN_LEG or len(Sh) < MIN_LEG:
        return {}
    wl, ws = 0.5 + t, 0.5 - t
    if len(L) + len(Sh) > MAXCOUNT:
        nl = max(MIN_LEG, min(len(L), round(MAXCOUNT * wl)))
        ns = max(MIN_LEG, min(len(Sh), MAXCOUNT - nl))
        L, Sh = L[:nl], Sh[:ns]
    if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
    if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
    w = {a: GROSS * wl / len(L) for a in L}
    for a in Sh: w[a] = w.get(a, 0.0) - GROSS * ws / len(Sh)
    return w


U = timezone.utc
day = datetime(2022, 1, 5, tzinfo=U)
END = datetime(2026, 8, 1, tzinfo=U)
per_tranche, union_names, union_max, conflicts = [], [], [], 0
samples = 0
while day <= END:
    # On any given day the seven tranches last rebalanced on the seven
    # preceding days, so the live book is the sum of those seven.
    combined = defaultdict(float)
    live = 0
    for k in range(7):
        w = book_on(day - timedelta(days=k))
        if w: live += 1
        for a, v in w.items():
            combined[a] += v / 7.0
    if live == 0:
        day += timedelta(days=28); continue
    per_tranche.append(live)
    union_names.append(len(combined))
    if combined:
        union_max.append(max(abs(v) for v in combined.values()))
    # A name held long by one tranche and short by another nets down, but the
    # two orders were still both sent.
    signs = defaultdict(set)
    for k in range(7):
        for a, v in book_on(day - timedelta(days=k)).items():
            signs[a].add(1 if v > 0 else -1)
    conflicts += sum(1 for a, s in signs.items() if len(s) > 1)
    samples += 1
    day += timedelta(days=28)

print(f"{samples} sample dates, 2022-01 .. 2026-08\n")
print(f"live tranches      median {statistics.median(per_tranche):.0f} of 7")
print(f"distinct names     median {statistics.median(union_names):.0f}   "
      f"min {min(union_names)}   max {max(union_names)}")
print(f"                   (one tranche is capped at {MAXCOUNT})")
print(f"largest combined   median {statistics.median(union_max)*100:.1f}%   "
      f"max {max(union_max)*100:.1f}%   (cap is {MAXPOS*100:.0f}% per tranche)")
print(f"\nnames held long by one tranche and short by another: "
      f"{conflicts} across {samples} dates ({conflicts/samples:.1f} per date)")