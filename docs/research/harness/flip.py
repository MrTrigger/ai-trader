"""Flip the short-leg ranking, which is currently pointed the wrong way.

The twelve-position cap forces a choice of WHICH shorts to keep, and the rule
invented for it - deepest below the lower band first - turns out to select the
quintile with the best forward return (+1.76%) and short it, while discarding the
quintile that keeps falling (-0.98%). That is not a tuning preference; it is
using an informative ordering backwards.

Three variants, everything else identical and tranched across all seven phases:

  deepest   current: short the names furthest below their band
  shallow   flipped: short the names only just below it
  random    control: no ordering at all, so the two above can be read against
            "does the choice matter" rather than only against each other
"""
import json, math, random, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure)
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
CAP, SCALE, MIN_LEG, STEP = 0.5, 8.0, 3, 7
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, perp_listed_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
_C = {}


def pools(day):
    k = day.date()
    if k in _C: return _C[k]
    hz = day - timedelta(seconds=cfg.interval_s)
    try:
        members = universe.load(day, root=DATA)
        e = {m.asset for m in members if m.eligible}
        cx = frame.filter((pl.col("ts_utc") == hz) & pl.col("asset").is_in(list(e))
            & (pl.col("bars_available") >= cfg.min_history_bars)
            & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
            & pl.col("vol_30").is_not_null() & (pl.col("vol_30") >= float(cfg.min_volatility))
            & pl.col("gc_upper").is_not_null() & pl.col("perp_listed"))
        L = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("gc_breakout_age")["asset"].to_list()
        sd = (cx.filter(pl.col("gc_breakout_age").is_null())
              .with_columns(((pl.col("close") - pl.col("gc_lower")) / pl.col("gc_lower")).alias("_d"))
              .sort("_d"))
        S_ = sd["asset"].to_list()
    except FileNotFoundError:
        L, S_ = [], []
    _C[k] = (L, S_); return L, S_


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def run(S, E, mode, seed=0):
    rng = random.Random(seed)
    prev, rows = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
        L, Sh = pools(day)
        Sh = list(Sh)
        if mode == "shallow":
            Sh = Sh[::-1]                 # least-deep first
        elif mode == "random":
            rng.shuffle(Sh)
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < MIN_LEG or len(Sh) < MIN_LEG:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue
        if len(L) + len(Sh) > MAXCOUNT:
            nl = max(MIN_LEG, min(len(L), round(MAXCOUNT * wl)))
            ns = max(MIN_LEG, min(len(Sh), MAXCOUNT - nl))
            L, Sh = L[:nl], Sh[:ns]
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < MIN_LEG or len(sr) < MIN_LEG:
            rows.append((day, 0.0)); day += timedelta(days=STEP); continue
        g = GROSS * wl * (sum(lr)/len(lr)) - GROSS * ws * (sum(sr)/len(sr))
        fp = (GROSS * ws * (sum(hf(a, day) for a in Sh) / len(Sh))
              - GROSS * wl * (sum(hf(a, day) for a in L) / len(L)))
        w = {a: GROSS * wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rows.append((day, g + fp - turn * COST / 10_000))
        day += timedelta(days=STEP)
    return rows


def tranche(mode, S, E, seed=0):
    per = {}
    for k in range(7):
        for d, r in run(S + timedelta(days=k), E, mode, seed + k):
            per.setdefault(d.isocalendar()[:2], []).append(r)
    wks = sorted(w for w, v in per.items() if len(v) == 7)
    return [statistics.mean(per[w]) for w in wks]


def stats(rets):
    eq, pk, dd = 1.0, 1.0, 0.0
    for r in rets:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"final": eq - 1, "sharpe": (m*52)/(sd*math.sqrt(52)), "maxdd": dd,
            "t": m/(sd/math.sqrt(len(rets)))}


U = timezone.utc
W = {"fresh": (datetime(2019,10,1,tzinfo=U), datetime(2021,10,1,tzinfo=U)),
     "orig": (datetime(2021,10,1,tzinfo=U), datetime(2026,8,1,tzinfo=U))}
print(f"{'short ranking':<28}{'window':<8}{'return':>11}{'Sharpe':>9}{'maxDD':>9}{'t':>7}")
res = {}
for mode in ("deepest", "shallow", "random"):
    res[mode] = {}
    for wn, (S, E) in W.items():
        st = stats(tranche(mode, S, E)); res[mode][wn] = st
        lab = {"deepest": "deepest below (current)", "shallow": "shallowest below (flipped)",
               "random": "random (control)"}[mode]
        print(f"{lab:<28}{wn:<8}{st['final']*100:>10.1f}%{st['sharpe']:>9.2f}"
              f"{st['maxdd']*100:>8.1f}%{st['t']:>7.2f}")
        sys.stdout.flush()
    c = (1+res[mode]["fresh"]["final"])*(1+res[mode]["orig"]["final"])-1
    print(f"{'':<28}{'COMBINED':<8}{c*100:>10.1f}%\n")
Path(sys.argv[1]).write_bytes((json.dumps(res, indent=2)+"\n").encode())