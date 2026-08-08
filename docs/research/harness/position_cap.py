"""Apply the risk limit the research has been ignoring, and show the P&L two ways.

`limits.max_position = 0.25` is already in config/default.toml and is enforced by
the risk layer inside pipeline.run(). Every long/short number produced so far was
computed outside that pipeline, so the cap has never bound - and the book has been
running up to 33.3% of NAV on a single name, which the live system would reject.

This is a compliance fix, not a search: the value is read from config, nothing is
swept, and the excess moves to the opposite leg so gross stays pinned at 1.0.
A leg too thin to absorb its allocation is itself information - it means almost
nothing is above its channel - and the other leg is fat exactly then.

Also emits both equity conventions:
  compounded   - eq *= 1+r   ... what the account actually does
  fixed budget - eq += r     ... the same weekly returns on a constant stake,
                               which strips the exponential and shows whether the
                               edge itself is growing, decaying, or flat.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT=Path("/home/magnus/dev/magnus/ai-trader"); DATA=ROOT/"data"
STEP,CAP,LEAN_N,SCALE,RP=7,0.5,20,8.0,48
cfg=Config.load(ROOT/"config"/"default.toml")
MAXPOS=float(cfg.limits.max_position)
bars=store.read(root=DATA, interval_s=cfg.interval_s)
frame=features.build(bars, benchmark=cfg.benchmark)
prices=mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund=pl.read_parquet(DATA/"funding"/"binance_um.parquet")
COST=float(cfg.costs.commission_bps+cfg.costs.spread_bps)
perp_from=dict(fund.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
ftab={(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(),v["daily_rate"].to_list()))
      for k,v in fund.partition_by("asset",as_dict=True).items()}
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

def hf(a,day):
    t=ftab.get(a); return 0.0 if not t else sum(t.get(day+timedelta(days=k),0.0) for k in range(STEP))

def run(S,E,*,cap=None):
    eq,simple,curve,scurve,prev=1.0,0.0,[],[],{}
    bound=0; worst=0.0
    day=S
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
        worst=max(worst,wl/len(L),ws/len(Sh))
        if cap is not None:
            # A leg cannot hold more than cap per name. The excess moves to the
            # other leg rather than to cash: gross stays at 1.0 and the thin leg
            # being thin is the signal.
            if wl/len(L)>cap: wl=cap*len(L); ws=1.0-wl; bound+=1
            if ws/len(Sh)>cap: ws=cap*len(Sh); wl=1.0-ws; bound+=1
        fwd=ic._forward_returns(prices,L+Sh,day,STEP)
        tab=dict(zip(fwd["asset"].to_list(),fwd["forward_return"].to_list()))
        lr=[tab[a] for a in L if a in tab]; sr=[tab[a] for a in Sh if a in tab]
        if len(lr)<3 or len(sr)<3: day+=timedelta(days=STEP); continue
        g=wl*(sum(lr)/len(lr))-ws*(sum(sr)/len(sr))
        fp=ws*(sum(hf(a,day) for a in Sh)/len(Sh))
        w={a:wl/len(L) for a in L}
        for a in Sh: w[a]=w.get(a,0.0)-ws/len(Sh)
        turn=sum(abs(w.get(a,0.0)-prev.get(a,0.0)) for a in set(w)|set(prev)); prev=w
        wk=g+fp-turn*COST/10_000
        eq*=1+wk; simple+=wk
        curve.append([day.date().isoformat(),eq]); scurve.append([day.date().isoformat(),1.0+simple])
        day+=timedelta(days=STEP)
    rr=[curve[i][1]/curve[i-1][1]-1 for i in range(1,len(curve))]
    m=sum(rr)/len(rr); sd=statistics.stdev(rr)
    def mdd(c):
        pk,d=c[0][1],0.0
        for _,v in c: pk=max(pk,v); d=min(d,v/pk-1)
        return d
    return {"net":eq-1,"simple":simple,"sharpe":(m*52)/(sd*math.sqrt(52)),
            "maxdd":mdd(curve),"simple_maxdd":mdd(scurve),"n":len(curve),
            "bound":bound,"worst_name":worst,"curve":curve,"simple_curve":scurve}

U=timezone.utc
W={"fresh":(datetime(2019,10,1,tzinfo=U),datetime(2021,10,1,tzinfo=U)),
   "orig":(datetime(2021,10,1,tzinfo=U),datetime(2026,8,1,tzinfo=U))}
out={}
print(f"{'':<22}{'fresh Sh':>9}{'orig Sh':>9}{'min Sh':>8}{'worst DD':>10}"
      f"{'compounded':>12}{'fixed-budget':>14}{'max name':>10}{'weeks capped':>14}")
for lab,cap in (("uncapped (as shown)",None),(f"capped at {MAXPOS:.0%} (config)",MAXPOS)):
    rs={w:run(S,E,cap=cap) for w,(S,E) in W.items()}
    out[lab]=rs
    c=(1+rs['fresh']['net'])*(1+rs['orig']['net'])-1
    sp=rs['fresh']['simple']+rs['orig']['simple']
    print(f"{lab:<22}{rs['fresh']['sharpe']:>9.2f}{rs['orig']['sharpe']:>9.2f}"
          f"{min(rs[w]['sharpe'] for w in W):>8.2f}{min(rs[w]['maxdd'] for w in W)*100:>9.1f}%"
          f"{c*100:>11.1f}%{sp*100:>13.1f}%{max(rs[w]['worst_name'] for w in W)*100:>9.1f}%"
          f"{sum(rs[w]['bound'] for w in W):>14}")
Path(sys.argv[1]).write_bytes((json.dumps(out,indent=2)+"\n").encode())