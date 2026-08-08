"""The long/short probe again, with the two things the first one assumed away.

**Shortability.** Only 442 of our 656 assets ever had a USD-M perp, and they
listed at different dates. An asset with no perp at time T cannot be shorted at
time T, so the short leg is now drawn only from what actually existed. The first
probe shorted anything, which is not a modelling simplification - it is shorting
instruments that did not exist.

**Funding.** A perp position pays or receives funding every 8 hours; the median
across this data is 0.03%/day, about 11% annualised, which is far too large to
leave out of a 15% CAGR. Sign convention: a positive rate means longs pay shorts,
so a SHORT receives when funding is positive and pays when it is negative.

Two structures are measured because they have opposite funding exposure and both
are legitimate at 1.0 gross:

  * **spot-long / perp-short** - the classic market-neutral crypto shape. Only
    the short leg is funded, and it RECEIVES when funding is positive. No
    leverage. This is what a real book would most likely do.
  * **perp-long / perp-short** - both legs funded. The naive expectation is that
    funding nets out; it does not, because the long leg is uptrending and
    uptrending assets attract leveraged longs and therefore higher funding. This
    structure pays the difference.

Still not modelled and still cutting the same way: borrow availability, perp
liquidity at size, squeeze risk on beaten-down names, and the fact that a short
leg of the weakest assets is where a borrow gets recalled.
"""

from __future__ import annotations

import json
import math
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import polars as pl

from planner import features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader")
DATA = ROOT / "data"
OUT = Path(sys.argv[1])
S = datetime(2021, 10, 1, tzinfo=timezone.utc)
E = datetime(2026, 8, 1, tzinfo=timezone.utc)
STEP = 7

cfg = Config.load(ROOT / "config" / "default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark)
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
funding = pl.read_parquet(DATA / "funding" / "binance_um.parquet")

COST_BPS = float(cfg.costs.commission_bps + cfg.costs.spread_bps)

# When each asset's perp began. Before it, the asset is unshortable.
perp_from = dict(
    funding.group_by("asset")
    .agg(pl.col("day").min().alias("from"))
    .iter_rows()
)

# Cumulative funding over a holding period, per asset, keyed by start date.
funding_by_asset: dict[str, dict] = {}
for asset, sub in funding.partition_by("asset", as_dict=True).items():
    key = asset[0] if isinstance(asset, tuple) else asset
    funding_by_asset[key] = dict(
        zip(sub["day"].to_list(), sub["daily_rate"].to_list())
    )


def held_funding(asset: str, day: datetime, days: int) -> float:
    """Total funding rate accrued holding `asset` from `day` for `days`."""
    table = funding_by_asset.get(asset)
    if not table:
        return 0.0
    total, d = 0.0, day
    for _ in range(days):
        total += table.get(d, 0.0)
        d += timedelta(days=1)
    return total


def eligible_at(day: datetime) -> pl.DataFrame:
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


def shortable(asset: str, day: datetime) -> bool:
    began = perp_from.get(asset)
    return began is not None and began <= day


def run(structure: str, apply_shortability: bool) -> dict:
    eq, curve, turns, fnet, prev = 1.0, [], [], [], (set(), set())
    unshortable_dropped = 0
    day = S
    while day <= E:
        cx = eligible_at(day)
        if cx.is_empty():
            day += timedelta(days=STEP)
            continue

        above = cx.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")
        below = cx.filter(pl.col("gc_breakout_age").is_null()).sort("asset")
        longs = above["asset"].to_list()
        shorts = below["asset"].to_list()
        if apply_shortability:
            before = len(shorts)
            shorts = [a for a in shorts if shortable(a, day)]
            unshortable_dropped += before - len(shorts)

        if len(longs) < 3 or len(shorts) < 3:
            day += timedelta(days=STEP)
            continue

        fwd = ic._forward_returns(prices, longs + shorts, day, STEP)
        table = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [table[a] for a in longs if a in table]
        sr = [table[a] for a in shorts if a in table]
        if len(lr) < 3 or len(sr) < 3:
            day += timedelta(days=STEP)
            continue

        gross = 0.5 * (sum(lr) / len(lr)) - 0.5 * (sum(sr) / len(sr))

        # Funding. Positive rate = longs pay shorts.
        f_short = sum(held_funding(a, day, STEP) for a in shorts) / len(shorts)
        f_long = sum(held_funding(a, day, STEP) for a in longs) / len(longs)
        if structure == "spot_long_perp_short":
            funding_pnl = 0.5 * f_short            # short receives when positive
        else:
            funding_pnl = 0.5 * f_short - 0.5 * f_long

        changed = len(set(longs) ^ prev[0]) + len(set(shorts) ^ prev[1])
        turnover = min(1.0, changed / (2 * (len(longs) + len(shorts)))) if curve else 1.0
        prev = (set(longs), set(shorts))

        eq *= 1 + gross + funding_pnl - turnover * COST_BPS / 10_000
        curve.append([day.date().isoformat(), eq])
        turns.append(turnover)
        fnet.append(funding_pnl)
        day += timedelta(days=STEP)

    rets = [curve[i][1] / curve[i - 1][1] - 1 for i in range(1, len(curve))]
    m = sum(rets) / len(rets) if rets else 0.0
    sd = (
        math.sqrt(sum((r - m) ** 2 for r in rets) / (len(rets) - 1))
        if len(rets) > 1
        else 0.0
    )
    ppy = 365 / STEP
    years = (E - S).days / 365
    peak, dd = curve[0][1] if curve else 1.0, 0.0
    for _, v in curve:
        peak = max(peak, v)
        dd = min(dd, v / peak - 1)
    return {
        "structure": structure,
        "shortability_enforced": apply_shortability,
        "periods": len(curve),
        "net_return": eq - 1,
        "cagr": (eq ** (1 / years) - 1) if eq > 0 else -1.0,
        "volatility": sd * math.sqrt(ppy),
        "sharpe": (m * ppy) / (sd * math.sqrt(ppy)) if sd > 0 else 0.0,
        "max_drawdown": dd,
        "mean_funding_per_period": sum(fnet) / len(fnet) if fnet else 0.0,
        "funding_annualised": (sum(fnet) / len(fnet)) * ppy if fnet else 0.0,
        "unshortable_dropped": unshortable_dropped,
        "curve": curve,
    }


results = [
    run("perp_long_perp_short", False),   # closest to the original probe
    run("perp_long_perp_short", True),
    run("spot_long_perp_short", True),
]
OUT.write_bytes((json.dumps({"step_days": STEP, "results": results}, indent=2) + "\n").encode())

print(f"{'structure':<24}{'short?':>8}{'n':>6}{'net':>10}{'CAGR':>9}{'vol':>8}"
      f"{'Sharpe':>8}{'maxDD':>9}{'funding/yr':>12}")
for r in results:
    print(f"{r['structure']:<24}{'yes' if r['shortability_enforced'] else 'no':>8}"
          f"{r['periods']:>6}{r['net_return']*100:>9.2f}%{r['cagr']*100:>8.2f}%"
          f"{r['volatility']*100:>7.1f}%{r['sharpe']:>8.2f}{r['max_drawdown']*100:>8.2f}%"
          f"{r['funding_annualised']*100:>11.2f}%")
print(f"\nWROTE {OUT}")
