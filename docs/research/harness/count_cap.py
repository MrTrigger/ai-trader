"""Does the strategy survive max_position_count, or does it need the limit moved?

The research book holds ~32 names and the config allows 12, so the risk gate
rejects every plan. The tempting move is to raise the limit, which is how a
constraint quietly becomes a parameter. The honest move is to ask what the
strategy is worth *under* the limit, and only then decide.

Truncating needs a ranking per leg, and only one leg has one. Longs already rank
by breakout recency (`gc_breakout` sizes on it). The short leg is a residual and
has no score at all - so it is ranked by distance below the lower band, the same
detector read the other way, which is the least arbitrary choice available and
still an addition the research never had to make.

Swept rather than asserted, because 12 is one point on a curve and the shape of
that curve is the actual answer.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import features, ic, store, universe, borrow
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT=Path("/home/magnus/dev/magnus/ai-trader"); DATA=ROOT/"data"
STEP,CAP,SCALE=7,0.5,8.0
cfg=Config.load(ROOT/"config"/"default.toml")
MAXPOS=float(cfg.limits.max_position)
bars=store.read(root=DATA, interval_s=cfg.interval_s)
frame=features.build(bars, benchmark=cfg.benchmark,
                     shortable_from=borrow.listings(root=DATA))
prices=mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund=pl.read_parquet(DATA/"funding"/"binance_um.parquet")
COST=float(cfg.costs.commission_bps+cfg.costs.spread_bps)
ftab={(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(),v["daily_rate"].to_list()))
      for k,v in fund.partition_by("asset",as_dict=True).items()}
BENCH={r["ts_utc"]:r for r in frame.filter(pl.col("asset")==cfg.benchmark)
       .select(["ts_utc","close","gc_regime_filter","gc_regime_upper","gc_regime_slope"])
       .iter_rows(named=True)}

def hf(a,day):
    t=ftab.get(a); return 0.0 if not t else sum(t.get(day+timedelta(days=k),0.0) for k in range(STEP))

def run(S,E,*,max_names=None):
    eq,simple,curve,prev=1.0,0.0,[],{}
    day=S
    while day<=E:
        try: members=universe.load(day, root=DATA)
        except FileNotFoundError: day+=timedelta(days=STEP); continue
        e={m.asset for m in members if m.eligible}
        hz=day-timedelta(seconds=cfg.interval_s)
        cx=frame.filter((pl.col("ts_utc")==hz)
            & pl.col("asset").is_in(list(e)) & (pl.col("bars_available")>=cfg.min_history_bars)
            & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
            & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
            & pl.col("gc_upper").is_not_null())
        Ld=cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("gc_breakout_age")
        Sd=(cx.filter(pl.col("gc_breakout_age").is_null() & pl.col("shortable"))
            .with_columns(((pl.col("close")-pl.col("gc_lower"))/pl.col("gc_lower")).alias("_d"))
            .sort("_d"))
        L=Ld["asset"].to_list(); Sh=Sd["asset"].to_list()
        if len(L)<3 or len(Sh)<3: day+=timedelta(days=STEP); continue

        b=BENCH.get(hz); t=0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg=1.0 if b["close"]>b["gc_regime_upper"] else (-1.0 if b["close"]<b["gc_regime_filter"] else 0.0)
            t=max(-CAP,min(CAP,sg*abs(b["gc_regime_slope"])*SCALE))
        wl,ws=0.5+t,0.5-t

        if max_names:
            # Split the budget by leg weight, never below 3 a side.
            nl=max(3,min(len(L),round(max_names*wl)))
            ns=max(3,min(len(Sh),max_names-nl))
            L,Sh=L[:nl],Sh[:ns]
        if len(L)<3 or len(Sh)<3: day+=timedelta(days=STEP); continue

        # The per-position cap the config already sets.
        if wl/len(L)>MAXPOS: wl=MAXPOS*len(L); ws=1.0-wl
        if ws/len(Sh)>MAXPOS: ws=MAXPOS*len(Sh); wl=1.0-ws

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
        curve.append([day.date().isoformat(),eq])
        day+=timedelta(days=STEP)
    rr=[curve[i][1]/curve[i-1][1]-1 for i in range(1,len(curve))]
    m=sum(rr)/len(rr); sd=statistics.stdev(rr)
    pk,dd=curve[0][1],0.0
    for _,v in curve: pk=max(pk,v); dd=min(dd,v/pk-1)
    return {"net":eq-1,"simple":simple,"sharpe":(m*52)/(sd*math.sqrt(52)),"maxdd":dd,"n":len(curve)}

U=timezone.utc
W={"fresh":(datetime(2019,10,1,tzinfo=U),datetime(2021,10,1,tzinfo=U)),
   "orig":(datetime(2021,10,1,tzinfo=U),datetime(2026,8,1,tzinfo=U))}
print(f"{'max names':>10}{'fresh Sh':>10}{'orig Sh':>10}{'min Sh':>8}{'worst DD':>10}"
      f"{'compounded':>13}{'fixed-budget':>14}")
out={}
for mn in (12,16,20,24,32,None):
    rs={w:run(S,E,max_names=mn) for w,(S,E) in W.items()}
    out[str(mn)]=rs
    c=(1+rs['fresh']['net'])*(1+rs['orig']['net'])-1
    sp=rs['fresh']['simple']+rs['orig']['simple']
    lab="uncapped" if mn is None else str(mn)
    print(f"{lab:>10}{rs['fresh']['sharpe']:>10.2f}{rs['orig']['sharpe']:>10.2f}"
          f"{min(rs[w]['sharpe'] for w in W):>8.2f}{min(rs[w]['maxdd'] for w in W)*100:>9.1f}%"
          f"{c*100:>12.1f}%{sp*100:>13.1f}%{'   <- config limit' if mn==12 else ''}")
Path(sys.argv[1]).write_bytes((json.dumps(out,indent=2)+"\n").encode())