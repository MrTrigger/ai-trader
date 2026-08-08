"""Diagnostic: what would a market-neutral version of these signals have returned?

**This is not a strategy and cannot become one here.** §9.2 puts leverage above
1x out of scope before Phase 3, and shorting spot needs margin, so a book like
this is not runnable today whatever it says. It is measured because it settles
which of two very different situations Phase 1 is in:

  * the signals are simply bad -> a long/short version loses too, and the
    family is exhausted;
  * the signals carry a real relative edge that a long-only book structurally
    cannot monetise -> a roadmap fact about when this becomes reachable, not a
    reason to keep tuning long-only variants.

Construction, deliberately crude and pessimistic:

  * dollar-neutral, 50% long / 50% short, gross 1.0 - no leverage
  * equal weight within each leg
  * forward returns `mark_open` to `mark_open`, the same convention the backtest
    fills at, so the delisted keep their return to the last price they traded
  * costs charged on measured name turnover in both legs at the config's
    commission + spread, and again at 2x as the §9 sensitivity check
  * NO shorting cost, NO borrow, NO funding - all of which are real and would
    make this worse. The number is therefore an UPPER bound on what the shape
    could deliver, which is the useful direction for a go/no-go.
"""

from __future__ import annotations

import json
import math
import sys
from datetime import datetime, timedelta, timezone
from decimal import Decimal
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
LEG = 10  # names per leg

cfg = Config.load(ROOT / "config" / "default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark)
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])

# Round-trip cost of moving one unit of weight, in bps: cross the spread and pay
# commission. Same numbers the backtest's fill model uses.
COST_BPS = float(cfg.costs.commission_bps + cfg.costs.spread_bps)


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
    )


def legs_momentum(cx: pl.DataFrame) -> tuple[list[str], list[str]]:
    d = cx.filter(pl.col("ret_30_skip_7").is_not_null()).sort(
        ["ret_30_skip_7", "asset"], descending=[True, False]
    )
    if d.height < LEG * 2:
        return [], []
    return d.head(LEG)["asset"].to_list(), d.tail(LEG)["asset"].to_list()


def legs_breakout(cx: pl.DataFrame) -> tuple[list[str], list[str]]:
    d = cx.filter(pl.col("gc_upper").is_not_null())
    above = d.filter(pl.col("gc_breakout_age").is_not_null()).sort("asset")
    below = d.filter(pl.col("gc_breakout_age").is_null()).sort("asset")
    if above.height < 3 or below.height < 3:
        return [], []
    return above.head(LEG)["asset"].to_list(), below.head(LEG)["asset"].to_list()


def probe(name: str, pick) -> dict:
    equity_gross, equity_net, equity_2x = 1.0, 1.0, 1.0
    curve, turnovers, periods = [], [], 0
    prev_long: set[str] = set()
    prev_short: set[str] = set()

    day = S
    while day <= E:
        cx = eligible_at(day)
        if cx.is_empty():
            day += timedelta(days=STEP)
            continue
        longs, shorts = pick(cx)
        if not longs:
            day += timedelta(days=STEP)
            continue

        fwd = ic._forward_returns(prices, longs + shorts, day, STEP)
        table = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [table[a] for a in longs if a in table]
        sr = [table[a] for a in shorts if a in table]
        if len(lr) < 3 or len(sr) < 3:
            day += timedelta(days=STEP)
            continue

        # Dollar-neutral: half the book each side.
        gross = 0.5 * (sum(lr) / len(lr)) - 0.5 * (sum(sr) / len(sr))

        # Name turnover across both legs, as a fraction of gross.
        changed = len(set(longs) ^ prev_long) + len(set(shorts) ^ prev_short)
        turnover = min(1.0, changed / (2 * LEG * 2)) if periods else 1.0
        cost = turnover * COST_BPS / 10_000
        prev_long, prev_short = set(longs), set(shorts)

        equity_gross *= 1 + gross
        equity_net *= 1 + gross - cost
        equity_2x *= 1 + gross - turnover * (
            float(cfg.costs.commission_bps) + 2 * float(cfg.costs.spread_bps)
        ) / 10_000
        turnovers.append(turnover)
        periods += 1
        curve.append([day.date().isoformat(), equity_net])
        day += timedelta(days=STEP)

    years = (E - S).days / 365
    def cagr(x): return x ** (1 / years) - 1 if x > 0 else -1.0
    rets = [
        curve[i][1] / curve[i - 1][1] - 1 for i in range(1, len(curve))
    ]
    mean = sum(rets) / len(rets) if rets else 0.0
    sd = math.sqrt(sum((r - mean) ** 2 for r in rets) / (len(rets) - 1)) if len(rets) > 1 else 0.0
    ppy = 365 / STEP
    peak, dd = curve[0][1] if curve else 1.0, 0.0
    for _, v in curve:
        peak = max(peak, v)
        dd = min(dd, v / peak - 1)

    return {
        "signal": name,
        "periods": periods,
        "gross_return": equity_gross - 1,
        "net_return": equity_net - 1,
        "net_return_2x_slippage": equity_2x - 1,
        "cagr_net": cagr(equity_net),
        "volatility": sd * math.sqrt(ppy),
        "sharpe": (mean * ppy) / (sd * math.sqrt(ppy)) if sd > 0 else 0.0,
        "max_drawdown": dd,
        "mean_turnover": sum(turnovers) / len(turnovers) if turnovers else 0.0,
        "curve": curve,
    }


results = [probe("xs_momentum", legs_momentum), probe("gc_breakout", legs_breakout)]
OUT.write_bytes((json.dumps({"leg_size": LEG, "step_days": STEP,
                             "cost_bps_round_trip": COST_BPS,
                             "results": results}, indent=2) + "\n").encode())

print(f"{'signal':<14}{'n':>6}{'gross':>10}{'net':>10}{'net@2x':>10}"
      f"{'CAGR':>9}{'vol':>8}{'Sharpe':>8}{'maxDD':>9}{'turn':>7}")
for r in results:
    print(f"{r['signal']:<14}{r['periods']:>6}{r['gross_return']*100:>9.2f}%"
          f"{r['net_return']*100:>9.2f}%{r['net_return_2x_slippage']*100:>9.2f}%"
          f"{r['cagr_net']*100:>8.2f}%{r['volatility']*100:>7.1f}%{r['sharpe']:>8.2f}"
          f"{r['max_drawdown']*100:>8.2f}%{r['mean_turnover']*100:>6.0f}%")
print(f"\nWROTE {OUT}")
