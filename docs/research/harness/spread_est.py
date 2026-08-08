"""Estimate the effective spread the book actually pays, per asset.

The cost model charges a flat 2bps half-spread, described in config as "a
placeholder for liquid majors". This book trades alts at a one-day holding
period and turns over 322x NAV a year, so if that placeholder is wrong the whole
result moves with it.

Quotes are not in the store and never will be for history, but the effective
spread is recoverable from OHLC. Two standard estimators, both computed so they
can be checked against each other:

  Abdi-Ranaldo (2017)   S = 2*sqrt(max(0, E[(c_t - m_t)(c_t - m_{t+1})]))
                        with c the log close and m = (log high + log low)/2.
                        Robust and needs only consecutive bars.
  Corwin-Schultz (2012) uses the ratio of a two-period high-low range to two
                        one-period ranges: a wider combined range than two
                        singles implies a spread rather than volatility.

Validated on BTC first. The true BTC spread is known to be around 1bp, so an
estimator returning 30bps there is measuring volatility, not spread, and its
alt numbers cannot be trusted either.

Both estimate the FULL spread. A taker crosses half of it, so the comparison
against config's `spread_bps` is estimate/2.
"""
import math, statistics
from pathlib import Path
import numpy as np
import polars as pl
from planner import store
from planner.config import Config

ROOT = Path("/home/magnus/dev/magnus/ai-trader"); DATA = ROOT / "data"
cfg = Config.load(ROOT / "config" / "default.toml")
h = store.read(root=DATA, interval_s=3600).sort(["asset", "ts_utc"])
print(f"{h.height:,} hourly bars, {h['asset'].n_unique()} assets")

lo_ok = (pl.col("low") > 0) & (pl.col("high") > 0) & (pl.col("close") > 0)
h = h.filter(lo_ok).with_columns([
    pl.col("high").log().alias("lh"),
    pl.col("low").log().alias("ll"),
    pl.col("close").log().alias("lc"),
])
h = h.with_columns(((pl.col("lh") + pl.col("ll")) / 2).alias("m"))
h = h.with_columns([
    pl.col("m").shift(-1).over("asset").alias("m_next"),
    pl.col("lh").shift(-1).over("asset").alias("lh_next"),
    pl.col("ll").shift(-1).over("asset").alias("ll_next"),
    pl.col("ts_utc").diff().over("asset").dt.total_seconds().shift(-1).alias("dt_next"),
])
# Only consecutive bars: a gap across a delisting is not a two-period window.
h = h.filter(pl.col("dt_next") == 3600).drop_nulls(["m_next", "lh_next", "ll_next"])


def abdi_ranaldo(g):
    """2*sqrt(E[(c-m)(c-m_next)]), clipped at zero where the covariance is negative."""
    v = ((g["lc"] - g["m"]) * (g["lc"] - g["m_next"])).to_numpy()
    v = v[np.isfinite(v)]
    if len(v) < 200:
        return None
    return 2.0 * math.sqrt(max(0.0, float(v.mean())))


def corwin_schultz(g):
    beta = ((g["lh"] - g["ll"]) ** 2 + (g["lh_next"] - g["ll_next"]) ** 2).to_numpy()
    hi2 = np.maximum(g["lh"].to_numpy(), g["lh_next"].to_numpy())
    lo2 = np.minimum(g["ll"].to_numpy(), g["ll_next"].to_numpy())
    gamma = (hi2 - lo2) ** 2
    k = 3 - 2 * math.sqrt(2)
    with np.errstate(invalid="ignore"):
        alpha = (np.sqrt(2 * beta) - np.sqrt(beta)) / k - np.sqrt(gamma / k)
    s = 2 * (np.exp(alpha) - 1) / (1 + np.exp(alpha))
    s = s[np.isfinite(s)]
    s = s[(s > -0.5) & (s < 0.5)]
    if len(s) < 200:
        return None
    # Negative estimates are noise around a small true spread; the standard
    # treatment is to floor them at zero before averaging.
    return float(np.maximum(s, 0).mean())


rows = []
for (asset,), g in h.partition_by("asset", as_dict=True).items():
    ar, cs = abdi_ranaldo(g), corwin_schultz(g)
    if ar is None or cs is None:
        continue
    adv = float(g["quote_volume"].median()) * 24
    rows.append((asset, ar, cs, adv, g.height))

rows.sort(key=lambda r: -r[3])
print(f"\nestimated FULL spread, hourly bars, {len(rows)} assets")
print(f"{'asset':<10}{'Abdi-Ran':>11}{'Corwin-S':>11}{'daily $vol':>14}{'bars':>9}")
for a, ar, cs, adv, n in rows[:8]:
    print(f"{a:<10}{ar*10000:>10.1f}b{cs*10000:>10.1f}b{adv:>14,.0f}{n:>9,}")
print("  ...")
for a, ar, cs, adv, n in rows[-6:]:
    print(f"{a:<10}{ar*10000:>10.1f}b{cs*10000:>10.1f}b{adv:>14,.0f}{n:>9,}")

ars = sorted(r[1] for r in rows)
css = sorted(r[2] for r in rows)
def q(v, p): return v[int(p * (len(v) - 1))]
print(f"\n{'':<14}{'median':>10}{'75th':>10}{'90th':>10}{'99th':>10}")
print(f"{'Abdi-Ranaldo':<14}{q(ars,.5)*10000:>9.1f}b{q(ars,.75)*10000:>9.1f}b"
      f"{q(ars,.90)*10000:>9.1f}b{q(ars,.99)*10000:>9.1f}b")
print(f"{'Corwin-Schultz':<14}{q(css,.5)*10000:>9.1f}b{q(css,.75)*10000:>9.1f}b"
      f"{q(css,.90)*10000:>9.1f}b{q(css,.99)*10000:>9.1f}b")

btc = [r for r in rows if r[0] == "BTC"]
if btc:
    _, ar, cs, _, _ = btc[0]
    print(f"\nvalidation - BTC true spread is ~1bp:")
    print(f"  Abdi-Ranaldo   {ar*10000:.1f}bp full / {ar*10000/2:.1f}bp half")
    print(f"  Corwin-Schultz {cs*10000:.1f}bp full / {cs*10000/2:.1f}bp half")
    print("  An estimator far above ~2bp here is reading volatility, not spread.")

print(f"\nconfig assumes a {cfg.costs.spread_bps}bp HALF-spread, i.e. "
      f"{float(cfg.costs.spread_bps)*2:.0f}bp full")