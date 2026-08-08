"""Regime tilt: keep the selection edge, stop being flat through the big moves.

The market-neutral book earns a steady 25% CAGR and sits out every large
directional move, up and down. The hypothesis here is that the *same* detector
already used on each asset can be pointed at the benchmark to decide how much
net exposure the book should carry.

    BTC above its upper channel  -> strong uptrend  -> tilt net LONG
    BTC below its channel filter -> downtrend       -> tilt net SHORT
    in between                    -> no opinion      -> stay neutral

**Gross stays at 1.0 in every state**, so this is not leverage and §9.2 still
holds. Only the split moves:

    long = 0.5 + tilt,  short = 0.5 - tilt     (uptrend)
    long = 0.5 - tilt,  short = 0.5 + tilt     (downtrend)
    long = short = 0.5                          (neutral)

`tilt` is swept, and **tilt = 0 is exactly the book already measured**, so the
sweep contains its own control: any setting that fails to beat the tilt-0 column
has added nothing. Held to the same rule as every other sweep here — plateau
centre, never the peak, and a width-1 plateau is a peak.

Reported on Sharpe and on return/maxDD rather than on return, because the
decision this is serving is explicitly risk-adjusted.
"""

from __future__ import annotations

import json
import math
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import polars as pl

from planner import features, ic, store, universe, validate, backtest
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader")
DATA = ROOT / "data"
OUT = Path(sys.argv[1])
STEP = 7
TILTS = (0.0, 0.1, 0.2, 0.3, 0.4, 0.5)

cfg = Config.load(ROOT / "config" / "default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark)
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
funding = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
COST_BPS = float(cfg.costs.commission_bps + cfg.costs.spread_bps)

perp_from = dict(funding.group_by("asset").agg(pl.col("day").min().alias("f")).iter_rows())
ftab = {
    (k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
    for k, v in funding.partition_by("asset", as_dict=True).items()
}

# The benchmark's own channel state, by bar. Same indicator, pointed at BTC.
bench = (
    frame.filter(pl.col("asset") == cfg.benchmark)
    .select(["ts_utc", "close", "gc_filter", "gc_upper"])
    .sort("ts_utc")
)
regime = {}
for row in bench.iter_rows(named=True):
    if row["gc_upper"] is None:
        continue
    if row["close"] > row["gc_upper"]:
        regime[row["ts_utc"]] = "up"
    elif row["close"] < row["gc_filter"]:
        regime[row["ts_utc"]] = "down"
    else:
        regime[row["ts_utc"]] = "flat"


def held_funding(asset, day, days):
    t = ftab.get(asset)
    if not t:
        return 0.0
    return sum(t.get(day + timedelta(days=k), 0.0) for k in range(days))


def eligible_at(day):
    try:
        members = universe.load(day, root=DATA)
    except FileNotFoundError:
        return pl.DataFrame()
    elig = {m.asset for m in members if m.eligible}
    return frame.filter(
        (pl.col("ts_utc") == day - timedelta(seconds=cfg.interval_s))
        & pl.col("asset").is_in(list(elig))
        & (pl.col("bars_available") >= cfg.min_history_bars)
        & pl.col("adv_quote").is_not_null()
        & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
        & pl.col("vol_30").is_not_null()
        & (pl.col("vol_30") >= float(cfg.min_volatility))
        & pl.col("gc_upper").is_not_null()
    )


def run(tilt: float, S: datetime, E: datetime) -> dict:
    eq, curve, prev, states = 1.0, [], (set(), set()), {"up": 0, "down": 0, "flat": 0}
    day = S
    while day <= E:
        cx = eligible_at(day)
        if cx.is_empty():
            day += timedelta(days=STEP)
            continue
        above = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")
        below = cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")
        longs = above["asset"].to_list()
        shorts = [a for a in below["asset"].to_list()
                  if perp_from.get(a) is not None and perp_from[a] <= day]
        if len(longs) < 3 or len(shorts) < 3:
            day += timedelta(days=STEP)
            continue

        state = regime.get(day - timedelta(seconds=cfg.interval_s), "flat")
        states[state] += 1
        if state == "up":
            wl, ws = 0.5 + tilt, 0.5 - tilt
        elif state == "down":
            wl, ws = 0.5 - tilt, 0.5 + tilt
        else:
            wl = ws = 0.5

        fwd = ic._forward_returns(prices, longs + shorts, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in longs if a in tab]
        sr = [tab[a] for a in shorts if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            day += timedelta(days=STEP)
            continue

        gross = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        f_short = sum(held_funding(a, day, STEP) for a in shorts) / len(shorts)
        funding_pnl = ws * f_short  # spot long, perp short

        changed = len(set(longs) ^ prev[0]) + len(set(shorts) ^ prev[1])
        turn = min(1.0, changed / (2 * (len(longs) + len(shorts)))) if curve else 1.0
        prev = (set(longs), set(shorts))

        eq *= 1 + gross + funding_pnl - turn * COST_BPS / 10_000
        curve.append([day.date().isoformat(), eq])
        day += timedelta(days=STEP)

    rets = [curve[i][1] / curve[i - 1][1] - 1 for i in range(1, len(curve))]
    m = sum(rets) / len(rets) if rets else 0.0
    sd = math.sqrt(sum((r - m) ** 2 for r in rets) / (len(rets) - 1)) if len(rets) > 1 else 0.0
    ppy, years = 365 / STEP, (E - S).days / 365
    peak, dd = curve[0][1] if curve else 1.0, 0.0
    for _, v in curve:
        peak = max(peak, v)
        dd = min(dd, v / peak - 1)
    cagr = (eq ** (1 / years) - 1) if eq > 0 else -1.0
    return {
        "tilt": tilt, "periods": len(curve), "net_return": eq - 1, "cagr": cagr,
        "volatility": sd * math.sqrt(ppy),
        "sharpe": (m * ppy) / (sd * math.sqrt(ppy)) if sd > 0 else 0.0,
        "max_drawdown": dd, "calmar": (cagr / abs(dd)) if dd < 0 else 0.0,
        "regime_periods": states, "curve": curve,
    }


WINDOWS = {
    "fresh 2019-10..2021-10": (datetime(2019,10,1,tzinfo=timezone.utc), datetime(2021,10,1,tzinfo=timezone.utc)),
    "orig  2021-10..2026-08": (datetime(2021,10,1,tzinfo=timezone.utc), datetime(2026,8,1,tzinfo=timezone.utc)),
}

out = {}
for name, (S, E) in WINDOWS.items():
    rows = [run(t, S, E) for t in TILTS]
    out[name] = rows
    print(f"\n{name}   (tilt 0.0 is the market-neutral book already measured)")
    print(f"{'tilt':>6}{'n':>6}{'return':>10}{'CAGR':>9}{'vol':>8}{'Sharpe':>8}{'maxDD':>9}{'Calmar':>8}")
    for r in rows:
        print(f"{r['tilt']:>6.1f}{r['periods']:>6}{r['net_return']*100:>9.1f}%{r['cagr']*100:>8.1f}%"
              f"{r['volatility']*100:>7.1f}%{r['sharpe']:>8.2f}{r['max_drawdown']*100:>8.1f}%"
              f"{r['calmar']:>8.2f}")
    print(f"   regimes: {rows[0]['regime_periods']}")

# Plateau on "beats the tilt-0 Sharpe", computed per window and then required in BOTH.
base = {w: rows[0]["sharpe"] for w, rows in out.items()}
holds = []
for i, t in enumerate(TILTS):
    ok = all(out[w][i]["sharpe"] > base[w] for w in WINDOWS)
    holds.append(validate.SweepPoint(value=t, metrics=backtest.metrics([], interval_s=86400), holds_up=ok))
plateau = validate.find_plateau(holds, axis="regime_tilt")
print(f"\nimproves Sharpe in BOTH windows at: {[p.value for p in holds if p.holds_up]}")
print(plateau)

OUT.write_bytes((json.dumps({"tilts": list(TILTS), "windows": out,
                             "plateau": {"values": list(plateau.values), "centre": plateau.centre,
                                         "width": plateau.width, "is_a_peak": plateau.is_a_peak}},
                            indent=2) + "\n").encode())
print(f"WROTE {OUT}")
