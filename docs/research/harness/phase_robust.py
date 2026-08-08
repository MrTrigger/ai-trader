"""Every candidate, measured across all seven rebalance phases.

A weekly strategy has seven equally valid start days and the choice between them
is arbitrary. Reporting one is reporting one draw from a distribution and calling
it the answer. The median across phases is the estimate; the spread is the
uncertainty, and for this strategy the spread is most of the story.

The equity curve shown is the MEDIAN phase, not the best one, so the picture and
the number describe the same thing.
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
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_filter", "gc_upper", "gc_regime_filter",
                  "gc_regime_upper", "gc_regime_slope"]).iter_rows(named=True)}
BTC_PX = {r["ts_utc"]: r["mark_open"] for r in
          prices.filter(pl.col("asset") == cfg.benchmark).iter_rows(named=True)}
CAP, SCALE = 0.5, 8.0


def tilt_for(mode, hz):
    b = BENCH.get(hz)
    if b is None or mode == "neutral": return 0.0
    if mode == "slow144":
        if b["gc_upper"] is None: return 0.0
        return 0.15 if b["close"] > b["gc_upper"] else (-0.15 if b["close"] < b["gc_filter"] else 0.0)
    if b["gc_regime_upper"] is None: return 0.0
    up, dn = b["close"] > b["gc_regime_upper"], b["close"] < b["gc_regime_filter"]
    if mode == "fixed48": return 0.15 if up else (-0.15 if dn else 0.0)
    if mode == "fast48":
        if b["gc_regime_slope"] is None: return 0.0
        s = 1.0 if up else (-1.0 if dn else 0.0)
        return max(-CAP, min(CAP, s * abs(b["gc_regime_slope"]) * SCALE))
    return 0.0


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def weekly(S, E, mode, truncate):
    prev, rows = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        if mode is None:
            px = BTC_PX.get(day)
            prev_px = prev.get("px")
            rows.append([day.date().isoformat(), 0.0 if (prev_px is None or px is None) else px / prev_px - 1])
            if px is not None: prev = {"px": px}
            day += timedelta(days=STEP); continue
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
        t = tilt_for(mode, hz); wl, ws = 0.5 + t, 0.5 - t
        if len(L) < 3 or len(Sh) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append([day.date().isoformat(), -turn * COST / 10_000]); day += timedelta(days=STEP); continue
        if truncate and len(L) + len(Sh) > MAXCOUNT:
            nl = max(3, min(len(L), round(MAXCOUNT * wl))); ns = max(3, min(len(Sh), MAXCOUNT - nl))
            L, Sh = L[:nl], Sh[:ns]
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append([day.date().isoformat(), -turn * COST / 10_000]); day += timedelta(days=STEP); continue
        g = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        w = {a: wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rows.append([day.date().isoformat(), g + fp - turn * COST / 10_000])
        day += timedelta(days=STEP)
    return rows


def stats(rows):
    eq, curve, simple, scurve, pk, dd = 1.0, [], 0.0, [], 1.0, 0.0
    for d, r in rows:
        eq *= 1 + r; simple += r
        pk = max(pk, eq); dd = min(dd, eq / pk - 1)
        curve.append([d, eq]); scurve.append([d, 1.0 + simple])
    rets = [r for _, r in rows]
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"final": eq - 1, "sharpe": (m * 52) / (sd * math.sqrt(52)) if sd else 0.0,
            "maxdd": dd, "n": len(curve), "curve": curve, "simple_curve": scurve,
            "simple_final": simple}


U = timezone.utc
BF, BO, END = datetime(2019, 10, 1, tzinfo=U), datetime(2021, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
SPECS = [("buy & hold BTC", None, False), ("market-neutral", "neutral", False),
         ("+ regime tilt 0.15, 144d read", "slow144", False), ("+ regime read at 48d", "fixed48", False),
         ("+ lean-sized tilt, scale 8", "fast48", False), ("as shipped (12 names, capped)", "fast48", True)]

out = {}
print(f"{'candidate':<32}{'fresh med':>11}{'orig med':>11}{'Sh med':>8}"
      f"{'Sh min':>8}{'Sh max':>8}{'orig min':>11}{'orig max':>11}")
for label, mode, trunc in SPECS:
    phases = []
    for k in range(7):
        f = stats(weekly(BF + timedelta(days=k), BO, mode, trunc))
        o = stats(weekly(BO + timedelta(days=k), END, mode, trunc))
        phases.append({"offset": k, "fresh": f, "orig": o})
    fs = sorted(p["fresh"]["final"] for p in phases)
    os_ = sorted(p["orig"]["final"] for p in phases)
    sh = sorted(p["orig"]["sharpe"] for p in phases)
    med_off = sorted(phases, key=lambda p: p["orig"]["final"])[3]["offset"]
    out[label] = {"phases": phases, "median_offset": med_off,
                  "fresh_median": fs[3], "orig_median": os_[3],
                  "sharpe_median": sh[3], "sharpe_min": sh[0], "sharpe_max": sh[-1],
                  "orig_min": os_[0], "orig_max": os_[-1],
                  "fresh_min": fs[0], "fresh_max": fs[-1]}
    print(f"{label:<32}{fs[3]*100:>10.1f}%{os_[3]*100:>10.1f}%{sh[3]:>8.2f}"
          f"{sh[0]:>8.2f}{sh[-1]:>8.2f}{os_[0]*100:>10.1f}%{os_[-1]*100:>10.1f}%")
    sys.stdout.flush()

Path(sys.argv[1]).write_bytes((json.dumps(out, indent=2) + "\n").encode())
print("WROTE", sys.argv[1])