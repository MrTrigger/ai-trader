"""Which half of the strategy is doing the work - the selection or the timing?

The channel spread over the full window is flat (+0.14%/wk at 7d, t=+0.27), so
the cross-sectional selection may be contributing nothing and the return may be
coming entirely from the benchmark regime tilt. If so the honest description is
not "market-neutral long/short with a regime overlay" but "BTC market timing",
and the correct version to ship is far simpler than what has been built.

Four variants, each tranched across all seven rebalance phases so no result
depends on an arbitrary weekday:

  SELECTION ONLY  legs from the channel, no tilt      - is the cross-section worth anything?
  TIMING ONLY     long/short BTC on the regime read   - is the tilt the whole story?
  BOTH            the shipped strategy
  BTC             buy and hold                        - the bar to clear

If TIMING ONLY matches BOTH, the selection is decoration and should be deleted.
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
DAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")


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
            # Net exposure to BTC equal to what the tilt would put on the book.
            px_now, px_next = BTC_PX.get(day), BTC_PX.get(day + timedelta(days=STEP))
            net = 2 * t                      # long 0.5+t minus short 0.5-t
            r = 0.0 if (px_now is None or px_next is None) else net * (px_next / px_now - 1)
            turn = abs(net - prev.get("net", 0.0)); prev = {"net": net}
            rows.append((day, r - turn * COST / 10_000))
            day += timedelta(days=STEP); continue

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
    """Equal-weight the seven phases, binned to a common calendar week."""
    per = {}
    for k in range(7):
        for d, r in run(S + timedelta(days=k), E, mode):
            per.setdefault(d.isocalendar()[:2], []).append(r)
    weeks = sorted(w for w, v in per.items() if len(v) == 7)
    return [statistics.mean(per[w]) for w in weeks], weeks


def stats(rets):
    if len(rets) < 2:
        return None
    eq, pk, dd = 1.0, 1.0, 0.0
    for r in rets:
        eq *= 1 + r; pk = max(pk, eq); dd = min(dd, eq / pk - 1)
    m, sd = statistics.mean(rets), statistics.stdev(rets)
    return {"n": len(rets), "final": eq - 1, "mean": m, "sd": sd,
            "sharpe": (m * 52) / (sd * math.sqrt(52)) if sd else 0.0,
            "maxdd": dd, "t": m / (sd / math.sqrt(len(rets))) if sd else 0.0}


U = timezone.utc
W = {"fresh 2019-10..2021-10": (datetime(2019,10,1,tzinfo=U), datetime(2021,10,1,tzinfo=U)),
     "orig  2021-10..2026-08": (datetime(2021,10,1,tzinfo=U), datetime(2026,8,1,tzinfo=U))}
MODES = [("BTC buy & hold", "btc"), ("selection only (no tilt)", "selection"),
         ("timing only (BTC)", "timing"), ("BOTH - as shipped", "both")]

out = {}
for wname, (S, E) in W.items():
    print(f"\n=== {wname}  (tranched across all 7 phases) ===")
    print(f"{'variant':<28}{'n':>5}{'return':>11}{'mean wk':>10}{'Sharpe':>9}{'maxDD':>9}{'t':>7}")
    out[wname] = {}
    for label, mode in MODES:
        rets, _ = tranche(S, E, mode)
        st = stats(rets)
        out[wname][label] = st
        print(f"{label:<28}{st['n']:>5}{st['final']*100:>10.1f}%{st['mean']*100:>9.3f}%"
              f"{st['sharpe']:>9.2f}{st['maxdd']*100:>8.1f}%{st['t']:>7.2f}")
        sys.stdout.flush()

Path(sys.argv[1]).write_bytes((json.dumps(out, indent=2) + "\n").encode())