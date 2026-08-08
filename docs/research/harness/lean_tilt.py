"""Tilt sized by how hard the channel is leaning, on the original 144d channel.

Three mechanisms, all reading the SAME 144-day channel the asset selection uses,
differing only in how they turn it into a net exposure:

  state      the incumbent: 3 states from price vs bands, fixed +/-t
  lean       sign AND size from the filter's 20-day slope, clipped
  state_x    sign from the bands, size from |lean| - so the bands decide
             direction and the lean decides conviction

`lean` is the filter's own 20-day fractional change. Its distribution on BTC:
p25 -0.048, median +0.001, p75 +0.068, p98 +0.233 - so a scale near 2 maps a
strong trend to roughly a 0.3 tilt, and the sweep varies that.

Everything else is pinned: selection, costs, funding, shortability, cadence. Cap
is 0.5 throughout because beyond it the book is one-sided and gross would have
to exceed 1.0, which 9.2 forbids.
"""
import json, math, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import backtest, features, ic, store, universe, validate
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT=Path("/home/magnus/dev/magnus/ai-trader"); DATA=ROOT/"data"
STEP, LEAN_N, CAP = 7, 20, 0.5
cfg=Config.load(ROOT/"config"/"default.toml")
bars=store.read(root=DATA, interval_s=cfg.interval_s)
frame=features.build(bars, benchmark=cfg.benchmark)
prices=mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund=pl.read_parquet(DATA/"funding"/"binance_um.parquet")
COST=float(cfg.costs.commission_bps+cfg.costs.spread_bps)
perp_from=dict(fund.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
ftab={(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
      for k,v in fund.partition_by("asset", as_dict=True).items()}

bt=(features.build(bars.filter(pl.col("asset")==cfg.benchmark), benchmark=cfg.benchmark)
    .sort("ts_utc")
    .with_columns((pl.col("gc_filter")/pl.col("gc_filter").shift(LEAN_N)-1).alias("lean")))
T={r["ts_utc"]: r for r in bt.iter_rows(named=True)}

def clip(v, c): return max(-c, min(c, v))

def tilt_for(mode, scale, ts):
    r=T.get(ts)
    if r is None or r["gc_upper"] is None or r["lean"] is None: return 0.0
    lean=r["lean"]
    if mode=="state":
        return scale if r["close"]>r["gc_upper"] else (-scale if r["close"]<r["gc_filter"] else 0.0)
    if mode=="lean":
        return clip(lean*scale, CAP)
    if mode=="state_x":
        sign = 1.0 if r["close"]>r["gc_upper"] else (-1.0 if r["close"]<r["gc_filter"] else 0.0)
        return clip(sign*abs(lean)*scale, CAP)
    raise ValueError(mode)

def held_funding(a,day,days):
    t=ftab.get(a); return 0.0 if not t else sum(t.get(day+timedelta(days=k),0.0) for k in range(days))

def eligible_at(day):
    try: members=universe.load(day, root=DATA)
    except FileNotFoundError: return pl.DataFrame()
    e={m.asset for m in members if m.eligible}
    return frame.filter((pl.col("ts_utc")==day-timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(e))
        & (pl.col("bars_available")>=cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())

def run(mode, scale, S, E):
    eq, curve, prev = 1.0, [], (set(), set())
    day=S
    while day<=E:
        cx=eligible_at(day)
        if cx.is_empty(): day+=timedelta(days=STEP); continue
        ab=cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")
        be=cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")
        L=ab["asset"].to_list()
        Sh=[a for a in be["asset"].to_list() if perp_from.get(a) is not None and perp_from[a]<=day]
        if len(L)<3 or len(Sh)<3: day+=timedelta(days=STEP); continue
        t=tilt_for(mode, scale, day-timedelta(seconds=cfg.interval_s))
        wl, ws = 0.5+t, 0.5-t
        fwd=ic._forward_returns(prices, L+Sh, day, STEP)
        tab=dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr=[tab[a] for a in L if a in tab]; sr=[tab[a] for a in Sh if a in tab]
        if len(lr)<3 or len(sr)<3: day+=timedelta(days=STEP); continue
        g=wl*(sum(lr)/len(lr))-ws*(sum(sr)/len(sr))
        fp=ws*(sum(held_funding(a,day,STEP) for a in Sh)/len(Sh))
        ch=len(set(L)^prev[0])+len(set(Sh)^prev[1])
        turn=min(1.0, ch/(2*(len(L)+len(Sh)))) if curve else 1.0
        prev=(set(L),set(Sh))
        eq*=1+g+fp-turn*COST/10_000
        curve.append([day.date().isoformat(), eq]); day+=timedelta(days=STEP)
    rets=[curve[i][1]/curve[i-1][1]-1 for i in range(1,len(curve))]
    m=sum(rets)/len(rets); sd=math.sqrt(sum((r-m)**2 for r in rets)/(len(rets)-1))
    ppy,yrs=365/STEP,(E-S).days/365
    peak,dd=curve[0][1],0.0
    for _,v in curve: peak=max(peak,v); dd=min(dd,v/peak-1)
    return {"mode":mode,"scale":scale,"net":eq-1,"cagr":eq**(1/yrs)-1,
            "sharpe":(m*ppy)/(sd*math.sqrt(ppy)),"maxdd":dd,"curve":curve}

W={"fresh":(datetime(2019,10,1,tzinfo=timezone.utc),datetime(2021,10,1,tzinfo=timezone.utc)),
   "orig": (datetime(2021,10,1,tzinfo=timezone.utc),datetime(2026,8,1,tzinfo=timezone.utc))}
SCALES=(0.5,1.0,1.5,2.0,3.0,4.0)
out={}
for mode in ("lean","state_x"):
    out[mode]={}
    print(f"\n=== {mode}  (144d channel, cap {CAP}) ===")
    print(f"{'scale':>6}{'fresh Sh':>10}{'fresh DD':>10}{'orig Sh':>10}{'orig DD':>10}"
          f"{'combined':>11}{'min Sh':>8}")
    pts=[]
    for sc in SCALES:
        f=run(mode,sc,*W["fresh"]); o=run(mode,sc,*W["orig"])
        out[mode][sc]={"fresh":f,"orig":o}
        comb=(1+f["net"])*(1+o["net"])-1; ms=min(f["sharpe"],o["sharpe"])
        ok = ms>1.14 and comb>6.586      # beat the 144d incumbent's floor and BTC
        pts.append(validate.SweepPoint(value=sc, metrics=backtest.metrics([],interval_s=86400), holds_up=ok))
        print(f"{sc:>6.1f}{f['sharpe']:>10.2f}{f['maxdd']*100:>9.1f}%{o['sharpe']:>10.2f}"
              f"{o['maxdd']*100:>9.1f}%{comb*100:>10.1f}%{ms:>8.2f}{'  yes' if ok else ''}")
    print(validate.find_plateau(pts, axis=f"{mode}_scale"))
Path(sys.argv[1]).write_bytes((json.dumps({k:{str(s):v for s,v in d.items()} for k,d in out.items()}, indent=2)+"\n").encode())