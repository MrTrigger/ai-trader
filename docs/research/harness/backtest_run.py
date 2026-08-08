"""One backtest run, fully instrumented.

Produces everything a reader needs to judge a single run and nothing about any
other: the exact parameters, the equity under both conventions, the exposure the
book actually carried week by week (which is the margin question), and the
per-week statistics that say how it got there.
"""
import json, math, statistics, sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
STEP = 7
cfg = Config.load(ROOT / "config" / "default.toml")
GROSS = float(cfg.target_gross_exposure)
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
CAP, SCALE, LEAN_BARS, REGIME_P, MIN_LEG = 0.5, 8.0, 20, 48, 3

bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, shortable_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
fund = pl.read_parquet(DATA / "funding" / "binance_um.parquet")
ftab = {(k[0] if isinstance(k, tuple) else k): dict(zip(v["day"].to_list(), v["daily_rate"].to_list()))
        for k, v in fund.partition_by("asset", as_dict=True).items()}
BENCH = {r["ts_utc"]: r for r in frame.filter(pl.col("asset") == cfg.benchmark)
         .select(["ts_utc", "close", "gc_regime_filter", "gc_regime_upper", "gc_regime_slope"])
         .iter_rows(named=True)}
BTC_PX = {r["ts_utc"]: r["mark_open"] for r in
          prices.filter(pl.col("asset") == cfg.benchmark).iter_rows(named=True)}


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def run(S, E):
    """One phase. Returns a per-week ledger with exposure and attribution."""
    prev, out = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        b = BENCH.get(hz); t = 0.0; state = "warmup"
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            if b["close"] > b["gc_regime_upper"]: sg, state = 1.0, "up"
            elif b["close"] < b["gc_regime_filter"]: sg, state = -1.0, "down"
            else: sg, state = 0.0, "flat"
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
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
        wl, ws = 0.5 + t, 0.5 - t
        rec = {"date": day.date().isoformat(), "state": state, "tilt": t,
               "n_long": 0, "n_short": 0, "long_w": 0.0, "short_w": 0.0,
               "gross": 0.0, "net": 0.0, "turnover": 0.0, "cost": 0.0,
               "funding": 0.0, "from_long": 0.0, "from_short": 0.0, "ret": 0.0,
               "flat": True, "max_name": 0.0}
        if len(L) < MIN_LEG or len(Sh) < MIN_LEG:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rec["turnover"] = turn; rec["cost"] = turn * COST / 10_000
            rec["ret"] = -rec["cost"]
            out.append(rec); day += timedelta(days=STEP); continue
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
            out.append(rec); day += timedelta(days=STEP); continue
        # Weights are fractions of NAV, scaled to the configured gross target.
        w = {a: GROSS * wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - GROSS * ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev))
        prev = w
        from_l = GROSS * wl * (sum(lr) / len(lr))
        from_s = -GROSS * ws * (sum(sr) / len(sr))
        fp = GROSS * ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        cost = turn * COST / 10_000
        rec.update({
            "n_long": len(L), "n_short": len(Sh),
            "long_w": GROSS * wl, "short_w": GROSS * ws,
            "gross": sum(abs(v) for v in w.values()), "net": sum(w.values()),
            "turnover": turn, "cost": cost, "funding": fp,
            "from_long": from_l, "from_short": from_s,
            "ret": from_l + from_s + fp - cost, "flat": False,
            "max_name": max(abs(v) for v in w.values()),
        })
        out.append(rec); day += timedelta(days=STEP)
    return out


U = timezone.utc
S, E = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)

# Seven tranches, averaged per calendar week. Every field averages the same way
# because each tranche holds a seventh of capital.
per = {}
for k in range(7):
    for r in run(S + timedelta(days=k), E):
        wk = datetime.fromisoformat(r["date"]).isocalendar()[:2]
        per.setdefault(wk, []).append(r)
weeks = sorted(w for w, v in per.items() if len(v) == 7)
FIELDS = ("ret", "gross", "net", "turnover", "cost", "funding",
          "from_long", "from_short", "n_long", "n_short", "max_name", "tilt")
ledger = []
for wk in weeks:
    rs = per[wk]
    row = {"date": min(r["date"] for r in rs)}
    for f in FIELDS:
        row[f] = statistics.mean(r[f] for r in rs)
    row["flat_tranches"] = sum(1 for r in rs if r["flat"])
    ledger.append(row)

rets = [r["ret"] for r in ledger]
eq, comp, fixed, ddc, pk, acc = 1.0, [], [], [], 1.0, 1.0
for r in ledger:
    eq *= 1 + r["ret"]; acc += r["ret"]
    pk = max(pk, eq)
    comp.append([r["date"], eq]); fixed.append([r["date"], acc]); ddc.append([r["date"], eq / pk - 1])

# BTC on the same weekly grid, for one comparison line only.
btc, last = [], None
beq = 1.0
for r in ledger:
    d = datetime.fromisoformat(r["date"]).replace(tzinfo=U)
    px = BTC_PX.get(d)
    if px is not None and last is not None:
        beq *= px / last
    if px is not None: last = px
    btc.append([r["date"], beq])
bpk, bdd = 1.0, []
for d, v in btc:
    bpk = max(bpk, v); bdd.append([d, v / bpk - 1])

n = len(rets)
m, sd = statistics.mean(rets), statistics.stdev(rets)
years = (datetime.fromisoformat(ledger[-1]["date"]) - datetime.fromisoformat(ledger[0]["date"])).days / 365
down = [r for r in rets if r < 0]
dsd = math.sqrt(sum(r * r for r in down) / len(down)) if down else 0.0
maxdd = min(x for _, x in ddc)
cagr = eq ** (1 / years) - 1
live = [r for r in ledger if r["flat_tranches"] < 7]

def yearly(curve):
    out, seen = [], {}
    for d, v in curve:
        seen.setdefault(d[:4], []).append(v)
    prev_end = None
    for y in sorted(seen):
        vals = seen[y]
        start = prev_end if prev_end is not None else vals[0]
        out.append({"year": y, "ret": vals[-1] / start - 1})
        prev_end = vals[-1]
    return out

rec = {
    "strategy": {
        "name": "gc_long_short + conviction_tilt, tranched",
        "signal": "gc_long_short",
        "constructor": "conviction_tilt",
        "params": [
            ("channel period (selection)", "144 bars, 4-pole, 1.414x TR"),
            ("channel period (regime)", f"{REGIME_P} bars, 4-pole"),
            ("regime slope window", f"{LEAN_BARS} bars"),
            ("tilt gain / cap", f"{SCALE:g}x slope, capped +/-{CAP:g}"),
            ("long leg", "close above upper channel band"),
            ("short leg", "eligible, borrowable, not above band"),
            ("minimum leg", f"{MIN_LEG} names a side, else flat"),
            ("target gross", f"{GROSS:g} of NAV"),
            ("max position", f"{MAXPOS:.0%} of NAV"),
            ("max positions", str(MAXCOUNT)),
            ("rebalance", f"every {STEP}d, 7 tranches (one per weekday)"),
            ("costs", f"{cfg.costs.commission_bps}bps commission + {cfg.costs.spread_bps}bps spread"),
            ("funding", "Binance USD-M perpetuals, realised daily rates"),
            ("benchmark", cfg.benchmark),
        ],
    },
    "window": [ledger[0]["date"], ledger[-1]["date"]],
    "metrics": {
        "total_return": eq - 1, "cagr": cagr,
        "fixed_budget_return": acc - 1,
        "volatility": sd * math.sqrt(52),
        "sharpe": (m * 52) / (sd * math.sqrt(52)),
        "sortino": (m * 52) / (dsd * math.sqrt(52)) if dsd else 0.0,
        "max_drawdown": maxdd, "calmar": cagr / abs(maxdd) if maxdd else 0.0,
        "weeks": n, "years": years,
        "win_rate": sum(1 for r in rets if r > 0) / n,
        "best_week": max(rets), "worst_week": min(rets),
        "t_stat": m / (sd / math.sqrt(n)),
        "btc_return": btc[-1][1] - 1, "btc_maxdd": min(x for _, x in bdd),
    },
    "exposure": {
        "gross": [[r["date"], r["gross"]] for r in ledger],
        "net": [[r["date"], r["net"]] for r in ledger],
        "max_gross": max(r["gross"] for r in ledger),
        "mean_gross": statistics.mean(r["gross"] for r in ledger),
        "max_net_long": max(r["net"] for r in ledger),
        "max_net_short": min(r["net"] for r in ledger),
        "mean_abs_net": statistics.mean(abs(r["net"]) for r in ledger),
        "max_name": max(r["max_name"] for r in ledger),
        "leverage_note": f"gross never exceeds the {GROSS:g} target; the short leg is the "
                         f"only margin user",
    },
    "stats": {
        "mean_long_names": statistics.mean(r["n_long"] for r in live),
        "mean_short_names": statistics.mean(r["n_short"] for r in live),
        "mean_turnover": statistics.mean(r["turnover"] for r in ledger),
        "total_cost": sum(r["cost"] for r in ledger),
        "total_funding": sum(r["funding"] for r in ledger),
        "from_long": sum(r["from_long"] for r in ledger),
        "from_short": sum(r["from_short"] for r in ledger),
        "flat_weeks": sum(1 for r in ledger if r["flat_tranches"] == 7),
        "partial_weeks": sum(1 for r in ledger if 0 < r["flat_tranches"] < 7),
        "pct_up_regime": sum(1 for r in ledger if r["tilt"] > 0.01) / n,
        "pct_down_regime": sum(1 for r in ledger if r["tilt"] < -0.01) / n,
    },
    "series": {"compounded": comp, "fixed": fixed, "drawdown": ddc,
               "btc": btc, "btc_drawdown": bdd},
    "yearly": yearly(comp),
}
Path(sys.argv[1]).write_bytes((json.dumps(rec, indent=2) + "\n").encode())
mt = rec["metrics"]
print(f"weeks {mt['weeks']}  years {mt['years']:.1f}")
print(f"total {mt['total_return']*100:.1f}%  CAGR {mt['cagr']*100:.1f}%  "
      f"fixed-budget {mt['fixed_budget_return']*100:.1f}%")
print(f"vol {mt['volatility']*100:.1f}%  Sharpe {mt['sharpe']:.2f}  Sortino {mt['sortino']:.2f}  "
      f"maxDD {mt['max_drawdown']*100:.1f}%  Calmar {mt['calmar']:.2f}")
print(f"win rate {mt['win_rate']*100:.1f}%  best {mt['best_week']*100:+.1f}%  worst {mt['worst_week']*100:+.1f}%  t {mt['t_stat']:.2f}")
ex = rec["exposure"]
print(f"gross mean {ex['mean_gross']:.3f} max {ex['max_gross']:.3f} | "
      f"net max long {ex['max_net_long']:+.3f} max short {ex['max_net_short']:+.3f} | "
      f"biggest name {ex['max_name']:.3f}")
st = rec["stats"]
print(f"names L{st['mean_long_names']:.1f}/S{st['mean_short_names']:.1f}  "
      f"turnover {st['mean_turnover']*100:.1f}%/wk  costs {st['total_cost']*100:.1f}%  "
      f"funding {st['total_funding']*100:+.1f}%")
print(f"from long {st['from_long']*100:+.1f}%  from short {st['from_short']*100:+.1f}%  "
      f"flat weeks {st['flat_weeks']}  partial {st['partial_weeks']}")