"""Week-by-week ledger for the final drawdown, from the book rather than the curve.

The chart shows the strategy falling at the end and the obvious reading is "it
failed to short a falling market". The dates say otherwise - BTC fell from ~18x
to ~11x while the strategy ROSE to its peak, and the loss came afterwards while
BTC was flat. So the question is not "why didn't it short" but "what was it
holding, and which leg lost".

Prints, per week: the regime state and tilt, how many names on each side, what
each leg actually returned, and where the week's P&L came from. A flat book shows
as nL/nS = 0.
"""
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


U = timezone.utc
S, E = datetime(2025, 11, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
print(f"{'week':<12}{'BTC':>7}{'state':>6}{'tilt':>7}{'nL':>4}{'nS':>4}"
      f"{'longR':>8}{'shortR':>8}{'from L':>8}{'from S':>8}{'fund':>7}{'cost':>7}{'week':>8}{'cum':>9}")
eq = 1.0
day = S
while day <= E:
    hz = day - timedelta(seconds=cfg.interval_s)
    b = BENCH.get(hz); t = 0.0; state = "-"
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
    px_a, px_b = BTC_PX.get(day), BTC_PX.get(day + timedelta(days=STEP))
    btc_r = (px_b / px_a - 1) if (px_a and px_b) else 0.0
    wl, ws = 0.5 + t, 0.5 - t
    if len(L) < 3 or len(Sh) < 3:
        print(f"{day.date()}  {btc_r*100:>6.1f}%{state:>6}{t:>+7.2f}{len(L):>4}{len(Sh):>4}"
              f"{'':>8}{'':>8}{'':>8}{'':>8}{'':>7}{'':>7}{'FLAT':>8}{eq:>9.3f}")
        day += timedelta(days=STEP); continue
    if len(L) + len(Sh) > MAXCOUNT:
        nl = max(3, min(len(L), round(MAXCOUNT * wl))); ns = max(3, min(len(Sh), MAXCOUNT - nl))
        L, Sh = L[:nl], Sh[:ns]
    if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
    if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
    fwd = ic._forward_returns(prices, L + Sh, day, STEP)
    tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
    lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
    if len(lr) < 3 or len(sr) < 3:
        day += timedelta(days=STEP); continue
    lmean, smean = sum(lr)/len(lr), sum(sr)/len(sr)
    from_l, from_s = wl*lmean, -ws*smean
    fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
    cost = 0.30 * COST / 10_000   # typical weekly turnover, for scale
    wk = from_l + from_s + fp - cost
    eq *= 1 + wk
    print(f"{day.date()}  {btc_r*100:>6.1f}%{state:>6}{t:>+7.2f}{len(L):>4}{len(Sh):>4}"
          f"{lmean*100:>7.1f}%{smean*100:>7.1f}%{from_l*100:>7.2f}%{from_s*100:>7.2f}%"
          f"{fp*100:>6.2f}%{-cost*100:>6.2f}%{wk*100:>7.2f}%{eq:>9.3f}")
    day += timedelta(days=STEP)