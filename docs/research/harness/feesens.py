"""How much does the fee assumption actually decide?

The cost model charges a flat 7bps (5 commission + 2 half-spread) on every unit
of turnover, with no venue fee tier, no maker/taker split, and no distinction
between the spot long leg and the perpetual short leg. Two of those omissions
point in opposite directions:

  tiers        30-day volume moves a taker into a cheaper band, so a high-
               turnover book partly subsidises its own fees. A flat rate makes
               high turnover look worse than it is.
  leg mismatch spot taker fees are materially above perp taker fees, and 5bps is
               roughly a PERP rate. If so the long leg is under-charged and the
               whole book looks cheaper than it is.

Rather than guess which dominates, record the ledger once - gross P&L, funding
and turnover per week - then re-price it at every fee level. Cost enters
linearly in turnover, so one pass answers the whole sweep exactly.

The number that matters is the BREAK-EVEN: the fee at which the strategy stops
being worth running. If that is far above any plausible schedule the modelling
gap is academic; if it is close, the schedules have to be looked up properly.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure)
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
CAP, SCALE, MIN_LEG = 0.5, 8.0, 3
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
_C = {}


def legs(day):
    k = day.date()
    if k in _C: return _C[k]
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
    _C[k] = (L, Sh); return L, Sh


def hf(a, day, n):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))


def ledger(S, E, step):
    """Per period: gross P&L before costs, funding, turnover on the LONG and
    SHORT legs separately, so the two can be priced at different rates."""
    prev, out = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
        L, Sh = legs(day)
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < MIN_LEG or len(Sh) < MIN_LEG:
            tl = sum(abs(v) for v in prev.values() if v > 0)
            ts = sum(abs(v) for v in prev.values() if v < 0)
            prev = {}
            out.append((day, 0.0, 0.0, tl, ts)); day += timedelta(days=step); continue
        if len(L) + len(Sh) > MAXCOUNT:
            nl = max(MIN_LEG, min(len(L), round(MAXCOUNT * wl)))
            ns = max(MIN_LEG, min(len(Sh), MAXCOUNT - nl))
            L, Sh = L[:nl], Sh[:ns]
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, step)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < MIN_LEG or len(sr) < MIN_LEG:
            out.append((day, 0.0, 0.0, 0.0, 0.0)); day += timedelta(days=step); continue
        g = GROSS * wl * (sum(lr)/len(lr)) - GROSS * ws * (sum(sr)/len(sr))
        fp = GROSS * ws * (sum(hf(a, day, step) for a in Sh) / len(Sh))
        w = {a: GROSS * wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * ws / len(Sh)
        keys = set(w) | set(prev)
        tl = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in keys
                 if w.get(a, 0.0) > 0 or prev.get(a, 0.0) > 0)
        ts = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in keys
                 if w.get(a, 0.0) < 0 or prev.get(a, 0.0) < 0)
        prev = w
        out.append((day, g, fp, tl, ts)); day += timedelta(days=step))
    return out