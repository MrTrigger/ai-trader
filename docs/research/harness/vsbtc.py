"""Why does a dollar-neutral book track BTC, and what would it take to beat it?

Two questions from the equity chart. The curves run together from 2022-09 to
2025-04 and then separate sharply, which for a book with zero net exposure needs
explaining - dollar-neutral is not beta-neutral, since the long and short legs
can carry different betas.

And "keeping pace" is only disappointing if the two are taking the same risk.
They are not, so the comparison at equal notional is the wrong one: what matters
is what the strategy returns when scaled to the benchmark's volatility.

Also tests the likeliest reason a cross-sectional book does better in some
periods than others: it monetises the SPREAD between winners and losers, so its
edge should scale with cross-sectional dispersion, which rises in volatile and
falling markets and collapses in calm ones.
"""
import json, math, statistics
from datetime import datetime, timedelta, timezone
from pathlib import Path
import numpy as np
import polars as pl
from planner import store
from planner.config import Config

S = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
DATA = Path("/home/magnus/dev/magnus/ai-trader/data")
cfg = Config.load(Path("/home/magnus/dev/magnus/ai-trader/config/default.toml"))
rec = json.loads(Path("docs/research/backtest.json").read_text())

comp = rec["series"]["compounded"]; btcc = rec["series"]["btc"]
sr = [comp[i][1]/comp[i-1][1]-1 for i in range(1, len(comp))]
br = [btcc[i][1]/btcc[i-1][1]-1 for i in range(1, len(btcc))]
dates = [comp[i][0] for i in range(1, len(comp))]
n = len(sr)

def ann(x): return statistics.mean(x)*365, statistics.stdev(x)*math.sqrt(365)
sm, ss = ann(sr); bm, bs = ann(br)
print(f"{'':<12}{'ann return':>12}{'ann vol':>10}{'Sharpe':>9}")
print(f"{'strategy':<12}{sm*100:>11.1f}%{ss*100:>9.1f}%{sm/ss:>9.2f}")
print(f"{'BTC':<12}{bm*100:>11.1f}%{bs*100:>9.1f}%{bm/bs:>9.2f}")
print(f"\nthe strategy runs at {ss/bs:.2f}x BTC's volatility")

# --- beta -------------------------------------------------------------------
mb = statistics.mean(br); ms = statistics.mean(sr)
cov = sum((x-mb)*(y-ms) for x, y in zip(br, sr))/n
beta = cov/statistics.pvariance(br)
alpha = ms - beta*mb
print(f"\nregression of strategy on BTC, daily:")
print(f"  beta  {beta:+.3f}      alpha {alpha*365*100:+.1f}%/yr")
resid = [y - (alpha + beta*x) for x, y in zip(br, sr)]
print(f"  residual vol {statistics.stdev(resid)*math.sqrt(365)*100:.1f}%/yr   "
      f"R^2 {1 - statistics.pvariance(resid)/statistics.pvariance(sr):.3f}")
print("  A dollar-neutral book is not beta-neutral: the legs carry different betas.")

# --- what leverage to BTC's risk would have produced -------------------------
k = bs/ss
lev = [k*x for x in sr]
eq = 1.0
for x in lev: eq *= 1+x
print(f"\nscaled to BTC's volatility ({k:.2f}x leverage, gross "
      f"{float(cfg.target_gross_exposure)*k:.2f}x NAV):")
print(f"  return {(eq-1)*100:,.0f}%   vs BTC {(btcc[-1][1]-1)*100:.0f}%")
pk, dd = 1.0, 0.0
e = 1.0
for x in lev:
    e *= 1+x; pk = max(pk, e); dd = min(dd, e/pk-1)
print(f"  maxDD {dd*100:.1f}%   vs BTC {rec['metrics']['btc_maxdd']*100:.1f}%")

# --- up-market versus down-market -------------------------------------------
print("\nsplit by what BTC did over the same day:")
for label, sel in (("BTC up days", [i for i in range(n) if br[i] > 0]),
                   ("BTC down days", [i for i in range(n) if br[i] <= 0])):
    s_ = [sr[i] for i in sel]; b_ = [br[i] for i in sel]
    print(f"  {label:<15} {len(sel):>5} days   strategy {statistics.mean(s_)*100:+.3f}%/day"
          f"   BTC {statistics.mean(b_)*100:+.3f}%/day")

# --- is the edge dispersion-driven? -----------------------------------------
ds = pl.read_parquet(S / "ds4.parquet")
disp = (ds.group_by("date").agg(pl.col("y").std().alias("d"))
        .sort("date"))
dmap = dict(zip(disp["date"].to_list(), disp["d"].to_list()))
pairs = [(dmap[d], r) for d, r in zip(dates, sr) if dmap.get(d) is not None]
pairs.sort()
q = len(pairs)//4
print(f"\nstrategy return by cross-sectional DISPERSION quartile "
      f"({len(pairs)} days):")
for i, lab in enumerate(("Q1 lowest", "Q2", "Q3", "Q4 highest")):
    blk = pairs[i*q:(i+1)*q] if i < 3 else pairs[3*q:]
    m_ = statistics.mean(r for _, r in blk)
    sd_ = statistics.stdev(r for _, r in blk)
    print(f"  {lab:<12} dispersion {statistics.median(d for d, _ in blk)*100:>5.2f}%"
          f"   mean {m_*100:+.3f}%/day   ann {m_*365*100:>7.1f}%   "
          f"Sharpe {m_*365/(sd_*math.sqrt(365)):>5.2f}")
xs = [d for d, _ in pairs]; ys = [r for _, r in pairs]
mx, my = statistics.mean(xs), statistics.mean(ys)
c = sum((a-mx)*(b-my) for a, b in zip(xs, ys))/len(xs)
rho = c/(statistics.pstdev(xs)*statistics.pstdev(ys))
print(f"  correlation dispersion vs return {rho:+.3f}  "
      f"t {rho*math.sqrt((len(xs)-2)/max(1e-9, 1-rho*rho)):+.2f}")