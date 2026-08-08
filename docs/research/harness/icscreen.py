"""Do any features predict the cross-section at all?

A model combines features; it cannot invent signal that is not in them. Before
spending degrees of freedom on a learned ranker it is worth knowing whether the
inputs carry anything, and the information coefficient answers that far more
cheaply than a backtest - one observation per asset per week rather than one per
week (spec 7.5).

Every feature already computed is screened at the 7-day horizon, plus a few
cheap derived ones that cost nothing to add. The t-stat is deflated for
cross-sectional correlation by counting PERIODS rather than observations: within
one week the names move together, so treating each as independent would inflate
every t here by roughly sqrt(28).
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
STEP = 7
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, perp_listed_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}

BASE = ["ret_7", "ret_30", "ret_90", "ret_30_skip_7", "vol_30", "adv_quote",
        "beta_bench", "gc_breakout_age"]
DERIVED = ["dist_upper", "dist_lower", "band_width", "gc_slope", "funding_7d",
           "ret_7_rev", "vol_ratio"]

per_period = {f: [] for f in BASE + DERIVED}
U = timezone.utc
day, END = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
while day <= END:
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

    rows = [r for r in cx.iter_rows(named=True) if tab.get(r["asset"]) is not None]
    if len(rows) < 10:
        day += timedelta(days=STEP); continue
    rets = [tab[r["asset"]] for r in rows]

    def val(r, f):
        up, lo, cl = r["gc_upper"], r["gc_lower"], r["close"]
        if f == "dist_upper": return (cl - up) / abs(up) if up else None
        if f == "dist_lower": return (cl - lo) / abs(lo) if lo else None
        if f == "band_width": return (up - lo) / cl if cl else None
        if f == "gc_slope": return r.get("gc_regime_slope")
        if f == "funding_7d":
            t = ftab.get(r["asset"])
            return None if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(7))
        if f == "ret_7_rev": return -r["ret_7"] if r["ret_7"] is not None else None
        if f == "vol_ratio":
            return r["vol_30"] / r["ret_90"] if r.get("ret_90") else None
        return r.get(f)

    for f in BASE + DERIVED:
        xs, ys = [], []
        for r, ret in zip(rows, rets):
            v = val(r, f)
            if v is not None:
                xs.append(float(v)); ys.append(ret)
        if len(xs) >= 10:
            rho = ic.spearman(xs, ys)
            if rho is not None:
                per_period[f].append(rho)
    day += timedelta(days=STEP)

print(f"{'feature':<18}{'periods':>9}{'mean IC':>10}{'sd':>8}{'t':>8}   verdict")
out = []
for f in BASE + DERIVED:
    v = per_period[f]
    if len(v) < 30:
        continue
    m, sd = statistics.mean(v), statistics.stdev(v)
    t = m / (sd / math.sqrt(len(v)))
    out.append((abs(t), f, len(v), m, sd, t))
for _, f, n, m, sd, t in sorted(out, reverse=True):
    verdict = "SIGNAL" if abs(t) > 3 else ("weak" if abs(t) > 2 else "noise")
    print(f"{f:<18}{n:>9}{m:>+10.4f}{sd:>8.3f}{t:>+8.2f}   {verdict}")
print("\n|t| > 3 is the bar worth acting on; 2-3 is suggestive on one screen of 15.")