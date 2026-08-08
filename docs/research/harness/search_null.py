"""The test that actually addresses the selection effect.

The previous null compared the REAL best-of-44 against a null at ONE config,
which is not the right comparison. Give each null draw the same search the real
data got - sweep the scale, take its best - and see how high a signal with no
information can climb when it is allowed to search.

If the null's best-of-sweep routinely reaches the real result, the search
explains it. If the real result still stands clear, the search does not.
"""
import math, random, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT=Path("/home/magnus/dev/magnus/ai-trader"); DATA=ROOT/"data"
STEP,CAP,LEAN_N,REGIME_P=7,0.5,20,48
SCALES=(1.0,2.0,4.0,8.0,12.0)          # the sweep each draw is allowed
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
_a=features._gc_alpha(REGIME_P,4)
_ch=(btc.with_columns(features._cascade((pl.col("high")+pl.col("low")+pl.col("close"))/3,_a,4).alias("f"),
                      features._cascade(_tr,_a,4).alias("t"))
     .with_columns((pl.col("f")+pl.col("t")*1.414).alias("u")).with_row_index("i")
     .with_columns(pl.when(pl.col("i")>=REGIME_P).then(pl.col("f")).otherwise(None).alias("f"),
                   pl.when(pl.col("i")>=REGIME_P).then(pl.col("u")).otherwise(None).alias("u"))
     .with_columns((pl.col("f")/pl.col("f").shift(LEAN_N)-1).alias("lean")).sort("ts_utc"))
REG={r["ts_utc"]:r for r in _ch.iter_rows(named=True)}
CACHE={}
def universe_at(day):
    if day in CACHE: return CACHE[day]
    try: members=universe.load(day, root=DATA)
    except FileNotFoundError: CACHE[day]=None; return None
    e={m.asset for m in members if m.eligible}
    cx=frame.filter((pl.col("ts_utc")==day-timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(e)) & (pl.col("bars_available")>=cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())
    L=cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")["asset"].to_list()
    S=[a for a in cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")["asset"].to_list()
       if perp_from.get(a) is not None and perp_from[a]<=day]
    if len(L)<3 or len(S)<3: CACHE[day]=None; return None
    fwd=ic._forward_returns(prices,L+S,day,STEP)
    CACHE[day]=(L,S,dict(zip(fwd["asset"].to_list(),fwd["forward_return"].to_list())))
    return CACHE[day]
def hf(a,day):
    t=ftab.get(a); return 0.0 if not t else sum(t.get(day+timedelta(days=k),0.0) for k in range(STEP))
def run(S,E,scale,shuffle_seed=None):
    rng=random.Random(shuffle_seed); eq=1.0; curve=[]; prev={}
    day=S
    while day<=E:
        u=universe_at(day)
        if u is None: day+=timedelta(days=STEP); continue
        rl,rs,tab=u
        r=REG.get(day-timedelta(seconds=cfg.interval_s))
        t=0.0
        if r and r["u"] is not None and r["lean"] is not None:
            sg=1.0 if r["close"]>r["u"] else (-1.0 if r["close"]<r["f"] else 0.0)
            t=max(-CAP,min(CAP,sg*abs(r["lean"])*scale))
        wl,ws=0.5+t,0.5-t
        if shuffle_seed is not None:
            pool=[a for a in rl+rs if perp_from.get(a) is not None and perp_from[a]<=day]
            if len(pool)<len(rl)+3: day+=timedelta(days=STEP); continue
            rng.shuffle(pool); L,Sh=pool[:len(rl)],pool[len(rl):]
        else: L,Sh=rl,rs
        lr=[tab[a] for a in L if a in tab]; sr=[tab[a] for a in Sh if a in tab]
        if len(lr)<3 or len(sr)<3: day+=timedelta(days=STEP); continue
        g=wl*(sum(lr)/len(lr))-ws*(sum(sr)/len(sr))
        fp=ws*(sum(hf(a,day) for a in Sh)/len(Sh))
        w={a:wl/len(L) for a in L}
        for a in Sh: w[a]=w.get(a,0.0)-ws/len(Sh)
        turn=sum(abs(w.get(a,0.0)-prev.get(a,0.0)) for a in set(w)|set(prev)); prev=w
        eq*=1+g+fp-turn*COST/10_000; curve.append(eq); day+=timedelta(days=STEP)
    rets=[curve[i]/curve[i-1]-1 for i in range(1,len(curve))]
    m=sum(rets)/len(rets); sd=math.sqrt(sum((x-m)**2 for x in rets)/(len(rets)-1))
    return (m*365/STEP)/(sd*math.sqrt(365/STEP)), eq-1
U=timezone.utc
FR=(datetime(2019,10,1,tzinfo=U),datetime(2021,10,1,tzinfo=U))
OR=(datetime(2021,10,1,tzinfo=U),datetime(2026,8,1,tzinfo=U))
def best_of_sweep(seed):
    best=(-9,None)
    for sc in SCALES:
        fs,fn=run(*FR,sc,seed); os_,on=run(*OR,sc,seed)
        ms=min(fs,os_)
        if ms>best[0]: best=(ms,(1+fn)*(1+on)-1,sc)
    return best
rm,rc,rsc=best_of_sweep(None)
print(f"REAL best-of-sweep : min-Sharpe {rm:.2f}  combined {rc*100:.1f}%  at scale {rsc}", flush=True)
N=int(sys.argv[1]) if len(sys.argv)>1 else 12
nulls=[]
for s in range(N):
    b=best_of_sweep(s); nulls.append(b)
    print(f"  null seed {s:>2}: best min-Sharpe {b[0]:.2f}  combined {b[1]*100:>9.1f}%  at scale {b[2]}", flush=True)
ms=sorted(x[0] for x in nulls); cs=sorted(x[1] for x in nulls)
print(f"\nnull best-of-sweep min-Sharpe: median {statistics.median(ms):.2f}  max {ms[-1]:.2f}")
print(f"null best-of-sweep combined  : median {statistics.median(cs)*100:.1f}%  max {cs[-1]*100:.1f}%")
above=sum(1 for x in ms if x>=rm)
print(f"\nnull draws matching or beating the REAL min-Sharpe: {above}/{N}"
      f"   -> empirical p = {(above+1)/(N+1):.3f}")