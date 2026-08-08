"""Is the short leg a signal or a residual? And what does the edge look like
without compounding?

Current construction is asymmetric: LONG = close above the upper band (a
signal); SHORT = every other eligible name (a residual). We therefore short ~24
names a week regardless of whether anything about them says "short".

The symmetric alternative uses the channel's own lower band, so both legs are
signals of the same kind, and the book is only deployed where the detector
actually has an opinion. Gross then floats below 1.0 when there is nothing to
say - which is a feature, not a shortfall.

Also reports FIXED-BUDGET P&L: the same weekly returns applied to a constant
stake instead of a compounding one. That strips the exponential and shows the
edge itself - whether the strategy is getting better, worse, or staying put.
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

def run(S,E,mode):
    """mode: 'residual' (current) or 'symmetric' (short only below the lower band)."""
    eq,curve,prev,gl,ns_,nl_=1.0,[],{},[],[],[]
    simple=0.0; simple_curve=[]
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
        if mode=="residual":
            cand=cx.filter(pl.col("gc_breakout_age").is_null())
        else:
            cand=cx.filter(pl.col("close") < pl.col("gc_lower"))
        Sh=[a for a in cand.sort("asset")["asset"].to_list()
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
        g=wl*(sum(lr)/len(lr))-ws*(sum(sr)/len(sr))
        fp=ws*(sum(hf(a,day) for a in Sh)/len(Sh))
        w={a:wl/len(L) for a in L}
        for a in Sh: w[a]=w.get(a,0.0)-ws/len(Sh)
        turn=sum(abs(w.get(a,0.0)-prev.get(a,0.0)) for a in set(w)|set(prev)); prev=w
        wk=g+fp-turn*COST/10_000
        eq*=1+wk; simple+=wk
        curve.append([day.date().isoformat(),eq]); simple_curve.append([day.date().isoformat(),simple])
        gl.append(wl+ws); nl_.append(len(L)); ns_.append(len(Sh)); day+=timedelta(days=STEP)
    rr=[curve[i][1]/curve[i-1][1]-1 for i in range(1,len(curve))]
    m=sum(rr)/len(rr); sd=statistics.stdev(rr)
    pk,dd=curve[0][1],0.0
    for _,v in curve: pk=max(pk,v); dd=min(dd,v/pk-1)
    return {"net":eq-1,"simple":simple,"sharpe":(m*52)/(sd*math.sqrt(52)),"maxdd":dd,
            "n":len(curve),"long":statistics.median(nl_),"short":statistics.median(ns_),
            "curve":curve,"simple_curve":simple_curve}

U=timezone.utc
W={"fresh":(datetime(2019,10,1,tzinfo=U),datetime(2021,10,1,tzinfo=U)),
   "orig":(datetime(2021,10,1,tzinfo=U),datetime(2026,8,1,tzinfo=U))}
out={}
print(f"{'mode':<11}{'window':<8}{'n':>5}{'nL':>5}{'nS':>5}{'compounded':>12}"
      f"{'fixed-budget':>14}{'Sharpe':>8}{'maxDD':>9}")
for mode in ("residual","symmetric"):
    out[mode]={}
    for w,(S,E) in W.items():
        r=run(S,E,mode); out[mode][w]=r
        print(f"{mode:<11}{w:<8}{r['n']:>5}{r['long']:>5.0f}{r['short']:>5.0f}"
              f"{r['net']*100:>11.1f}%{r['simple']*100:>13.1f}%{r['sharpe']:>8.2f}{r['maxdd']*100:>8.1f}%")
for mode in out:
    c=(1+out[mode]['fresh']['net'])*(1+out[mode]['orig']['net'])-1
    s=out[mode]['fresh']['simple']+out[mode]['orig']['simple']
    print(f"\n{mode:<11} combined compounded {c*100:>9.1f}%   fixed-budget {s*100:>8.1f}%   "
          f"min Sharpe {min(out[mode][w]['sharpe'] for w in W):.2f}")
Path(sys.argv[1]).write_bytes((json.dumps(out,indent=2)+"\n").encode())