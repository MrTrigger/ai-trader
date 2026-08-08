"""Does sampling daily actually give seven times the training data?

Seven times the ROWS, certainly. The question is whether they are seven times the
INFORMATION, and that depends entirely on the prediction horizon:

  predict 7d forward, sample daily   consecutive observations share 6/7 of their
                                     window. Heavily overlapping, so the extra
                                     rows are mostly restatements of the same
                                     price action.
  predict 1d forward, sample daily   non-overlapping, genuinely independent - but
                                     a different and much noisier target.

If daily sampling delivered seven independent samples, a t-statistic computed on
it would rise by sqrt(7) = 2.65x. Measured here on the strongest feature from the
screen, so the comparison is on something that definitely carries signal.

Also reports the autocorrelation of the feature itself: a feature that barely
changes day to day cannot supply independent observations however often it is
read.
"""
import math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import polars as pl
from planner import borrow, features, ic, store, universe
from planner.bars import mark_discontinuities
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
bars = store.read(root=DATA, interval_s=cfg.interval_s)
frame = features.build(bars, benchmark=cfg.benchmark, perp_listed_from=borrow.listings(root=DATA))
prices = mark_discontinuities(bars).select(["asset", "ts_utc", "mark_open"])
U = timezone.utc
START, END = datetime(2019, 10, 1, tzinfo=U), datetime(2026, 8, 1, tzinfo=U)
_C = {}


def cross_at(day):
    k = day.date()
    if k in _C: return _C[k]
    hz = day - timedelta(seconds=cfg.interval_s)
    try:
        members = universe.load(day, root=DATA)
        e = {m.asset for m in members if m.eligible}
        cx = frame.filter((pl.col("ts_utc") == hz) & pl.col("asset").is_in(list(e))
            & (pl.col("bars_available") >= cfg.min_history_bars)
            & pl.col("adv_quote").is_not_null() & (pl.col("adv_quote") >= float(cfg.min_dollar_volume))
            & pl.col("vol_30").is_not_null() & (pl.col("vol_30") >= float(cfg.min_volatility))
            & pl.col("gc_upper").is_not_null() & pl.col("perp_listed"))
    except FileNotFoundError:
        cx = None
    _C[k] = cx; return cx


def ic_series(step, horizon, feature="vol_30"):
    out, day = [], START
    while day <= END:
        cx = cross_at(day)
        if cx is None or cx.height < 10:
            day += timedelta(days=step); continue
        fwd = ic._forward_returns(prices, cx["asset"].to_list(), day, horizon)
        tab = dict(zip(fwd["asset"].to_list(), fwd["forward_return"].to_list()))
        xs, ys = [], []
        for r in cx.iter_rows(named=True):
            v, ret = r.get(feature), tab.get(r["asset"])
            if v is not None and ret is not None:
                xs.append(float(v)); ys.append(ret)
        if len(xs) >= 10:
            rho = ic.spearman(xs, ys)
            if rho is not None:
                out.append(rho)
        day += timedelta(days=step)
    return out


def report(label, series, overlap):
    n = len(series)
    m, sd = statistics.mean(series), statistics.stdev(series)
    naive_t = m / (sd / math.sqrt(n))
    eff = n / overlap
    honest_t = m / (sd / math.sqrt(eff))
    print(f"{label:<34}{n:>7}{m:>+9.4f}{naive_t:>10.2f}{eff:>10.0f}{honest_t:>10.2f}")
    return naive_t, honest_t


print("IC of vol_30, the strongest feature in the screen\n")
print(f"{'sampling / horizon':<34}{'rows':>7}{'mean IC':>9}{'naive t':>10}{'eff n':>10}{'honest t':>10}")
w7 = report("weekly sample, 7d horizon", ic_series(7, 7), 1.0)
d7 = report("daily sample, 7d horizon", ic_series(1, 7), 7.0)
d1 = report("daily sample, 1d horizon", ic_series(1, 1), 1.0)

print(f"\nif daily gave 7x independent samples, the t would rise by sqrt(7) = 2.65x")
print(f"  weekly/7d naive t   {w7[0]:>6.2f}")
print(f"  daily/7d  naive t   {d7[0]:>6.2f}   ratio {d7[0]/w7[0]:.2f}x  <- not 2.65")
print(f"  daily/7d  honest t  {d7[1]:>6.2f}   after deflating for 7x overlap")
print(f"  daily/1d  naive t   {d1[0]:>6.2f}   non-overlapping, but a different target")

# How fast does the feature itself move? A slow feature cannot supply fresh
# observations however often it is sampled.
print("\nfeature autocorrelation (does it even change day to day?)")
for feat in ("vol_30", "adv_quote", "beta_bench"):
    piv = (frame.filter(pl.col(feat).is_not_null())
           .select(["asset", "ts_utc", feat]).sort(["asset", "ts_utc"]))
    lagged = piv.with_columns(pl.col(feat).shift(1).over("asset").alias("_l")).drop_nulls()
    a = lagged[feat].to_list(); b = lagged["_l"].to_list()
    ma, mb = statistics.mean(a), statistics.mean(b)
    sa, sb = statistics.pstdev(a), statistics.pstdev(b)
    cov = sum((x-ma)*(y-mb) for x, y in zip(a, b)) / len(a)
    print(f"  {feat:<14} lag-1 day autocorrelation {cov/(sa*sb):.4f}")