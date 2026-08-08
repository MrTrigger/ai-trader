"""Two changes, tested one axis at a time.

**1. Breadth (a defect fix, not a search).** When almost nothing is above its
channel the long leg collapses to 3-6 names and 50% of NAV lands on whatever is
left - a concentrated bet arrived at by accident rather than by conviction. The
fix is NOT to hold cash: a thin long leg is itself the signal that everything is
falling, and the short leg is fat (25-32 names), so the shortfall goes there.
Gross stays at 1.0 and nothing is concentrated:

    wl_eff = wl * min(1, len(L) / MIN_NAMES)
    ws_eff = 1 - wl_eff

`MIN_NAMES = 1` reproduces the current behaviour exactly, so the sweep contains
its own control.

**2. Dispersion gate (a new hypothesis, config 89+).** A cross-sectional book
monetises the GAP between winners and losers. In 2025-07..2026-08 that gap fell
from +1.19%/wk to +0.11%/wk while the hit rate stayed at ~57% - the signal kept
picking the right side and the sides stopped being different. Below some
dispersion the expected spread cannot cover a 30%-turnover book's costs, and the
correct position is flat.

Measured point-in-time as the cross-sectional standard deviation of trailing
20-day returns across the eligible universe, expressed as a percentile of its
own trailing two-year history. Nothing about the strategy's own results enters
it - that would be performance-chasing rather than a market-state reading.
"""
from __future__ import annotations

import json, math, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import polars as pl

from planner import backtest, features, ic, store, universe, validate
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT/"data"
STEP, TILT, REGIME_P = 7, 0.15, 48
cfg = Config.load(ROOT/"config"/"default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark)
prices = mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund = pl.read_parquet(DATA/"funding"/"binance_um.parquet")
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
perp_from = dict(fund.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
ftab = {(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k,v in fund.partition_by("asset", as_dict=True).items()}

btc = bars.filter(pl.col("asset")==cfg.benchmark).sort("ts_utc")
_p = pl.col("close").shift(1)
_tr = pl.max_horizontal(pl.col("high")-pl.col("low"), (pl.col("high")-_p).abs(), (pl.col("low")-_p).abs())
_a = features._gc_alpha(REGIME_P, 4)
_ch = (btc.with_columns(features._cascade((pl.col("high")+pl.col("low")+pl.col("close"))/3,_a,4).alias("f"),
                        features._cascade(_tr,_a,4).alias("t"))
       .with_columns((pl.col("f")+pl.col("t")*1.414).alias("u"))
       .with_row_index("i")
       .with_columns(pl.when(pl.col("i")>=REGIME_P).then(pl.col("f")).otherwise(None).alias("f"),
                     pl.when(pl.col("i")>=REGIME_P).then(pl.col("u")).otherwise(None).alias("u"))
       .sort("ts_utc"))
REG = {r["ts_utc"]: r for r in _ch.iter_rows(named=True)}

# --- dispersion, point-in-time ---------------------------------------------
# Cross-sectional std of trailing 20d returns, per bar, over everything that
# was eligible-ish. Computed once over the whole frame; causal because each row
# only uses its own bar's cross-section of trailing returns.
disp = (frame.filter(pl.col("ret_30").is_not_null() & pl.col("adv_quote").is_not_null()
                     & (pl.col("adv_quote") >= float(cfg.min_dollar_volume)))
        .group_by("ts_utc").agg(pl.col("ret_30").std().alias("disp"), pl.len().alias("n"))
        .filter(pl.col("n") >= 10).sort("ts_utc"))
# Percentile against its OWN trailing history only - never the full series.
disp = disp.with_columns(
    pl.col("disp").rolling_map(lambda s: float((s[:-1] < s[-1]).mean()) if len(s) > 30 else 0.5,
                               window_size=730, min_samples=60).alias("pct"))
DISP = {r["ts_utc"]: r["pct"] for r in disp.iter_rows(named=True)}

def held_funding(a, day, days):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day+timedelta(days=k), 0.0) for k in range(days))

def eligible_at(day):
    try: members = universe.load(day, root=DATA)
    except FileNotFoundError: return pl.DataFrame()
    e = {m.asset for m in members if m.eligible}
    return frame.filter((pl.col("ts_utc")==day-timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(e)) & (pl.col("bars_available")>=cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())

def run(S, E, *, min_names=1, disp_floor=None):
    eq, curve, prev_w, stood_down = 1.0, [], {}, 0
    day = S
    while day <= E:
        cx = eligible_at(day)
        if cx.is_empty(): day += timedelta(days=STEP); continue
        L = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")["asset"].to_list()
        Sh = [a for a in cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")["asset"].to_list()
              if perp_from.get(a) is not None and perp_from[a] <= day]
        if len(L) < 3 or len(Sh) < 3: day += timedelta(days=STEP); continue

        hz = day - timedelta(seconds=cfg.interval_s)
        r = REG.get(hz)
        state = "flat"
        if r and r["u"] is not None:
            state = "up" if r["close"] > r["u"] else ("down" if r["close"] < r["f"] else "flat")
        wl = 0.5 + (TILT if state=="up" else (-TILT if state=="down" else 0.0))

        # Breadth: a thin long leg cannot absorb its allocation without becoming
        # a concentrated bet, so the shortfall moves to the (fat) short leg.
        wl *= min(1.0, len(L)/min_names) if min_names > 1 else 1.0
        ws = 1.0 - wl

        flat_week = disp_floor is not None and DISP.get(hz, 0.5) < disp_floor
        if flat_week:
            stood_down += 1
            w = {}
        else:
            w = {a: wl/len(L) for a in L}
            for a in Sh: w[a] = w.get(a, 0.0) - ws/len(Sh)

        fwd = ic._forward_returns(prices, L+Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3: day += timedelta(days=STEP); continue

        g = 0.0 if flat_week else wl*(sum(lr)/len(lr)) - ws*(sum(sr)/len(sr))
        fp = 0.0 if flat_week else ws*(sum(held_funding(a,day,STEP) for a in Sh)/len(Sh))
        turn = sum(abs(w.get(a,0.0)-prev_w.get(a,0.0)) for a in set(w)|set(prev_w))
        prev_w = w
        eq *= 1 + g + fp - turn*COST/10_000
        curve.append([day.date().isoformat(), eq]); day += timedelta(days=STEP)

    rets=[curve[i][1]/curve[i-1][1]-1 for i in range(1,len(curve))]
    m=sum(rets)/len(rets); sd=math.sqrt(sum((x-m)**2 for x in rets)/(len(rets)-1))
    ppy,yrs=365/STEP,(E-S).days/365
    peak,dd=curve[0][1],0.0
    for _,v in curve: peak=max(peak,v); dd=min(dd,v/peak-1)
    return {"net":eq-1,"cagr":eq**(1/yrs)-1,"sharpe":(m*ppy)/(sd*math.sqrt(ppy)),
            "maxdd":dd,"stood_down":stood_down,"n":len(curve),"curve":curve}

U=timezone.utc
W={"fresh":(datetime(2019,10,1,tzinfo=U),datetime(2021,10,1,tzinfo=U)),
   "orig": (datetime(2021,10,1,tzinfo=U),datetime(2026,8,1,tzinfo=U)),
   "late": (datetime(2025,7,1,tzinfo=U),datetime(2026,8,1,tzinfo=U))}

print("=== 1. BREADTH: shortfall to the short leg (min_names=1 is the control) ===")
print(f"{'min_names':>10}{'fresh Sh':>10}{'fresh DD':>10}{'orig Sh':>10}{'orig DD':>10}"
      f"{'late ret':>10}{'combined':>11}{'min Sh':>8}")
base=None; pts=[]
MN=(1,3,5,8,12,16)
for mn in MN:
    f=run(*W["fresh"],min_names=mn); o=run(*W["orig"],min_names=mn); l=run(*W["late"],min_names=mn)
    ms=min(f["sharpe"],o["sharpe"]); comb=(1+f["net"])*(1+o["net"])-1
    if base is None: base=ms
    pts.append(validate.SweepPoint(value=mn, metrics=backtest.metrics([],interval_s=86400), holds_up=ms>base))
    print(f"{mn:>10}{f['sharpe']:>10.2f}{f['maxdd']*100:>9.1f}%{o['sharpe']:>10.2f}"
          f"{o['maxdd']*100:>9.1f}%{l['net']*100:>9.1f}%{comb*100:>10.1f}%{ms:>8.2f}"
          f"{'  <- control' if mn==1 else ''}")
print(validate.find_plateau(pts, axis="min_names"))

print("\n=== 2. DISPERSION GATE: stand down below a percentile of trailing dispersion ===")
print(f"{'floor':>7}{'fresh Sh':>10}{'orig Sh':>10}{'late ret':>10}{'flat wks':>10}"
      f"{'combined':>11}{'min Sh':>8}")
base=None; pts=[]
FL=(0.0,0.10,0.20,0.30,0.40)
for fl in FL:
    d = None if fl==0.0 else fl
    f=run(*W["fresh"],disp_floor=d); o=run(*W["orig"],disp_floor=d); l=run(*W["late"],disp_floor=d)
    ms=min(f["sharpe"],o["sharpe"]); comb=(1+f["net"])*(1+o["net"])-1
    if base is None: base=ms
    pts.append(validate.SweepPoint(value=fl, metrics=backtest.metrics([],interval_s=86400), holds_up=ms>base))
    print(f"{fl:>7.2f}{f['sharpe']:>10.2f}{o['sharpe']:>10.2f}{l['net']*100:>9.1f}%"
          f"{o['stood_down']:>10}{comb*100:>10.1f}%{ms:>8.2f}{'  <- control' if fl==0.0 else ''}")
print(validate.find_plateau(pts, axis="dispersion_floor"))
