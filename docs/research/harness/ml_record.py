"""Build the backtest record for the current strategy, fully instrumented."""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import numpy as np
import polars as pl
import lightgbm as lgb
from planner import store
from planner.config import Config

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
DATA = Path("/home/magnus/dev/magnus/ai-trader/data")
cfg = Config.load(Path("/home/magnus/dev/magnus/ai-trader/config/default.toml"))
GROSS = float(cfg.target_gross_exposure); MAXPOS = float(cfg.limits.max_position)
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
ROUND_TRIP, K = 2 * COST / 10_000, 1.0
U = timezone.utc; LAG_H, HOLD_H = 1, 24
MIN_NAMES, MAX_NAMES = 6, 24

ds = pl.read_parquet(S / "ds4.parquet").sort(["date", "asset"])
XC = [c for c in ds.columns if c.startswith("x_")]
dates = ds["date"].unique().sort().to_list()
PARAMS = dict(objective="regression", metric="l2", learning_rate=0.03, num_leaves=15,
              min_data_in_leaf=200, feature_fraction=0.7, bagging_fraction=0.7,
              bagging_freq=1, lambda_l2=20.0, verbose=-1, num_threads=4)
EDGES = np.linspace(int(len(dates) * 0.4), len(dates), 7).astype(int)

preds = {}
for k in range(6):
    lo, hi = EDGES[k], EDGES[k + 1]
    tr = ds.filter(pl.col("date").is_in(dates[:max(0, lo - 1)]))
    te = ds.filter(pl.col("date").is_in(dates[lo:hi]))
    if tr.height < 3000 or te.height < 300: continue
    m = lgb.train(PARAMS, lgb.Dataset(tr.select(XC).to_numpy(), label=tr["t1"].to_numpy()),
                  num_boost_round=300)
    for d, a, v, rv in zip(te["date"].to_list(), te["asset"].to_list(),
                           m.predict(te.select(XC).to_numpy()), te["rv_24h"].to_list()):
        preds[(d, a)] = (float(v), float(rv) if rv else None)

hourly = store.read(root=DATA, interval_s=3600).select(["asset", "ts_utc", "open"])
PX = {(a, t): o for a, t, o in hourly.iter_rows()}
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}


def build(rows):
    cand = [(a, p, v) for a, p, v in rows if abs(p) >= K * ROUND_TRIP and v]
    if len(cand) < MIN_NAMES: return {}
    cand.sort(key=lambda x: -abs(x[1])); cand = cand[:MAX_NAMES]
    L = [(a, abs(p), v) for a, p, v in cand if p > 0]
    Sh = [(a, abs(p), v) for a, p, v in cand if p <= 0]
    if len(L) < 2 or len(Sh) < 2: return {}
    def side(it):
        raw = {a: e / v for a, e, v in it}; t = sum(raw.values()) or 1.0
        return {a: x / t for a, x in raw.items()}
    w = {}
    for a, x in side(L).items(): w[a] = GROSS * 0.5 * x
    for a, x in side(Sh).items(): w[a] = w.get(a, 0.0) - GROSS * 0.5 * x
    mx = max(abs(v) for v in w.values())
    return {a: v * MAXPOS / mx for a, v in w.items()} if mx > MAXPOS else w


by = {}
for (d, a), (p, v) in preds.items():
    by.setdefault(d, []).append((a, p, v))
prev, led = {}, []
for d in sorted(by):
    day = datetime.fromisoformat(d).replace(tzinfo=U)
    entry = day + timedelta(hours=LAG_H)
    w = build(by[d])
    rec = {"date": d, "n_long": 0, "n_short": 0, "gross": 0.0, "net": 0.0,
           "turnover": 0.0, "cost": 0.0, "funding": 0.0, "ret": 0.0, "maxw": 0.0,
           "effn": 0.0, "flat": True}
    if not w:
        turn = sum(abs(x) for x in prev.values()); prev = {}
        rec["turnover"] = turn; rec["cost"] = turn * COST / 10_000
        rec["ret"] = -rec["cost"]; led.append(rec); continue
    got = {}
    for a in w:
        p0 = PX.get((a, entry)); p1 = PX.get((a, entry + timedelta(hours=HOLD_H)))
        if p0 and p1: got[a] = p1 / p0 - 1
    if len(got) < max(4, len(w) * 0.6): continue
    pnl = sum(w[a] * r for a, r in got.items())
    fp = sum(-v * ftab.get(a, {}).get(day.date(), 0.0) for a, v in w.items())
    turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
    prev = w
    g = sum(abs(x) for x in w.values())
    sh = [abs(x) / g for x in w.values()]
    rec.update(n_long=sum(1 for x in w.values() if x > 0),
               n_short=sum(1 for x in w.values() if x < 0), gross=g, net=sum(w.values()),
               turnover=turn, cost=turn * COST / 10_000, funding=fp,
               ret=pnl + fp - turn * COST / 10_000, maxw=max(abs(x) for x in w.values()),
               effn=1.0 / sum(x * x for x in sh), flat=False)
    led.append(rec)

rets = [r["ret"] for r in led]
eq, comp, fixed, ddc, pk, acc = 1.0, [], [], [], 1.0, 1.0
for r in led:
    eq *= 1 + r["ret"]; acc += r["ret"]; pk = max(pk, eq)
    comp.append([r["date"], eq]); fixed.append([r["date"], acc]); ddc.append([r["date"], eq/pk-1])
btc, beq, last = [], 1.0, None
for r in led:
    d = datetime.fromisoformat(r["date"]).replace(tzinfo=U) + timedelta(hours=LAG_H)
    px = PX.get(("BTC", d))
    if px and last: beq *= px / last
    if px: last = px
    btc.append([r["date"], beq])
bpk, bdd = 1.0, []
for d, v in btc:
    bpk = max(bpk, v); bdd.append([d, v/bpk-1])

n = len(rets); m, sd = statistics.mean(rets), statistics.stdev(rets)
years = (datetime.fromisoformat(led[-1]["date"]) - datetime.fromisoformat(led[0]["date"])).days/365
down = [r for r in rets if r < 0]
dsd = math.sqrt(sum(r*r for r in down)/len(down)) if down else 0
maxdd = min(x for _, x in ddc); cagr = eq**(1/years)-1
live = [r for r in led if not r["flat"]]

def yearly(c):
    seen, prev_end, out = {}, None, []
    for d, v in c: seen.setdefault(d[:4], []).append(v)
    for y in sorted(seen):
        vals = seen[y]; start = prev_end if prev_end is not None else vals[0]
        out.append({"year": y, "ret": vals[-1]/start - 1}); prev_end = vals[-1]
    return out

rec = {
    "strategy": {"name": "gradient-boosted cross-sectional ranker, daily",
        "params": [
            ("model", "LightGBM, 300 trees, 15 leaves, L2 20.0"),
            ("features", f"{len(XC)} - daily aggregates + hourly path/microstructure"),
            ("target", "24h return from a fill 1h AFTER the signal, cross-sectionally demeaned"),
            ("validation", "6 expanding walk-forward folds, 1-day purge either side"),
            ("selection", "top |expected return| / volatility, dollar-neutral"),
            ("entry threshold", f"expected edge > 1x round-trip cost ({ROUND_TRIP*10000:.0f}bp)"),
            ("position count", f"floating, {MIN_NAMES}-{MAX_NAMES}"),
            ("max position", f"{MAXPOS:.0%} of NAV"),
            ("target gross", f"{GROSS:g} of NAV"),
            ("rebalance", "daily, executed 1h after the signal"),
            ("venue", "Hyperliquid perpetuals, both legs"),
            ("costs", f"{cfg.costs.commission_bps}bp taker + {cfg.costs.spread_bps}bp half-spread (measured)"),
            ("funding", "Binance USD-M realised rates (proxy)"),
        ]},
    "window": [led[0]["date"], led[-1]["date"]],
    "metrics": {"total_return": eq-1, "cagr": cagr, "fixed_budget_return": acc-1,
        "volatility": sd*math.sqrt(365), "sharpe": (m*365)/(sd*math.sqrt(365)),
        "sortino": (m*365)/(dsd*math.sqrt(365)) if dsd else 0, "max_drawdown": maxdd,
        "calmar": cagr/abs(maxdd), "weeks": n, "years": years,
        "win_rate": sum(1 for r in rets if r > 0)/n, "best_week": max(rets),
        "worst_week": min(rets), "t_stat": m/(sd/math.sqrt(n)),
        "btc_return": btc[-1][1]-1, "btc_maxdd": min(x for _, x in bdd)},
    "exposure": {"gross": [[r["date"], r["gross"]] for r in led],
        "net": [[r["date"], r["net"]] for r in led],
        "max_gross": max(r["gross"] for r in led),
        "mean_gross": statistics.mean(r["gross"] for r in led),
        "max_net_long": max(r["net"] for r in led),
        "max_net_short": min(r["net"] for r in led),
        "mean_abs_net": statistics.mean(abs(r["net"]) for r in led),
        "max_name": max(r["maxw"] for r in led)},
    "stats": {"mean_long_names": statistics.mean(r["n_long"] for r in live),
        "mean_short_names": statistics.mean(r["n_short"] for r in live),
        "mean_turnover": statistics.mean(r["turnover"] for r in led),
        "total_cost": sum(r["cost"] for r in led),
        "total_funding": sum(r["funding"] for r in led),
        "from_long": 0.0, "from_short": 0.0,
        "flat_weeks": sum(1 for r in led if r["flat"]), "partial_weeks": 0,
        "pct_up_regime": statistics.mean(r["effn"] for r in live)/MAX_NAMES,
        "pct_down_regime": 0.0,
        "effective_n": statistics.mean(r["effn"] for r in live)},
    "series": {"compounded": comp, "fixed": fixed, "drawdown": ddc,
               "btc": btc, "btc_drawdown": bdd},
    "yearly": yearly(comp),
}
Path(sys.argv[1]).write_bytes((json.dumps(rec, indent=2)+"\n").encode())
mt = rec["metrics"]
print(f"{mt['weeks']} days over {mt['years']:.1f}y")
print(f"total {mt['total_return']*100:.1f}%  CAGR {mt['cagr']*100:.1f}%  Sharpe {mt['sharpe']:.2f}"
      f"  Sortino {mt['sortino']:.2f}  maxDD {mt['max_drawdown']*100:.1f}%  t {mt['t_stat']:.2f}")
print(f"BTC {mt['btc_return']*100:.1f}%  maxDD {mt['btc_maxdd']*100:.1f}%")
print(f"names L{rec['stats']['mean_long_names']:.1f}/S{rec['stats']['mean_short_names']:.1f}"
      f"  eff N {rec['stats']['effective_n']:.1f}  turnover {rec['stats']['mean_turnover']*100:.0f}%/day")