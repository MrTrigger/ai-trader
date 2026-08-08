"""Spot-long/perp-short versus perp-long/perp-short.

The backtest assumes the long leg is SPOT, so funding is only ever received (on
the short perp) and never paid. That assumption is a venue choice, not a fact.
Running both legs as perpetuals - which is what a perps-first venue like
Hyperliquid implies - means paying funding on the longs as well, and in crypto
funding is positive far more often than not, so the two legs largely cancel.

Funding contributed +15.6% of the recorded return, so this is not a rounding
difference. Measured here rather than assumed, over the same tranched book.

Also prices both structures at their real venue rates instead of one blended
number, since the whole point of choosing a venue is that the rates differ.
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
CAP, SCALE, MIN_LEG, STEP = 0.5, 8.0, 3, 7
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
_C = {}


def legs(day):
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
            & pl.col("gc_upper").is_not_null())
        L = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("gc_breakout_age")["asset"].to_list()
        Sh = (cx.filter(pl.col("gc_breakout_age").is_null() & pl.col("shortable"))
              .with_columns(((pl.col("close") - pl.col("gc_lower")) / pl.col("gc_lower")).alias("_d"))
              .sort("_d"))["asset"].to_list()
    except FileNotFoundError:
        L = Sh = []
    _C[k] = (L, Sh); return L, Sh


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def ledger(S, E, long_is_perp):
    """Per week: gross P&L, net funding, long turnover, short turnover.

    `long_is_perp` decides whether the long leg pays funding. A perp long that
    is only tradeable where a perp exists also shrinks the long candidate set,
    so the same borrowable filter is applied to both legs in that case.
    """
    prev, out = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
        L, Sh = legs(day)
        if long_is_perp:
            # A perp long needs a listed perp, exactly as a short does.
            L = [a for a in L if a in ftab]
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < MIN_LEG or len(Sh) < MIN_LEG:
            tl = sum(v for v in prev.values() if v > 0)
            ts = sum(-v for v in prev.values() if v < 0)
            prev = {}
            out.append((day, 0.0, 0.0, tl, ts)); day += timedelta(days=STEP); continue
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
            out.append((day, 0.0, 0.0, 0.0, 0.0)); day += timedelta(days=STEP); continue
        g = GROSS * wl * (sum(lr)/len(lr)) - GROSS * ws * (sum(sr)/len(sr))
        # Short perp RECEIVES funding when the rate is positive; long perp PAYS.
        f_short = GROSS * ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        f_long = -GROSS * wl * (sum(hf(a, day) for a in L) / len(L)) if long_is_perp else 0.0
        w = {a: GROSS * wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * ws / len(Sh)
        keys = set(w) | set(prev)
        tl = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in keys
                 if w.get(a, 0.0) > 0 or prev.get(a, 0.0) > 0)
        ts = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in keys
                 if w.get(a, 0.0) < 0 or prev.get(a, 0.0) < 0)
        prev = w
        out.append((day, g, f_short + f_long, tl, ts))
        day += timedelta(days=STEP)
    return out


def tranche(long_is_perp):
    per = {}
    for k in range(7):
        for row in ledger(S + timedelta(days=k), E, long_is_perp):
            per.setdefault(row[0].isocalendar()[:2], []).append(row)
    wks = sorted(w for w, v in per.items() if len(v) == 7)
    return [tuple(statistics.mean(x[i] for x in per[w]) for i in (1, 2, 3, 4)) for w in wks]


def price(rows, long_bps, short_bps):
    rets = [g + f - (tl * long_bps + ts * short_bps) / 10_000 for g, f, tl, ts in rows]
    eq, pk, dd = 1.0, 1.0, 0.0
    for r in rets:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"final": eq - 1, "sharpe": (m*52)/(sd*math.sqrt(52)) if sd else 0.0,
            "maxdd": dd, "funding": sum(f for _, f, _, _ in rows)}


U = timezone.utc
S, E = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
spot = tranche(False)
perp = tranche(True)

print("funding, summed over the whole run:")
print(f"  spot long / perp short   {price(spot,7,7)['funding']*100:+7.1f}%   (received only)")
print(f"  perp long / perp short   {price(perp,7,7)['funding']*100:+7.1f}%   (paid on longs, received on shorts)")

print("\npriced at each venue's BASE taker rate, plus 2bps half-spread:")
ROWS = [
    ("OKX  spot long / perp short", spot, 10 + 2, 5 + 2),
    ("OKX  perp long / perp short", perp, 5 + 2, 5 + 2),
    ("HL   perp long / perp short", perp, 4.5 + 2, 4.5 + 2),
    ("HL   perp both, maker fills", perp, 1.5 + 0, 1.5 + 0),
    ("OKX  perp both, maker fills", perp, 2 + 0, 2 + 0),
]
print(f"{'structure':<32}{'long bps':>9}{'short bps':>10}{'return':>11}{'Sharpe':>8}{'maxDD':>8}")
for name, rows, lb, sb in ROWS:
    r = price(rows, lb, sb)
    print(f"{name:<32}{lb:>9.1f}{sb:>10.1f}{r['final']*100:>10.1f}%{r['sharpe']:>8.2f}{r['maxdd']*100:>7.1f}%")