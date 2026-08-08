"""Two tests that can actually falsify the fast version, rather than doubt it.

**A. Label shuffle.** Keep everything - the tilt schedule, the leg sizes, the
costs, the funding, the universe, the number of names on each side - and replace
only WHICH assets go in which leg, drawn at random. If the strategy still works,
the channel signal contributes nothing and the return is coming from the tilt
schedule alone. Run over many seeds to get a null distribution rather than one
draw, then ask where the real result sits in it.

**B. Timing-only.** Same tilt schedule, but the legs are replaced by the
benchmark itself: long BTC when the tilt is positive, short BTC when negative,
sized identically. If that captures most of the return, the cross-sectional
selection is decorative and this is a BTC market-timing strategy wearing a
long/short costume - a much simpler claim, and a much more fragile one, because
it rests entirely on one regime detector applied to one asset.

Together these separate three possible sources of the +6884%: the selection
edge, the market timing, and the search.
"""
from __future__ import annotations

import math, random, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import polars as pl

from planner import features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT=Path("/home/magnus/dev/magnus/ai-trader"); DATA=ROOT/"data"
STEP, CAP, LEAN_N, SCALE, REGIME_P = 7, 0.5, 20, 8.0, 48
cfg=Config.load(ROOT/"config"/"default.toml")
bars=store.read(root=DATA, interval_s=cfg.interval_s)
frame=features.build(bars, benchmark=cfg.benchmark)
prices=mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund=pl.read_parquet(DATA/"funding"/"binance_um.parquet")
COST=float(cfg.costs.commission_bps+cfg.costs.spread_bps)
perp_from=dict(fund.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
ftab={(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
      for k,v in fund.partition_by("asset", as_dict=True).items()}

btc=bars.filter(pl.col("asset")==cfg.benchmark).sort("ts_utc")
_p=pl.col("close").shift(1)
_tr=pl.max_horizontal(pl.col("high")-pl.col("low"),(pl.col("high")-_p).abs(),(pl.col("low")-_p).abs())
_a=features._gc_alpha(REGIME_P,4)
_ch=(btc.with_columns(features._cascade((pl.col("high")+pl.col("low")+pl.col("close"))/3,_a,4).alias("f"),
                      features._cascade(_tr,_a,4).alias("t"))
     .with_columns((pl.col("f")+pl.col("t")*1.414).alias("u"))
     .with_row_index("i")
     .with_columns(pl.when(pl.col("i")>=REGIME_P).then(pl.col("f")).otherwise(None).alias("f"),
                   pl.when(pl.col("i")>=REGIME_P).then(pl.col("u")).otherwise(None).alias("u"))
     .with_columns((pl.col("f")/pl.col("f").shift(LEAN_N)-1).alias("lean")).sort("ts_utc"))
REG={r["ts_utc"]:r for r in _ch.iter_rows(named=True)}

def tilt_at(hz):
    r=REG.get(hz)
    if not r or r["u"] is None or r["lean"] is None: return 0.0
    sign = 1.0 if r["close"]>r["u"] else (-1.0 if r["close"]<r["f"] else 0.0)
    return max(-CAP, min(CAP, sign*abs(r["lean"])*SCALE))

def held_funding(a,day,days):
    t=ftab.get(a); return 0.0 if not t else sum(t.get(day+timedelta(days=k),0.0) for k in range(days))

def eligible_at(day):
    try: members=universe.load(day, root=DATA)
    except FileNotFoundError: return pl.DataFrame()
    e={m.asset for m in members if m.eligible}
    return frame.filter((pl.col("ts_utc")==day-timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(e)) & (pl.col("bars_available")>=cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())

def run(S,E,*,mode="real",seed=0):
    rng=random.Random(seed)
    eq,curve,prev_w=1.0,[],{}
    day=S
    while day<=E:
        cx=eligible_at(day)
        if cx.is_empty(): day+=timedelta(days=STEP); continue
        real_L=cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")["asset"].to_list()
        real_S=[a for a in cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")["asset"].to_list()
                if perp_from.get(a) is not None and perp_from[a]<=day]
        if len(real_L)<3 or len(real_S)<3: day+=timedelta(days=STEP); continue
        hz=day-timedelta(seconds=cfg.interval_s)
        t=tilt_at(hz); wl,ws=0.5+t,0.5-t

        if mode=="shuffle":
            # Same leg SIZES, same shortable pool, random membership.
            pool=[a for a in real_L+real_S if perp_from.get(a) is not None and perp_from[a]<=day]
            if len(pool)<len(real_L)+3: day+=timedelta(days=STEP); continue
            rng.shuffle(pool)
            L, Sh = pool[:len(real_L)], pool[len(real_L):]
        elif mode=="timing":
            L, Sh = ["BTC"], ["BTC"]
        else:
            L, Sh = real_L, real_S

        need = list(dict.fromkeys(L+Sh))
        fwd=ic._forward_returns(prices, need, day, STEP)
        tab=dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr=[tab[a] for a in L if a in tab]; sr=[tab[a] for a in Sh if a in tab]
        if not lr or not sr: day+=timedelta(days=STEP); continue

        g=wl*(sum(lr)/len(lr))-ws*(sum(sr)/len(sr))
        fp=ws*(sum(held_funding(a,day,STEP) for a in Sh)/len(Sh))
        w={a: wl/len(L) for a in L}
        for a in Sh: w[a]=w.get(a,0.0)-ws/len(Sh)
        turn=sum(abs(w.get(a,0.0)-prev_w.get(a,0.0)) for a in set(w)|set(prev_w))
        prev_w=w
        eq*=1+g+fp-turn*COST/10_000
        curve.append(eq); day+=timedelta(days=STEP)

    rets=[curve[i]/curve[i-1]-1 for i in range(1,len(curve))]
    m=sum(rets)/len(rets); sd=math.sqrt(sum((x-m)**2 for x in rets)/(len(rets)-1))
    ppy=365/STEP
    peak,dd=curve[0],0.0
    for v in curve: peak=max(peak,v); dd=min(dd,v/peak-1)
    return {"net":eq-1,"sharpe":(m*ppy)/(sd*math.sqrt(ppy)),"maxdd":dd}

U=timezone.utc
FR=(datetime(2019,10,1,tzinfo=U),datetime(2021,10,1,tzinfo=U))
OR=(datetime(2021,10,1,tzinfo=U),datetime(2026,8,1,tzinfo=U))

real_f, real_o = run(*FR), run(*OR)
real_min=min(real_f["sharpe"],real_o["sharpe"])
real_comb=(1+real_f["net"])*(1+real_o["net"])-1
print(f"REAL      fresh Sh {real_f['sharpe']:.2f}   orig Sh {real_o['sharpe']:.2f}   "
      f"min {real_min:.2f}   combined {real_comb*100:.1f}%")

tim_f, tim_o = run(*FR,mode="timing"), run(*OR,mode="timing")
tim_min=min(tim_f["sharpe"],tim_o["sharpe"])
tim_comb=(1+tim_f["net"])*(1+tim_o["net"])-1
print(f"TIMING    fresh Sh {tim_f['sharpe']:.2f}   orig Sh {tim_o['sharpe']:.2f}   "
      f"min {tim_min:.2f}   combined {tim_comb*100:.1f}%   (same tilt, BTC only)")

print(f"\nSHUFFLE null - identical everything, random leg membership:")
N=int(sys.argv[1]) if len(sys.argv)>1 else 25
mins, combs = [], []
for s in range(N):
    f, o = run(*FR,mode="shuffle",seed=s), run(*OR,mode="shuffle",seed=s)
    mins.append(min(f["sharpe"],o["sharpe"])); combs.append((1+f["net"])*(1+o["net"])-1)
    if s<5 or s==N-1:
        print(f"  seed {s:>2}: fresh {f['sharpe']:>5.2f}  orig {o['sharpe']:>5.2f}  "
              f"min {mins[-1]:>5.2f}  combined {combs[-1]*100:>9.1f}%")
mins.sort(); combs.sort()
def pct(v, arr): return sum(1 for x in arr if x < v)/len(arr)*100
print(f"\n  null min-Sharpe : median {statistics.median(mins):+.2f}   "
      f"90th pct {mins[int(.9*len(mins))]:+.2f}   max {mins[-1]:+.2f}")
print(f"  null combined   : median {statistics.median(combs)*100:+.1f}%   "
      f"90th pct {combs[int(.9*len(combs))]*100:+.1f}%   max {combs[-1]*100:+.1f}%")
print(f"\n  REAL min-Sharpe {real_min:.2f} sits at the {pct(real_min,mins):.0f}th percentile of the null")
print(f"  REAL combined {real_comb*100:.1f}% sits at the {pct(real_comb,combs):.0f}th percentile of the null")
