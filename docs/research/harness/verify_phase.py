"""Is the Friday result a BUG, or is the strategy simply that noisy?

I called it a "sampling artifact" without establishing which. Two very different
claims:

  BUG      - something in the code or data makes Friday genuinely special
             (funding accrual aligned to a weekday, missing bars on some days,
             a forward return computed off-by-one for certain offsets).
  NOISE    - each phase is a near-independent sample of a strategy whose
             per-trade noise swamps its edge, and Friday was the lucky draw.

The discriminating measurement is the MEAN WEEKLY RETURN, not the compounded
total. Compounding turns a small difference in mean into an enormous difference
in total, so comparing totals cannot separate the two. If the seven phases agree
on the mean within sampling error, there is no bug - only noise being amplified
by compounding. If they disagree beyond it, something is structurally different
about certain weekdays and that is a bug worth finding.

Also checks the two data-side mechanisms that COULD make a weekday special.
"""
import math, statistics
from collections import Counter
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
STEP = 7
cfg = Config.load(ROOT / "config" / "default.toml")
MAXPOS = float(cfg.limits.max_position)
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
CAP, SCALE = 0.5, 8.0
DAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")

print("=" * 78)
print("A. DATA-SIDE CHECKS - could a weekday be structurally different?")
print("=" * 78)
wd = Counter(d.weekday() for d in bars["ts_utc"].unique().to_list())
print(f"  bar dates by weekday : {{{', '.join(f'{DAYS[k]} {wd[k]}' for k in range(7))}}}")
fd = Counter(d.weekday() for d in fund["day"].unique().to_list())
print(f"  funding days by wkday: {{{', '.join(f'{DAYS[k]} {fd[k]}' for k in range(7))}}}")
snaps = universe.snapshots(root=DATA)
sd = Counter(d.weekday() for d in snaps)
print(f"  universe snapshots   : {{{', '.join(f'{DAYS[k]} {sd[k]}' for k in range(7))}}}")
print("  -> flat across weekdays means no structural asymmetry in the inputs")


def run(S, E):
    prev, rets = {}, []
    day = S
    while day <= E:
        hz = day - timedelta(seconds=cfg.interval_s)
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
        b = BENCH.get(hz); t = 0.0
        if b and b["gc_regime_upper"] is not None and b["gc_regime_slope"] is not None:
            sg = 1.0 if b["close"] > b["gc_regime_upper"] else (-1.0 if b["close"] < b["gc_regime_filter"] else 0.0)
            t = max(-CAP, min(CAP, sg * abs(b["gc_regime_slope"]) * SCALE))
        wl, ws = 0.5 + t, 0.5 - t
        if len(L) < 3 or len(Sh) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rets.append(-turn * COST / 10_000); day += timedelta(days=STEP); continue
        if wl / len(L) > MAXPOS: wl = MAXPOS * len(L); ws = 1.0 - wl
        if ws / len(Sh) > MAXPOS: ws = MAXPOS * len(Sh); wl = 1.0 - ws
        fwd = ic._forward_returns(prices, L + Sh, day, STEP)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        lr = [tab[a] for a in L if a in tab]; sr = [tab[a] for a in Sh if a in tab]
        if len(lr) < 3 or len(sr) < 3:
            turn = sum(abs(v) for v in prev.values()); prev = {}
            rets.append(-turn * COST / 10_000); day += timedelta(days=STEP); continue
        g = wl * (sum(lr) / len(lr)) - ws * (sum(sr) / len(sr))
        fp = ws * (sum(hf(a, day) for a in Sh) / len(Sh))
        w = {a: wl / len(L) for a in L}
        for a in Sh: w[a] = w.get(a, 0.0) - ws / len(Sh)
        turn = sum(abs(w.get(a, 0.0) - prev.get(a, 0.0)) for a in set(w) | set(prev)); prev = w
        rets.append(g + fp - turn * COST / 10_000)
        day += timedelta(days=STEP)
    return rets


def hf(a, day):
    t = ftab.get(a)
    return 0.0 if not t else sum(t.get(day + timedelta(days=k), 0.0) for k in range(STEP))


U = timezone.utc
BO, END = datetime(2021, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
print()
print("=" * 78)
print("B. DO THE PHASES DISAGREE ABOUT THE EDGE, OR ONLY ABOUT THE PATH?")
print("=" * 78)
print(f"{'phase':<7}{'n':>5}{'mean wk':>10}{'SE':>9}{'t':>7}{'sd wk':>9}"
      f"{'sum of r':>11}{'compounded':>13}")
means, ses, sums = [], [], []
for k in range(7):
    r = run(BO + timedelta(days=k), END)
    m, sd_ = statistics.mean(r), statistics.stdev(r)
    se = sd_ / math.sqrt(len(r))
    eq = 1.0
    for x in r: eq *= 1 + x
    means.append(m); ses.append(se); sums.append(sum(r))
    print(f"{DAYS[(BO + timedelta(days=k)).weekday()]:<7}{len(r):>5}{m*100:>9.3f}%{se*100:>8.3f}%"
          f"{m/se:>7.2f}{sd_*100:>8.2f}%{sum(r)*100:>10.1f}%{(eq-1)*100:>12.1f}%")

spread = max(means) - min(means)
pooled_se = statistics.mean(ses)
print()
print(f"  spread in MEAN weekly return : {spread*100:.3f} pts")
print(f"  typical standard error       : {pooled_se*100:.3f} pts")
print(f"  spread / SE                  : {spread/pooled_se:.2f}")
print()
print("  The phases are NOT independent - they cover the same calendar period, so")
print("  their means are correlated and a spread of a couple of SE is unremarkable.")
print(f"  mean of means {statistics.mean(means)*100:.3f}%/wk, "
      f"sd of means {statistics.stdev(means)*100:.3f}%/wk")
print()
lo, hi = min(sums), max(sums)
print(f"  but the SUM ranges {lo*100:.0f}%..{hi*100:.0f}% and compounding turns that into")
print(f"  the headline spread: a {spread*100:.3f}pt/wk difference over ~250 weeks is")
print(f"  {spread*250*100:.0f} pts of cumulative return before compounding.")