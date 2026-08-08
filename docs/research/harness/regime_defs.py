"""Which regime definition, holding the tilt magnitude fixed at the plateau centre.

One axis: how the market state is read. Everything else - selection, tilt size,
costs, funding, shortability - is pinned, so any difference is attributable.

  A  level, 144d channel   the current one: above upper -> long, below filter -> short
  B  filter slope          the derivative. Measured as SLOWER than A in a crash:
                           a 4-pole 144d filter's slope lags price crossing it
  C  level, 36d channel    same test, faster instrument
  D  continuous            no threshold at all - tilt scales with where price sits
                           in its channel, so it starts reducing on the first
                           down-bar instead of waiting for a crossing
"""
import json, math, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT/"data"
STEP, TILT = 7, 0.15
cfg = Config.load(ROOT/"config"/"default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark)
prices = mark_discontinuities(bars).select(["asset","ts_utc","mark_open"])
fund = pl.read_parquet(DATA/"funding"/"binance_um.parquet")
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
perp_from = dict(fund.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
ftab = {(k[0] if isinstance(k,tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k,v in fund.partition_by("asset", as_dict=True).items()}

btc_bars = bars.filter(pl.col("asset")==cfg.benchmark).sort("ts_utc")
slow = (features.build(btc_bars, benchmark=cfg.benchmark).sort("ts_utc")
        .select(["ts_utc","close","gc_filter","gc_upper"]))
# A faster channel on the same price, for variant C.
a36 = features._gc_alpha(36, 4)
prev_c = pl.col("close").shift(1)
tr = pl.max_horizontal(pl.col("high")-pl.col("low"), (pl.col("high")-prev_c).abs(), (pl.col("low")-prev_c).abs())
fast = (btc_bars.with_columns(
            features._cascade((pl.col("high")+pl.col("low")+pl.col("close"))/3, a36, 4).alias("f36"),
            features._cascade(tr, a36, 4).alias("tr36"))
        .with_columns((pl.col("f36")+pl.col("tr36")*1.414).alias("u36"))
        .select(["ts_utc","close","f36","u36"]).sort("ts_utc"))

slow_t = {r["ts_utc"]: r for r in slow.iter_rows(named=True)}
fast_t = {r["ts_utc"]: r for r in fast.iter_rows(named=True)}
slope = {}
prev = None
for r in slow.iter_rows(named=True):
    if r["gc_filter"] is not None and prev is not None:
        slope[r["ts_utc"]] = r["gc_filter"] - prev
    prev = r["gc_filter"] if r["gc_filter"] is not None else prev

def tilt_for(defn, ts):
    s = slow_t.get(ts)
    if s is None or s["gc_upper"] is None: return 0.0
    if defn == "A_level_144":
        return TILT if s["close"] > s["gc_upper"] else (-TILT if s["close"] < s["gc_filter"] else 0.0)
    if defn == "B_filter_slope":
        v = slope.get(ts)
        return 0.0 if v is None else (TILT if v > 0 else -TILT)
    if defn == "C_level_36":
        g = fast_t.get(ts)
        if g is None or g["u36"] is None: return 0.0
        return TILT if g["close"] > g["u36"] else (-TILT if g["close"] < g["f36"] else 0.0)
    if defn == "D_continuous":
        band = s["gc_upper"] - s["gc_filter"]
        if band <= 0: return 0.0
        z = (s["close"] - s["gc_filter"]) / band
        return TILT * max(-1.0, min(1.0, z))
    raise ValueError(defn)

def held_funding(a, day, days):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day+timedelta(days=k), 0.0) for k in range(days))

def eligible_at(day):
    try: members = universe.load(day, root=DATA)
    except FileNotFoundError: return pl.DataFrame()
    elig = {m.asset for m in members if m.eligible}
    return frame.filter((pl.col("ts_utc")==day-timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(elig))
        & (pl.col("bars_available")>=cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote")>=float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null() & (pl.col("vol_30")>=float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null())

def run(defn, S, E):
    eq, curve, prev = 1.0, [], (set(), set())
    day = S
    while day <= E:
        cx = eligible_at(day)
        if cx.is_empty(): day += timedelta(days=STEP); continue
        ab = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")
        be = cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")
        longs = ab["asset"].to_list()
        shorts = [a for a in be["asset"].to_list() if perp_from.get(a) is not None and perp_from[a] <= day]
        if len(longs) < 3 or len(shorts) < 3: day += timedelta(days=STEP); continue
        t = tilt_for(defn, day - timedelta(seconds=cfg.interval_s))
        wl, ws = 0.5 + t, 0.5 - t
        fwd = ic._forward_returns(prices, longs+shorts, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in longs if a in tab]; sr = [tab[a] for a in shorts if a in tab]
        if len(lr) < 3 or len(sr) < 3: day += timedelta(days=STEP); continue
        g = wl*(sum(lr)/len(lr)) - ws*(sum(sr)/len(sr))
        fp = ws * (sum(held_funding(a, day, STEP) for a in shorts)/len(shorts))
        ch = len(set(longs)^prev[0]) + len(set(shorts)^prev[1])
        turn = min(1.0, ch/(2*(len(longs)+len(shorts)))) if curve else 1.0
        prev = (set(longs), set(shorts))
        eq *= 1 + g + fp - turn*COST/10_000
        curve.append([day.date().isoformat(), eq]); day += timedelta(days=STEP)
    rets = [curve[i][1]/curve[i-1][1]-1 for i in range(1,len(curve))]
    m = sum(rets)/len(rets); sd = math.sqrt(sum((r-m)**2 for r in rets)/(len(rets)-1))
    ppy, yrs = 365/STEP, (E-S).days/365
    peak, dd = curve[0][1], 0.0
    for _, v in curve: peak = max(peak, v); dd = min(dd, v/peak-1)
    return {"defn":defn,"net":eq-1,"cagr":eq**(1/yrs)-1,"sharpe":(m*ppy)/(sd*math.sqrt(ppy)),
            "maxdd":dd,"calmar":(eq**(1/yrs)-1)/abs(dd),"curve":curve}

W = {"fresh": (datetime(2019,10,1,tzinfo=timezone.utc), datetime(2021,10,1,tzinfo=timezone.utc)),
     "orig":  (datetime(2021,10,1,tzinfo=timezone.utc), datetime(2026,8,1,tzinfo=timezone.utc))}
DEFNS = ["A_level_144","B_filter_slope","C_level_36","D_continuous"]
out = {}
for w,(S,E) in W.items():
    out[w] = [run(d,S,E) for d in DEFNS]
    print(f"\n{w} window (tilt fixed at {TILT})")
    print(f"{'regime':<16}{'return':>10}{'CAGR':>9}{'Sharpe':>8}{'maxDD':>9}{'Calmar':>8}")
    for r in out[w]:
        print(f"{r['defn']:<16}{r['net']*100:>9.1f}%{r['cagr']*100:>8.1f}%{r['sharpe']:>8.2f}"
              f"{r['maxdd']*100:>8.1f}%{r['calmar']:>8.2f}")
print(f"\n{'regime':<16}{'combined':>11}{'vs BTC 658.6%':>15}{'worst DD':>10}")
for i,d in enumerate(DEFNS):
    c = (1+out['fresh'][i]['net'])*(1+out['orig'][i]['net'])-1
    dd = min(out['fresh'][i]['maxdd'], out['orig'][i]['maxdd'])
    print(f"{d:<16}{c*100:>10.1f}%{'BEATS' if c>6.586 else 'loses':>15}{dd*100:>9.1f}%")
Path(sys.argv[1]).write_bytes((json.dumps(out, indent=2)+"\n").encode())