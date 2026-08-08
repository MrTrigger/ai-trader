"""Regenerate every long/short series on a COMPLETE weekly grid.

The recorded curves skipped stand-down weeks entirely, which produced three
separate errors: gaps the chart drew straight lines across, Sharpe annualised by
52 against ~36 traded weeks, and - worst - a "buy & hold BTC" benchmark sampled
on the same gappy grid, which is not buy-and-hold at all.

Every week in the span now appears in every series. A week the book cannot form
two legs is a real week with a real return, and that return is not zero: the
pipeline's targets go empty, the diff emits exits, and the book PAYS to
liquidate. Modelling it as a costless pause would flatter exactly the 31% of
weeks the strategy spends flat.

BTC is priced every week unconditionally, because a benchmark that stops
compounding when the strategy stands down is not a benchmark.
"""
import json, math, statistics, sys
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
MAXCOUNT = cfg.limits.max_position_count
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)

bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_filter", "gc_upper",
                  "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"]).iter_rows(named=True)}
BTC_PX = {r["ts_utc"]: r["mark_open"] for r in
          prices.filter(pl.col("asset") == cfg.benchmark).iter_rows(named=True)}

CAP, SCALE = 0.5, 8.0


def tilt_for(mode, hz):
    b = BENCH.get(hz)
    if b is None:
        return 0.0
    if mode == "neutral":
        return 0.0
    if mode == "slow144":
        if b["gc_upper"] is None:
            return 0.0
        if b["close"] > b["gc_upper"]:
            return 0.15
        if b["close"] < b["gc_filter"]:
            return -0.15
        return 0.0
    if b["gc_regime_upper"] is None:
        return 0.0
    up = b["close"] > b["gc_regime_upper"]
    dn = b["close"] < b["gc_regime_filter"]
    if mode == "fixed48":
        return 0.15 if up else (-0.15 if dn else 0.0)
    if mode == "fast48":
        if b["gc_regime_slope"] is None:
            return 0.0
        sign = 1.0 if up else (-1.0 if dn else 0.0)
        return max(-CAP, min(CAP, sign * abs(b["gc_regime_slope"]) * SCALE))
    return 0.0


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def grid(S, E):
    d, out = S, []
    while d <= E:
        out.append(d); d += timedelta(days=STEP)
    return out


def run(S, E, mode, *, truncate=False):
    """Weekly returns over the FULL grid. Flat weeks included, and charged."""
    prev, rows = {}, []
    for day in grid(S, E):
        hz = day - timedelta(seconds=cfg.interval_s)
        L = Sh = []
        try:
            members = universe.load(day, root=DATA)
            e = {m.asset for m in members if m.eligible}
            cx = frame.filter((pl.col("ts_utc") == hz) & pl.col("asset").is_in(list(e))
                & (pl.col("bars_available") >= cfg.min_history_bars)
                & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
                & pl.col("vol_30").is_not_null() & (pl.col("vol_30") >= float(cfg.min_volatility))
                & pl.col("gc_upper").is_not_null())
            Ld = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("gc_breakout_age")
            Sd = (cx.filter(pl.col("gc_breakout_age").is_null() & pl.col("shortable"))
                  .with_columns(((pl.col("close") - pl.col("gc_lower")) / pl.col("gc_lower")).alias("_d"))
                  .sort("_d"))
            L, Sh = Ld["asset"].to_list(), Sd["asset"].to_list()
        except FileNotFoundError:
            pass

        t = tilt_for(mode, hz)
        wl, ws = 0.5 + t, 0.5 - t

        if len(L) < 3 or len(Sh) < 3:
            # Stand down. Targets go empty, so the book liquidates and pays for it.
            turn = sum(abs(v) for v in prev.values())
            prev = {}
            rows.append([day.date().isoformat(), -turn * COST / 10_000])
            continue

        if truncate and len(L) + len(Sh) > MAXCOUNT:
            nl = max(3, min(len(L), round(MAXCOUNT * wl)))
            ns = max(3, min(len(Sh), MAXCOUNT - nl))
            L, Sh = L[:nl], Sh[:ns]
        if wl / len(L) > MAXPOS:
            wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS:
            ws = MAXPOS * len(Sh); wl = 1.0 - ws

        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append([day.date().isoformat(), -turn * COST / 10_000]); continue

        g = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        w = {a: wl / len(L) for a in L}
        for a in Sh:
            w[a] = w.get(a, 0.0) - ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        rows.append([day.date().isoformat(), g + fp - turn * COST / 10_000])
    return rows


def btc(S, E):
    out, last = [], None
    for day in grid(S, E):
        hz_open = BTC_PX.get(day)
        r = 0.0 if (last is None or hz_open is None) else hz_open / last - 1
        if hz_open is not None:
            last = hz_open
        out.append([day.date().isoformat(), r])
    return out


def stats(weekly):
    eq, curve, simple, scurve = 1.0, [], 0.0, []
    for d, r in weekly:
        eq *= 1 + r; simple += r
        curve.append([d, eq]); scurve.append([d, 1.0 + simple])
    rets = [r for _, r in weekly]
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    pk, dd = curve[0][1], 0.0
    for _, v in curve:
        pk = max(pk, v); dd = min(dd, v / pk - 1)
    return {"final": eq - 1, "sharpe": (m * 52) / (sd * math.sqrt(52)) if sd else 0.0,
            "maxdd": dd, "n": len(curve), "curve": curve, "simple_curve": scurve,
            "simple_final": simple, "weekly": weekly}


U = timezone.utc
FRESH = (datetime(2019, 10, 1, tzinfo=U), datetime(2021, 10, 1, tzinfo=U))
ORIG = (datetime(2021, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U))
FULL = (FRESH[0], ORIG[1])

SPECS = [
    ("buy & hold BTC", None, False),
    ("market-neutral", "neutral", False),
    ("+ regime tilt 0.15, 144d read", "slow144", False),
    ("+ regime read at 48d", "fixed48", False),
    ("+ lean-sized tilt, scale 8", "fast48", False),
    ("as shipped (12 names, capped)", "fast48", True),
]

out = {}
for label, mode, trunc in SPECS:
    per = {}
    for wname, (S, E) in (("fresh", FRESH), ("orig", ORIG), ("full", FULL)):
        weekly = btc(S, E) if mode is None else run(S, E, mode, truncate=trunc)
        per[wname] = stats(weekly)
    out[label] = per
    print(f"{label:<32} fresh {per['fresh']['final']*100:>8.1f}% Sh {per['fresh']['sharpe']:>5.2f}"
          f" | orig {per['orig']['final']*100:>9.1f}% Sh {per['orig']['sharpe']:>5.2f}"
          f" | full {per['full']['final']*100:>10.1f}% dd {per['full']['maxdd']*100:>6.1f}%"
          f" | n {per['full']['n']}")
    sys.stdout.flush()

Path(sys.argv[1]).write_bytes((json.dumps(out, indent=2) + "\n").encode())
print("WROTE", sys.argv[1])