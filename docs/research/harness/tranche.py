"""What if we hold all seven phases at once instead of picking one?

Rebalancing weekly means choosing one of seven decision grids and discarding
six. The choice is arbitrary, and it turned out to matter more than any
parameter in the strategy. Holding all seven simultaneously - seven sub-books,
each rebalancing weekly on its own day, each with a seventh of the capital -
removes the choice entirely rather than justifying it.

This is not a smoothing trick. Each sub-book is the same strategy with the same
weekly holding period and the same turnover, so per-tranche costs are unchanged;
only the arbitrary alignment goes away. The expected return is the AVERAGE of
the seven phases, which is lower than Friday and higher than Tuesday, and the
variance falls by however much the sub-books fail to move together.

Measured here from the seven return series, binned to a common calendar week so
they can be correlated, then combined as an equal-weight portfolio of tranches.
"""
import math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
STEP = 7
cfg = Config.load(ROOT / "config" / "default.toml")
MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
CAP, SCALE = 0.5, 8.0
DAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def run(S, E):
    """(date, return) per rebalance for one phase."""
    prev, rows = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        try:
            members = universe.load(day, root=DATA)
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
        except FileNotFoundError:
            L = Sh = []
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < 3 or len(Sh) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue
        g = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        w = {a: wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rows.append((day, g + fp - turn * COST / 10_000))
        day += timedelta(days=STEP)
    return rows


U = timezone.utc
BO, END = datetime(2021, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
series = {}
for k in range(7):
    rows = run(BO + timedelta(days=k), END)
    # Bin to ISO week so the seven grids become comparable.
    series[k] = {}
    for d, r in rows:
        series[k][d.isocalendar()[:2]] = r

weeks = sorted(set.intersection(*(set(s) for s in series.values())))
print(f"{len(weeks)} calendar weeks common to all seven phases\n")

mat = [[series[k][w] for w in weeks] for k in range(7)]
means = [statistics.mean(row) for row in mat]
sds = [statistics.stdev(row) for row in mat]

print("correlation between phases (weekly returns, common weeks):")
print("       " + "".join(f"{DAYS[(BO+timedelta(days=j)).weekday()]:>7}" for j in range(7)))
cors = []
for i in range(7):
    line = f"  {DAYS[(BO+timedelta(days=i)).weekday()]:<5}"
    for j in range(7):
        c = statistics.correlation(mat[i], mat[j])
        line += f"{c:>7.2f}"
        if i < j: cors.append(c)
    print(line)
print(f"\n  mean off-diagonal correlation: {statistics.mean(cors):.3f}")

# Equal-weight portfolio of the seven tranches.
tranched = [statistics.mean(mat[k][t] for k in range(7)) for t in range(len(weeks))]
tm, tsd = statistics.mean(tranched), statistics.stdev(tranched)
eq = 1.0
for r in tranched: eq *= 1 + r

print()
print(f"{'':<12}{'mean wk':>10}{'sd wk':>9}{'Sharpe':>9}{'compounded':>13}")
for k in range(7):
    e = 1.0
    for r in mat[k]: e *= 1 + r
    sh = (means[k] * 52) / (sds[k] * math.sqrt(52))
    print(f"  {DAYS[(BO+timedelta(days=k)).weekday()]:<10}{means[k]*100:>9.3f}%{sds[k]*100:>8.2f}%"
          f"{sh:>9.2f}{(e-1)*100:>12.1f}%")
tsh = (tm * 52) / (tsd * math.sqrt(52))
print(f"  {'TRANCHED':<10}{tm*100:>9.3f}%{tsd*100:>8.2f}%{tsh:>9.2f}{(eq-1)*100:>12.1f}%")
print()
print(f"  single-phase Sharpe: median {statistics.median((means[k]*52)/(sds[k]*math.sqrt(52)) for k in range(7)):.2f}"
      f"   range {min((means[k]*52)/(sds[k]*math.sqrt(52)) for k in range(7)):.2f}"
      f" .. {max((means[k]*52)/(sds[k]*math.sqrt(52)) for k in range(7)):.2f}")
print(f"  tranched Sharpe    : {tsh:.2f}  (one number, no phase to choose)")
print(f"  vol reduction      : {(1 - tsd/statistics.mean(sds))*100:.1f}% vs the average single phase")