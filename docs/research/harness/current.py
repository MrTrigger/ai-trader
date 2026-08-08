"""Produce the CURRENT result: one strategy, tranched, with its decomposition.

The page is a dashboard, not an archive. It should answer "what does the current
best version do" and "how much of that is trustworthy", and nothing else. The
history of everything tried lives in docs/phase-1-findings.md.
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
MAXPOS = float(cfg.limits.max_position); MAXCOUNT = cfg.limits.max_position_count
COST = float(cfg.costs.commission_bps + cfg.costs.spread_bps)
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
CAP, SCALE = 0.5, 8.0


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


def tilt_at(hz):
    b = BENCH.get(hz)
    if b is None or b["gc_regime_upper"] is None or b["gc_regime_slope"] is None:
        return 0.0
    sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
    return max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))


def run(S, E, mode):
    prev, rows = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
        t = tilt_at(hz)
        if mode == "btc":
            px, prv = BTC_PX.get(day), prev.get("px")
            rows.append((day, 0.0 if (prv is None or px is None) else px / prv - 1))
            if px is not None: prev = {"px": px}
            day += timedelta(days=STEP); continue
        if mode == "timing":
            a, b = BTC_PX.get(day), BTC_PX.get(day + timedelta(days=STEP))
            net = 2 * t
            r = 0.0 if (a is None or b is None) else net * (b / a - 1)
            turn = abs(net - prev.get("net", 0.0)); prev = {"net": net}
            rows.append((day, r - turn * COST / 10_000)); day += timedelta(days=STEP); continue
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
        if mode == "selection":
            t = 0.0
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < 3 or len(Sh) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue
        if len(L) + len(Sh) > MAXCOUNT:
            nl = max(3, min(len(L), round(MAXCOUNT * wl))); ns = max(3, min(len(Sh), MAXCOUNT - nl))
            L, Sh = L[:nl], Sh[:ns]
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rows.append((day, -turn * COST / 10_000)); day += timedelta(days=STEP); continue
        g = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        w = {a: wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rows.append((day, g + fp - turn * COST / 10_000))
        day += timedelta(days=STEP)
    return rows


def tranche(S, E, mode):
    per = {}
    for k in range(7):
        for d, r in run(S + timedelta(days=k), E, mode):
            per.setdefault(d.isocalendar()[:2], []).append((d, r))
    weeks = sorted(w for w, v in per.items() if len(v) == 7)
    return [(min(d for d, _ in per[w]).date().isoformat(),
             statistics.mean(r for _, r in per[w])) for w in weeks]


def curve(rows):
    eq, out = 1.0, []
    for d, r in rows:
        eq *= 1 + r; out.append([d, eq])
    return out


def stats(rows):
    rets = [r for _, r in rows]
    eq, pk, dd, ddc = 1.0, 1.0, 0.0, []
    for d, r in rows:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq / pk - 1); ddc.append([d, eq / pk - 1])
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"n": len(rets), "final": eq - 1, "mean_week": m, "sd_week": sd,
            "sharpe": (m * 52) / (sd * math.sqrt(52)) if sd else 0.0, "maxdd": dd,
            "t": m / (sd / math.sqrt(len(rets))) if sd else 0.0, "drawdown": ddc}


U = timezone.utc
FRESH = (datetime(2019,10,1,tzinfo=U), datetime(2021,10,1,tzinfo=U))
ORIG = (datetime(2021,10,1,tzinfo=U), datetime(2026,8,1,tzinfo=U))
MODES = [("strategy", "both"), ("BTC buy & hold", "btc"),
         ("selection only", "selection"), ("timing only", "timing")]

rec = {"windows": {}, "series": [], "decomposition": []}
chained = {}
for label, mode in MODES:
    f = tranche(*FRESH, mode); o = tranche(*ORIG, mode)
    sf, so = stats(f), stats(o)
    cf = curve(f); tail = cf[-1][1]
    co = [[d, v * tail] for d, v in curve(o)]
    chained[label] = cf + co
    rec["decomposition"].append({
        "name": label,
        "fresh": {k: sf[k] for k in ("final","sharpe","maxdd","t","mean_week","n")},
        "orig": {k: so[k] for k in ("final","sharpe","maxdd","t","mean_week","n")},
        "combined": (1 + sf["final"]) * (1 + so["final"]) - 1,
    })
    print(f"{label:<18} fresh {sf['final']*100:>8.1f}% Sh {sf['sharpe']:>5.2f} | "
          f"orig {so['final']*100:>8.1f}% Sh {so['sharpe']:>5.2f} | "
          f"combined {((1+sf['final'])*(1+so['final'])-1)*100:>9.1f}%")
    sys.stdout.flush()

for label in ("strategy", "BTC buy & hold"):
    eq = chained[label]
    pk, ddc = eq[0][1], []
    for d, v in eq:
        pk = max(pk, v); ddc.append([d, v / pk - 1])
    rec["series"].append({"label": label, "equity": eq, "drawdown": ddc,
                          "final": eq[-1][1] - 1, "maxdd": min(x for _, x in ddc)})
rec["split"] = "2021-10-01"
rec["window"] = [rec["series"][0]["equity"][0][0], rec["series"][0]["equity"][-1][0]]
Path(sys.argv[1]).write_bytes((json.dumps(rec, indent=2) + "\n").encode())
print("\nWROTE", sys.argv[1])