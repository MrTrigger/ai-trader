"""Why does weighting by breadth hurt? Measure it, don't infer it from the curve.

Two questions, both answerable from the ledger:

  1. Where is the concentration actually? The worry is "few names on one side ->
     huge per-name bets". Which side does that land on, and how often?

  2. Is breadth informative about the FORWARD move at all? If a wide long leg
     predicted a good week, breadth-weighting would work. Rank-correlate the
     long share against next week's net return and see.
"""
import math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT=Path("/home/magnus/dev/magnus/ai-trader"); DATA=ROOT/"data"
STEP,CAP,LEAN_N,SCALE,RP=7,0.5,20,8.0,48
cfg=Config.load(ROOT/"config"/"default.toml")
bars=store.read(root=DATA, interval_s=cfg.interval_s)
frame=features.build(bars, benchmark=cfg.benchmark)
prices=mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund=pl.read_parquet(DATA/"funding"/"binance_um.parquet")
perp_from=dict(fund.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
btc=bars.filter(pl.col("asset")==cfg.benchmark).sort("ts_utc")
_p=pl.col("close").shift(1)
_tr=pl.max_horizontal(pl.col("high")-pl.col("low"),(pl.col("high")-_p).abs(),(pl.col("low")-_p).abs())
_a=features._gc_alpha(RP,4)
_ch=(btc.with_columns(features._cascade((pl.col("high")+pl.col("low")+pl.col("close"))/3,_a,4).alias("f"),
                      features._cascade(_tr,_a,4).alias("t"))
     .with_columns((pl.col("f")+pl.col("t")*1.414).alias("u")).with_row_index("i")
     .with_columns(pl.when(pl.col("i")>=RP).then(pl.col("f")).otherwise(None).alias("f"),
                   pl.when(pl.col("i")>=RP).then(pl.col("u")).otherwise(None).alias("u"))
     .with_columns((pl.col("f")/pl.col("f").shift(LEAN_N)-1).alias("lean")).sort("ts_utc"))
REG={r["ts_utc"]:r for r in _ch.iter_rows(named=True)}

S,E=datetime(2019,10,1,tzinfo=timezone.utc),datetime(2026,8,1,tzinfo=timezone.utc)
rows=[]; day=S
while day<=E:
    try: members=universe.load(day, root=DATA)
    except FileNotFoundError: day+=timedelta(days=STEP); continue
    e={m.asset for m in members if m.eligible}
    cx=frame.filter((pl.col("ts_utc")==day-timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(e)) & (pl.col("bars_available")>=cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())
    L=cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")["asset"].to_list()
    Sh=[a for a in cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")["asset"].to_list()
        if perp_from.get(a) is not None and perp_from[a]<=day]
    if len(L)<3 or len(Sh)<3: day+=timedelta(days=STEP); continue
    r=REG.get(day-timedelta(seconds=cfg.interval_s)); t=0.0
    if r and r["u"] is not None and r["lean"] is not None:
        sg=1.0 if r["close"]>r["u"] else (-1.0 if r["close"]<r["f"] else 0.0)
        t=max(-CAP,min(CAP,sg*abs(r["lean"])*SCALE))
    wl,ws=0.5+t,0.5-t
    fwd=ic._forward_returns(prices,L+Sh,day,STEP)
    tab=dict(zip(fwd["asset"].to_list(),fwd["forward_return"].to_list()))
    lr=[tab[a] for a in L if a in tab]; sr=[tab[a] for a in Sh if a in tab]
    if len(lr)<3 or len(sr)<3: day+=timedelta(days=STEP); continue
    rows.append({"day":day,"nL":len(L),"nS":len(Sh),"share":len(L)/(len(L)+len(Sh)),
                 "wL":wl/len(L),"wS":ws/len(Sh),
                 "net":wl*(sum(lr)/len(lr))-ws*(sum(sr)/len(sr)),
                 "legL":sum(lr)/len(lr),"legS":sum(sr)/len(sr)})
    day+=timedelta(days=STEP)

print(f"{len(rows)} weeks\n")
print("1. WHERE IS THE CONCENTRATION?  largest single-name weight, by side")
for k,lab in (("wL","long "),("wS","short")):
    v=sorted(r[k] for r in rows)
    print(f"   {lab}  median {statistics.median(v)*100:5.2f}%   90th {v[int(.9*len(v))]*100:5.2f}%"
          f"   max {v[-1]*100:5.2f}%")
big=[r for r in rows if r["wS"]>0.05]
print(f"\n   weeks where a single SHORT exceeds 5% of NAV: {len(big)}/{len(rows)}"
      f"  ({len(big)/len(rows)*100:.0f}%)")
big=[r for r in rows if r["wL"]>0.05]
print(f"   weeks where a single LONG  exceeds 5% of NAV: {len(big)}/{len(rows)}"
      f"  ({len(big)/len(rows)*100:.0f}%)")
thin=[r for r in rows if r["nS"]<r["nL"]]
print(f"   weeks with FEWER shorts than longs (your scenario): {len(thin)}/{len(rows)}"
      f"  ({len(thin)/len(rows)*100:.0f}%)")

print("\n2. IS BREADTH INFORMATIVE ABOUT THE FORWARD WEEK?")
sh=[r["share"] for r in rows]
for lab,key in (("net book return","net"),("long leg return","legL"),("short leg return","legS")):
    rho=ic.spearman(sh,[r[key] for r in rows])
    t=rho*math.sqrt((len(rows)-2)/max(1e-12,1-rho*rho))
    print(f"   long-share vs {lab:<18} rho {rho:+.3f}   t {t:+.2f}")

q=sorted(rows,key=lambda r:r["share"])
n=len(q)//4
print("\n   forward NET return by long-share quartile:")
for i,lab in enumerate(("Q1 narrowest","Q2","Q3","Q4 widest  ")):
    b=q[i*n:(i+1)*n] if i<3 else q[3*n:]
    print(f"     {lab}  share {statistics.median(r['share'] for r in b)*100:4.0f}%"
          f"   mean fwd net {statistics.mean(r['net'] for r in b)*100:+6.2f}%")