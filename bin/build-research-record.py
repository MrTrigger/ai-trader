"""Stitch the clean walk-forward folds into the record the dashboard reads.

The previous backtest.json was produced by a harness that summed funding
FORWARD. This one is assembled from bin/walk-forward.sh output: six expanding
folds, each scored by a model that never saw its test block.
"""
import json, math, statistics, sys, glob
from datetime import date

OUT = sys.argv[1]
folds = [json.load(open(f)) for f in sorted(glob.glob("var/research/wf-honest-perrisk/fold-*.json"),
                                            key=lambda p: int(p.split("-")[-1].split(".")[0]))]
steps = []
for f in folds:
    steps.extend(f["steps"])
steps.sort(key=lambda s: s["as_of"])

# Per-step returns from each fold's own NAV path, then chained across folds:
# every fold starts at its own initial cash, so levels are not comparable but
# returns are.
rets = []
for f in folds:
    ss = sorted(f["steps"], key=lambda s: s["as_of"])
    prev = None
    for s in ss:
        nav = float(s["nav"])
        if prev is not None and prev != 0:
            rets.append((s["as_of"][:10], nav / prev - 1.0))
        prev = nav

eq, comp, dd, peak = 1.0, [], [], 1.0
for d, r in rets:
    eq *= 1 + r
    peak = max(peak, eq)
    comp.append([d, eq]); dd.append([d, eq / peak - 1])

vals = [r for _, r in rets]
n = len(vals)
m, sd = statistics.mean(vals), statistics.stdev(vals)
years = (date.fromisoformat(rets[-1][0]) - date.fromisoformat(rets[0][0])).days / 365.25
cagr = eq ** (1 / years) - 1
down = [r for r in vals if r < 0]
dsd = math.sqrt(sum(r * r for r in down) / len(down)) if down else 0.0
maxdd = min(x for _, x in dd)

record = {
    "strategy": {
        "name": "gradient-boosted cross-sectional ranker, daily",
        "params": [
            ("model", "LightGBM, 300 trees, 15 leaves, L2 20.0"),
            ("features", "67 - daily aggregates, fast daily, trailing funding, hourly path"),
            ("target", "demeaned 24h return PER UNIT OF VOLATILITY, from a fill 1h after the signal"),
            ("validation", "6 expanding walk-forward folds, model retrained at each, 2-day purge"),
            ("selection", "top 24 by |expected return|, dollar-neutral, sized by edge/volatility"),
            ("entry threshold", "expected edge > 1x round-trip cost"),
            ("rebalance", "daily, executed 1h after the signal"),
            ("venue", "Hyperliquid perpetuals, both legs"),
            ("costs", "4.5bp taker + 0.5bp half-spread"),
            ("funding", "Binance USD-M realised rates, TRAILING windows (proxy)"),
        ],
    },
    "window": [rets[0][0], rets[-1][0]],
    "metrics": {
        "total_return": eq - 1, "cagr": cagr,
        "volatility": sd * math.sqrt(365),
        "sharpe": (m * 365) / (sd * math.sqrt(365)),
        "sortino": (m * 365) / (dsd * math.sqrt(365)) if dsd else 0.0,
        "max_drawdown": maxdd, "calmar": cagr / abs(maxdd),
        "weeks": n, "years": years,
        "win_rate": sum(1 for r in vals if r > 0) / n,
        "best_week": max(vals), "worst_week": min(vals),
        "t_stat": m / (sd / math.sqrt(n)),
    },
    "folds": [
        {"fold": i + 1,
         "window": [sorted(f["steps"], key=lambda s: s["as_of"])[0]["as_of"][:10],
                    sorted(f["steps"], key=lambda s: s["as_of"])[-1]["as_of"][:10]],
         "steps": len(f["steps"]),
         "refused": sum(1 for x in f.get("disclosures", []) if "gate failed" in str(x)),
         "total_return": float(f["metrics"]["total_return"]),
         "sharpe": float(f["metrics"]["sharpe"]),
         "max_drawdown": float(f["metrics"]["max_drawdown"])}
        for i, f in enumerate(folds)
    ],
    "series": {"compounded": comp, "drawdown": dd},
    "provenance": {
        "produced_by": "bin/walk-forward.sh 2022-09-18 2026-07-30 6 <out> per_risk",
        "supersedes": "the +875.5% / Sharpe 2.70 record, which was leaked - see note",
        "note": (
            "The previous record was produced by docs/research/harness/ml_record.py, whose "
            "dataset builder summed the funding features FORWARD from each decision date over "
            "realised rates. funding_7d/30d/chg/z therefore carried days that had not happened "
            "yet, and funding tracks contemporaneous price pressure closely enough that this is "
            "nearly a copy of the target. The walk-forward itself was sound; the features were "
            "not. Flipping the window to trailing and changing nothing else took that same "
            "harness from +760.7%/Sharpe 2.54 to +134.9%/1.05. This record uses trailing "
            "windows throughout and retrains per fold, so no date is scored by a model that "
            "saw it."
        ),
    },
}
json.dump(record, open(OUT, "w"), indent=2)
mt = record["metrics"]
print(f"{mt['weeks']} days over {mt['years']:.2f}y")
print(f"total {mt['total_return']*100:.1f}%  CAGR {mt['cagr']*100:.1f}%  Sharpe {mt['sharpe']:.2f}"
      f"  maxDD {mt['max_drawdown']*100:.1f}%  t {mt['t_stat']:.2f}")
