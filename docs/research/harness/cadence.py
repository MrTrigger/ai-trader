"""How often should the book be re-evaluated?

Checking daily is not the same as trading daily. Turnover is sum|delta weight|,
so a name that stays selected at the same target weight costs nothing to "keep"
- only genuine changes are paid for. Nor does a daily cadence shorten the
holding period: a position persists exactly as long as it stays selected. The
cadence governs how often the book can REACT, and a weekly cadence means a name
that collapses on day three is carried to day seven regardless.

Three cadences, everything else identical:

  weekly, 7 tranches   the current book. A seventh re-prices daily, but each
                       position is blind from its entry to its own next Monday.
  daily                every position re-checked every day. No blind window and
                       no phase to choose.
  daily + band         same, but a position's weight is only traded when it
                       drifts more than `BAND` from target, so price drift alone
                       does not generate turnover.

The band matters because without it the book re-targets every position every day
and pays for drift it did not choose.
"""
import json, math, statistics, sys
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
CAP, SCALE, MIN_LEG = 0.5, 8.0, 3
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
_CACHE = {}


def legs(day):
    key = day.date()
    if key in _CACHE:
        return _CACHE[key]
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
    _CACHE[key] = (L, Sh)
    return L, Sh


def tilt_at(hz):
    b = BENCH.get(hz)
    if b is None or b["gc_regime_upper"] is None or b["gc_regime_slope"] is None:
        return 0.0
    sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
    return max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))


def hf(a, day, days):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(days))


def targets(day):
    hz = day - timedelta(seconds=cfg.interval_s)
    t = tilt_at(hz)
    L, Sh = legs(day)
    wl, ws = 0.5 + t, 0.5 - t
    if len(L) < MIN_LEG or len(Sh) < MIN_LEG:
        return {}, [], []
    if len(L) + len(Sh) > MAXCOUNT:
        nl = max(MIN_LEG, min(len(L), round(MAXCOUNT * wl)))
        ns = max(MIN_LEG, min(len(Sh), MAXCOUNT - nl))
        L, Sh = L[:nl], Sh[:ns]
    if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
    if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
    w = {a: GROSS * wl / len(L) for a in L}
    for a in Sh: w[a] = w.get(a, 0.0) - GROSS * ws / len(Sh)
    return w, L, Sh


def run(S, E, step, band=0.0):
    held, rows = {}, []
    day = S
    while day <= E:
        want, L, Sh = targets(day)
        # A no-trade band: keep the existing weight unless the target has moved
        # more than `band`. Entries and exits always execute.
        if band > 0 and held:
            new = {}
            for a in set(want) | set(held):
                tgt, cur = want.get(a, 0.0), held.get(a, 0.0)
                if a not in want or a not in held:
                    new[a] = tgt
                elif abs(tgt - cur) < band:
                    new[a] = cur
                else:
                    new[a] = tgt
            want = {a: v for a, v in new.items() if v != 0.0}
        turn = sum(abs(want.get(a, 0.0) - held.get(a, 0.0)) for a in set(want) | set(held))
        held = want
        if not want:
            rows.append((day, -turn * COST / 10_000, turn)); day += timedelta(days=step); continue
        names = list(want)
        fwd = ic._forward_returns(prices, names, day, step)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        pnl = sum(w * tab[a] for a, w in want.items() if a in tab)
        shorts = [a for a, w in want.items() if w < 0]
        fp = sum(abs(want[a]) * hf(a, day, step) for a in shorts)
        rows.append((day, pnl + fp - turn * COST / 10_000, turn))
        day += timedelta(days=step)
    return rows


def stats(rows, ppy):
    rets = [r for _, r, _ in rows]
    eq, pk, dd = 1.0, 1.0, 0.0
    for r in rets:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    turn = statistics.mean(t for _, _, t in rows)
    return {"final": eq - 1, "sharpe": (m * ppy) / (sd * math.sqrt(ppy)) if sd else 0.0,
            "maxdd": dd, "worst": min(rets), "n": len(rets),
            "turn_yr": turn * ppy, "cost_yr": turn * ppy * COST / 10_000}


U = timezone.utc
S, E = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
out = {}
print(f"{'cadence':<26}{'n':>6}{'return':>11}{'Sharpe':>8}{'maxDD':>9}"
      f"{'worst':>8}{'turnover/yr':>13}{'cost/yr':>9}")

per = {}
for k in range(7):
    for day, r, t in run(S + timedelta(days=k), E, 7):
        per.setdefault(day.isocalendar()[:2], []).append((day, r, t))
wks = sorted(w for w, v in per.items() if len(v) == 7)
rows = [(min(x[0] for x in per[w]), statistics.mean(x[1] for x in per[w]),
         statistics.mean(x[2] for x in per[w])) for w in wks]
out["weekly, 7 tranches"] = stats(rows, 52)

for label, band in (("daily", 0.0), ("daily + 1% band", 0.01), ("daily + 2% band", 0.02)):
    out[label] = stats(run(S, E, 1), 365) if band == 0 else stats(run(S, E, 1, band), 365)

for label, st in out.items():
    print(f"{label:<26}{st['n']:>6}{st['final']*100:>10.1f}%{st['sharpe']:>8.2f}"
          f"{st['maxdd']*100:>8.1f}%{st['worst']*100:>7.1f}%"
          f"{st['turn_yr']*100:>12,.0f}%{st['cost_yr']*100:>8.1f}%")
    sys.stdout.flush()
Path(sys.argv[1]).write_bytes((json.dumps(out, indent=2) + "\n").encode())