"""Does the result survive changing which weekday we rebalance?

Nothing in the strategy references a weekday. Rebalancing on Tuesday rather than
Friday shifts every holding period by three days and changes nothing else - no
parameter, no rule, no data. A real weekly edge is therefore approximately
invariant to the offset; seven runs should scatter around one number.

If they do not, the result is a property of one sampling phase rather than of the
market, and no amount of out-of-sample window or null test would have caught it,
because every one of those tests reused this same phase.
"""
import math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
STEP = 7
cfg = Config.load(ROOT / "config" / "default.toml")
MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
BTC_PX = {r["ts_utc"]: r["mark_open"] for r in
          prices.filter(pl.col("asset") == cfg.benchmark).iter_rows(named=True)}
CAP, SCALE = 0.5, 8.0


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def run(S, E):
    prev, rets, flat = {}, [], 0
    day = S
    while day <= E:
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
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < 3 or len(Sh) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rets.append(-turn * COST / 10_000); flat += 1; day += timedelta(days=STEP); continue
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rets.append(-turn * COST / 10_000); flat += 1; day += timedelta(days=STEP); continue
        g = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        w = {a: wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rets.append(g + fp - turn * COST / 10_000)
        day += timedelta(days=STEP)
    eq = 1.0; pk = 1.0; dd = 0.0
    for r in rets:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"net": eq - 1, "sharpe": (m * 52) / (sd * math.sqrt(52)), "maxdd": dd,
            "flat": flat, "n": len(rets)}


def btc(S, E):
    rets, last = [], None
    day = S
    while day <= E:
        px = BTC_PX.get(day)
        rets.append(0.0 if (last is None or px is None) else px / last - 1)
        if px is not None: last = px
        day += timedelta(days=STEP)
    eq = 1.0
    for r in rets: eq *= 1 + r
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"net": eq - 1, "sharpe": (m * 52) / (sd * math.sqrt(52))}


U = timezone.utc
BASE_F = datetime(2019, 10, 1, tzinfo=U)
BASE_O = datetime(2021, 10, 1, tzinfo=U)
END = datetime(2026, 8, 1, tzinfo=U)
DAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")

print(f"{'offset':<8}{'weekday':<9}{'fresh ret':>12}{'fresh Sh':>10}"
      f"{'orig ret':>12}{'orig Sh':>10}{'orig DD':>9}{'flat wk':>9}")
fresh_n, orig_n, orig_s = [], [], []
for k in range(7):
    f = run(BASE_F + timedelta(days=k), BASE_O)
    o = run(BASE_O + timedelta(days=k), END)
    fresh_n.append(f["net"]); orig_n.append(o["net"]); orig_s.append(o["sharpe"])
    wd = DAYS[(BASE_O + timedelta(days=k)).weekday()]
    print(f"+{k:<7}{wd:<9}{f['net']*100:>11.1f}%{f['sharpe']:>10.2f}"
          f"{o['net']*100:>11.1f}%{o['sharpe']:>10.2f}{o['maxdd']*100:>8.1f}%{o['flat']:>9}")
    sys.stdout.flush()

print(f"\norig return  : median {statistics.median(orig_n)*100:>9.1f}%   "
      f"min {min(orig_n)*100:>9.1f}%   max {max(orig_n)*100:>9.1f}%")
print(f"orig Sharpe  : median {statistics.median(orig_s):>9.2f}   "
      f"min {min(orig_s):>9.2f}   max {max(orig_s):>9.2f}")
print(f"fresh return : median {statistics.median(fresh_n)*100:>9.1f}%   "
      f"min {min(fresh_n)*100:>9.1f}%   max {max(fresh_n)*100:>9.1f}%")
b = [btc(BASE_O + timedelta(days=k), END) for k in range(7)]
print(f"\nBTC control  : return median {statistics.median([x['net'] for x in b])*100:.1f}%   "
      f"min {min(x['net'] for x in b)*100:.1f}%   max {max(x['net'] for x in b)*100:.1f}%"
      f"   (should be tight - it is the same asset)")