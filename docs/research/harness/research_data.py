"""Produce the research record: every replay, gate and diagnostic, as one JSON.

Rendering reads this. Runs are expensive and results are immutable once
computed, so the record is the artifact and the page is a view of it.
"""

from __future__ import annotations

import json
import math
import sys
from dataclasses import replace
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl

from planner import backtest, features, ic, store, universe, validate
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader")
DATA = ROOT / "data"
OUT = Path(sys.argv[1])
S = datetime(2021, 10, 1, tzinfo=timezone.utc)
E = datetime(2026, 8, 1, tzinfo=timezone.utc)
CASH = Decimal(100_000)

base = Config.load(ROOT / "config" / "default.toml")


def metrics_of(m) -> dict:
    return {
        "n": m.n,
        "total_return": float(m.total_return),
        "cagr": m.cagr,
        "volatility": m.volatility,
        "sharpe": m.sharpe,
        "max_drawdown": float(m.max_drawdown),
        "turnover": float(m.turnover_per_rebalance),
        "cost_bps": float(m.cost_drag_bps),
        "rejected": m.rejected,
    }


def run_one(label: str, cfg: Config) -> dict:
    print(f"  replay {label} ...", flush=True)
    r = backtest.replay(config=cfg, start=S, end=E, data_root=DATA, initial_cash=CASH)
    stressed = backtest.replay(
        config=cfg, start=S, end=E, data_root=DATA, initial_cash=CASH,
        slippage_multiple=Decimal(2),
    )
    wf = validate.walk_forward(r, interval_s=cfg.interval_s, folds=4)
    ho = validate.holdout(r, interval_s=cfg.interval_s)
    return {
        "label": label,
        "signal": cfg.signal,
        "constructor": cfg.constructor,
        "metrics": metrics_of(r.metrics),
        "stressed": metrics_of(stressed.metrics),
        "nav": [[ts.date().isoformat(), float(nav)] for ts, nav in r.nav_series],
        "folds": [
            {
                "name": f.test.name,
                "start": f.test.dates[0].isoformat() if f.test.dates else None,
                "end": f.test.dates[-1].isoformat() if f.test.dates else None,
                "return": float(f.test.metrics.total_return),
                "sharpe": f.test.metrics.sharpe,
                "n": f.test.metrics.n,
            }
            for f in wf.folds
        ],
        "holdout": {
            "train_return": float(ho.train.metrics.total_return),
            "test_return": float(ho.test.metrics.total_return),
            "consistent": ho.consistent,
        },
        "disclosures": r.disclosures,
    }


record: dict = {"window": [S.date().isoformat(), E.date().isoformat()]}

# --- provenance ------------------------------------------------------------
bars = store.read(root=DATA, interval_s=86400)
last = bars.group_by("asset").agg(pl.col("ts_utc").max().alias("last"))
record["data"] = {
    "assets": bars["asset"].n_unique(),
    "bars": bars.height,
    "delisted": last.filter(
        pl.col("last") < datetime(2026, 7, 1, tzinfo=timezone.utc)
    ).height,
    "first_bar": str(bars["ts_utc"].min().date()),
    "last_bar": str(bars["ts_utc"].max().date()),
}
print("provenance done", flush=True)

# --- universe over time ----------------------------------------------------
days = universe.snapshots(root=DATA)
uni = []
for d in days[::4]:
    members = universe.load(datetime(d.year, d.month, d.day, tzinfo=timezone.utc), root=DATA)
    uni.append(
        {
            "date": d.isoformat(),
            "eligible": sum(1 for m in members if m.eligible),
            "considered": len(members),
            "dead": sum(1 for m in members if "delisted" in m.reason),
        }
    )
record["universe"] = uni
print(f"universe done ({len(uni)} points)", flush=True)

# --- the runs --------------------------------------------------------------
record["runs"] = [
    run_one("momentum", replace(base, signal="xs_momentum", constructor="conviction_tilt")),
    run_one("gc_breakout", replace(base, signal="gc_breakout", constructor="conviction_tilt")),
    run_one("baseline", replace(base, signal="liquidity_top", constructor="equal_weight")),
]

# --- IC --------------------------------------------------------------------
print("  ic ...", flush=True)
record["ic"] = [
    {
        "horizon": r.horizon_days,
        "periods": r.n_periods,
        "effective_n": r.effective_n,
        "observations": r.n_observations,
        "mean_ic": r.mean_ic,
        "t_stat": r.t_stat,
        "hit_rate": r.hit_rate,
        "significant": r.distinguishable_from_zero,
        "series": [[p.as_of.date().isoformat(), p.ic] for p in r.periods],
    }
    for r in ic.measure(config=base, start=S, end=E, data_root=DATA)
]

# --- GC spread test --------------------------------------------------------
print("  spread ...", flush=True)
frame = features.build(bars, benchmark=base.benchmark)
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
spread_out = []
for H in (7, 14, 30):
    spreads, frac = [], []
    day = S
    while day <= E:
        try:
            members = universe.load(day, root=DATA)
        except FileNotFoundError:
            day += timedelta(days=7)
            continue
        elig = {m.asset for m in members if m.eligible}
        cx = frame.filter(
            (pl.col("ts_utc") == day - timedelta(days=1))
            & pl.col("asset").is_in(list(elig))
            & (pl.col("bars_available") >= base.min_history_bars)
            & pl.col("adv_quote").is_not_null()
            & (pl.col("adv_quote") >= float(base.min_dollar_volume))
            & pl.col("vol_30").is_not_null()
            & (pl.col("vol_30") >= float(base.min_volatility))
            & pl.col("gc_upper").is_not_null()
        ).select(["asset", pl.col("gc_breakout_age").is_not_null().alias("above")])
        if cx.height >= 10:
            fwd = ic._forward_returns(prices, cx["asset"].to_list(), day, H)
            j = cx.join(fwd, on="asset", how="inner").drop_nulls()
            a, b = j.filter(pl.col("above")), j.filter(~pl.col("above"))
            if a.height >= 3 and b.height >= 3:
                spreads.append(a["forward_return"].mean() - b["forward_return"].mean())
                frac.append(a.height / j.height)
        day += timedelta(days=7)
    n = len(spreads)
    mean = sum(spreads) / n
    sd = math.sqrt(sum((s - mean) ** 2 for s in spreads) / (n - 1))
    eff = n / max(1.0, H / 7)
    spread_out.append(
        {
            "horizon": H,
            "periods": n,
            "effective_n": eff,
            "mean_spread": mean,
            "t_stat": mean / (sd / math.sqrt(eff)),
            "pct_above": sum(frac) / n,
        }
    )
record["spread"] = spread_out

OUT.write_bytes((json.dumps(record, indent=2) + "\n").encode())
print(f"WROTE {OUT}", flush=True)
