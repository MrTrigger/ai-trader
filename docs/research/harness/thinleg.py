"""Stop standing down when one leg is thin. Run the leg that exists.

The book requires three names a side and goes flat otherwise. In a downtrend
almost nothing is above its channel, so the long leg thins and the WHOLE book
switches off - including the short leg, which is fat (27-34 candidates) and
which was making 8-13% a week immediately beforehand. Thirteen of forty weeks
across late 2025 and 2026 were flat with the tilt pinned at its maximum bearish
setting.

The rule contradicts the tilt it sits next to. At tilt -0.50 the strategy is
already instructing the book to be entirely on one side; refusing to hold that
book because the other side is empty is not caution, it is an accident of how
the minimum was written.

`thin_leg="carry"` lets the surviving leg take the whole gross when its opposite
cannot form AND the tilt already points that way. Gross is unchanged, so §9.2
still holds; what changes is that the strategy is allowed to do what it said.
Control is `thin_leg="flat"`, the current behaviour.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
STEP = 7
cfg = Config.load(ROOT / "config" / "default.toml")
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
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
CAP, SCALE, MIN_LEG = 0.5, 8.0, 3


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def run(S, E, thin_leg):
    prev, rows, carried = {}, [], 0
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
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
        wl, ws = 0.5 + t, 0.5 - t
        thinL, thinS = len(L) < MIN_LEG, len(Sh) < MIN_LEG

        if thinL and thinS:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue
        if thinL or thinS:
            # Only carry when the tilt already points at the surviving leg.
            if thin_leg == "carry" and ((thinL and t < 0) or (thinS and t > 0)):
                if thinL: L, wl, ws = [], 0.0, 1.0
                else: Sh, ws, wl = [], 0.0, 1.0
                carried += 1
            else:
                turn = sum(abs(v) for v in prev.values()); prev = {}
                rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue

        keep = (L or []) + (Sh or [])
        if len(keep) > MAXCOUNT:
            if L and Sh:
                nl = max(MIN_LEG, min(len(L), round(MAXCOUNT * wl)))
                ns = max(MIN_LEG, min(len(Sh), MAXCOUNT - nl))
                L, Sh = L[:nl], Sh[:ns]
            elif L: L = L[:MAXCOUNT]
            else: Sh = Sh[:MAXCOUNT]
        if L and wl / len(L) > MAXPOS: wl = MAXPOS * len(L)
        if Sh and ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh)

        fwd = ic._forward_returns(prices, (L or []) + (Sh or []), day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if (L and not lr) or (Sh and not sr):
            day += timedelta(days=STEP); continue
        g = (wl * (sum(lr)/len(lr)) if lr else 0.0) - (ws * (sum(sr)/len(sr)) if sr else 0.0)
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh)) if Sh else 0.0
        w = {a: wl/len(L) for a in L} if L else {}
        if Sh:
            for a in Sh: w[a] = w.get(a, 0.0) - ws/len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rows.append((day, g + fp - turn * COST / 10_000))
        day += timedelta(days=STEP)
    return rows, carried


def tranche(S, E, thin_leg):
    per, carried = {}, 0
    for k in range(7):
        rows, c = run(S + timedelta(days=k), E, thin_leg)
        carried += c
        for d, r in rows:
            per.setdefault(d.isocalendar()[:2], []).append(r)
    weeks = sorted(w for w, v in per.items() if len(v) == 7)
    return [statistics.mean(per[w]) for w in weeks], carried


def stats(rets):
    eq, pk, dd = 1.0, 1.0, 0.0
    for r in rets:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq/pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"final": eq-1, "sharpe": (m*52)/(sd*math.sqrt(52)) if sd else 0.0, "maxdd": dd,
            "t": m/(sd/math.sqrt(len(rets))) if sd else 0.0, "n": len(rets)}


U = timezone.utc
W = {"fresh": (datetime(2019,10,1,tzinfo=U), datetime(2021,10,1,tzinfo=U)),
     "orig": (datetime(2021,10,1,tzinfo=U), datetime(2026,8,1,tzinfo=U))}
print(f"{'variant':<26}{'window':<8}{'n':>5}{'return':>11}{'Sharpe':>9}{'maxDD':>9}{'t':>7}{'carried':>9}")
res = {}
for mode, label in (("flat", "stand down (current)"), ("carry", "carry the live leg")):
    res[mode] = {}
    for wn, (S, E) in W.items():
        rets, carried = tranche(S, E, mode)
        st = stats(rets); res[mode][wn] = st
        print(f"{label:<26}{wn:<8}{st['n']:>5}{st['final']*100:>10.1f}%{st['sharpe']:>9.2f}"
              f"{st['maxdd']*100:>8.1f}%{st['t']:>7.2f}{carried:>9}")
        sys.stdout.flush()
    c = (1 + res[mode]["fresh"]["final"]) * (1 + res[mode]["orig"]["final"]) - 1
    print(f"{'':<26}{'COMBINED':<8}{'':>5}{c*100:>10.1f}%\n")
Path(sys.argv[1]).write_bytes((json.dumps(res, indent=2) + "\n").encode())