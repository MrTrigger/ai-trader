"""Rebalance-frequency sweep (design spec §10.3), with the control that makes it mean something.

§10.3: "Daily is the assumed start. Hours-to-weeks holds do not obviously need
daily evaluation, and every rebalance costs spread. Sweep it in Phase 1 — one
axis, plateau centre, not the peak."

**The control is the whole point.** A cadence sweep on a long-only book in a
falling market trivially favours trading less, because every trade costs and
every position loses. Run alone, this would "discover" that monthly beats
weekly and the discovery would be about the spread, not the signal.

So the baseline is swept on the identical grid and the reported statistic is
`candidate − baseline` at each cadence. That difference is not mechanically
driven by turnover: both arms pay the same kind of cost at the same frequency,
so what is left is whether the signal is worth acting on more or less often.
"""

from __future__ import annotations

import json
import sys
import time
from dataclasses import replace
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path

from planner import backtest, validate
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader")
DATA = ROOT / "data"
OUT = Path(sys.argv[1])
S = datetime(2021, 10, 1, tzinfo=timezone.utc)
E = datetime(2026, 8, 1, tzinfo=timezone.utc)
CASH = Decimal(100_000)

CADENCES = (1, 3, 7, 14, 30)

base = Config.load(ROOT / "config" / "default.toml")
candidate = replace(base, signal="gc_breakout", constructor="conviction_tilt")
control = replace(base, signal="liquidity_top", constructor="equal_weight")


def run(cfg: Config, every: int) -> dict:
    c = replace(cfg, rebalance_every=every)
    t0 = time.time()
    r = backtest.replay(config=c, start=S, end=E, data_root=DATA, initial_cash=CASH)
    wf = validate.walk_forward(r, interval_s=c.interval_s, folds=4)
    m = r.metrics
    print(
        f"    every={every:<3} n={m.n:<5} ret={float(m.total_return) * 100:>8.2f}% "
        f"turnover={float(m.turnover_per_rebalance) * 100:>6.2f}% "
        f"cost={float(m.cost_drag_bps):>7.1f}bps  ({time.time() - t0:.0f}s)",
        flush=True,
    )
    return {
        "every": every,
        "n": m.n,
        "return": float(m.total_return),
        "cagr": m.cagr,
        "sharpe": m.sharpe,
        "max_drawdown": float(m.max_drawdown),
        "turnover": float(m.turnover_per_rebalance),
        "cost_bps": float(m.cost_drag_bps),
        "rejected": m.rejected,
        "oos_folds_positive": wf.positive_folds,
        "oos_folds": len(wf.folds),
        "oos_mean": (
            sum(f.test.metrics.total_return for f in wf.folds) / len(wf.folds)
            if wf.folds
            else 0.0
        ),
    }


print("candidate: gc_breakout + conviction_tilt", flush=True)
cand = [run(candidate, e) for e in CADENCES]
print("control: liquidity_top + equal_weight", flush=True)
ctrl = [run(control, e) for e in CADENCES]

points = []
for c, k in zip(cand, ctrl):
    spread = c["return"] - k["return"]
    points.append(
        validate.SweepPoint(
            value=c["every"],
            metrics=backtest.metrics([], interval_s=86400),
            holds_up=spread > 0,
        )
    )

plateau = validate.find_plateau(points, axis="rebalance_every")

record = {
    "axis": "rebalance_every",
    "cadences": list(CADENCES),
    "candidate": cand,
    "control": ctrl,
    "spreads": [
        {
            "every": c["every"],
            "candidate": c["return"],
            "control": k["return"],
            "spread": c["return"] - k["return"],
            "beats_control": c["return"] > k["return"],
        }
        for c, k in zip(cand, ctrl)
    ],
    "plateau": {
        "values": list(plateau.values),
        "centre": plateau.centre,
        "width": plateau.width,
        "is_a_peak": plateau.is_a_peak,
    },
}
OUT.write_bytes((json.dumps(record, indent=2) + "\n").encode())

print("\n" + "=" * 78, flush=True)
print(f"{'every':>6} {'candidate':>11} {'control':>11} {'spread':>10}   verdict", flush=True)
for row in record["spreads"]:
    print(
        f"{row['every']:>6} {row['candidate'] * 100:>10.2f}% {row['control'] * 100:>10.2f}% "
        f"{row['spread'] * 100:>9.2f}pp   "
        f"{'beats control' if row['beats_control'] else 'loses to control'}",
        flush=True,
    )
print("=" * 78, flush=True)
print(str(plateau), flush=True)
print(f"WROTE {OUT}", flush=True)
