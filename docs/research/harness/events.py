"""A feature can be flat for months and carry all its information in the jumps.

Average autocorrelation is the wrong instrument for that shape. It is dominated
by the quiet stretches, so a feature that sits still 95% of the time and moves
sharply the other 5% reads as "frozen" while every observation that matters is in
the tail. Calling those features useless on that basis was a mistake, and it is
worse for small coins, where the quiet stretches are longer.

Measured properly:

  1. The DISTRIBUTION of daily change, not its average persistence. A large gap
     between the median and the 99th percentile is the flat-then-jumps shape.
  2. Conditional IC - does a feature predict better on the days it has just
     moved than on the days it has not? If yes, the signal is in the events and
     a model that only sees levels is throwing it away.
  3. Split by liquidity, because the claim is specifically about smaller coins.

Everything is in cross-sectional RANK space, so "moved a lot" means moved
relative to the other assets that day rather than in its own units.
"""
import math, statistics, sys
from pathlib import Path
import polars as pl

SCRATCH = Path("/tmp/claude-1000/-home-magnus-dev-magnus-ai-trader/7796ca66-c3de-497c-ae48-a31006b2e8f4/scratchpad")
df = pl.read_parquet(SCRATCH / "ds.parquet").sort(["asset", "date"])
FEATS = [c[2:] for c in df.columns if c.startswith("x_")]

# Daily change in each feature's cross-sectional rank.
df = df.with_columns([
    (pl.col(f"x_{f}") - pl.col(f"x_{f}").shift(1).over("asset")).abs().alias(f"d_{f}")
    for f in FEATS
])
# Liquidity tercile, computed within each date so it is a relative size label.
df = df.with_columns(
    (pl.col("x_adv_quote").rank("average").over("date") /
     pl.col("x_adv_quote").count().over("date")).alias("liq_pct")
)

print("How much does each feature move, day to day, in rank units?")
print(f"{'feature':<18}{'median':>9}{'90th':>9}{'99th':>9}{'99th/median':>13}   shape")
shapes = {}
for f in FEATS:
    v = df[f"d_{f}"].drop_nulls().to_list()
    v = [x for x in v if math.isfinite(x)]
    if len(v) < 1000:
        continue
    v.sort()
    med = v[len(v)//2]; p90 = v[int(.90*len(v))]; p99 = v[int(.99*len(v))]
    ratio = p99 / med if med > 1e-9 else float("inf")
    shape = "flat-then-jumps" if ratio > 20 else ("bursty" if ratio > 6 else "smooth")
    shapes[f] = shape
    print(f"{f:<18}{med:>9.4f}{p90:>9.4f}{p99:>9.4f}{ratio:>13.1f}   {shape}")


def ic_of(rows, xcol):
    """Spearman of feature vs relative 1-day return, pooled per date."""
    per = {}
    for d, x, y in rows:
        per.setdefault(d, ([], []))
        per[d][0].append(x); per[d][1].append(y)
    ics = []
    for xs, ys in per.values():
        if len(xs) < 8:
            continue
        n = len(xs)
        rx = {v: i for i, v in enumerate(sorted(range(n), key=lambda i: xs[i]))}
        ry = {v: i for i, v in enumerate(sorted(range(n), key=lambda i: ys[i]))}
        a = [rx[i] for i in range(n)]; b = [ry[i] for i in range(n)]
        ma, mb = sum(a)/n, sum(b)/n
        num = sum((p-ma)*(q-mb) for p, q in zip(a, b))
        den = math.sqrt(sum((p-ma)**2 for p in a) * sum((q-mb)**2 for q in b))
        if den: ics.append(num/den)
    if len(ics) < 30:
        return None, None, 0
    m, sd = statistics.mean(ics), statistics.stdev(ics)
    return m, m/(sd/math.sqrt(len(ics))), len(ics)


print("\nDoes a feature predict BETTER on the days it has just moved?")
print("(IC vs 1-day relative return, split on whether the daily rank change is")
print(" in the top decile for that feature)")
print(f"\n{'feature':<18}{'quiet IC':>10}{'quiet t':>9}{'MOVED IC':>10}{'MOVED t':>9}   read")
for f in FEATS:
    sub = df.select(["date", f"x_{f}", f"d_{f}", "t1"]).drop_nulls()
    if sub.height < 3000:
        continue
    thr = sub[f"d_{f}"].quantile(0.90)
    if thr is None or thr <= 0:
        continue
    quiet = [(d, x, y) for d, x, dd, y in sub.iter_rows() if dd <= thr]
    moved = [(d, x, y) for d, x, dd, y in sub.iter_rows() if dd > thr]
    qm, qt, qn = ic_of(quiet, f)
    mm, mt, mn = ic_of(moved, f)
    if qm is None or mm is None:
        continue
    read = ("EVENT-DRIVEN" if abs(mm) > abs(qm) * 1.5 and abs(mt) > 2
            else "level-driven" if abs(qm) > abs(mm) * 1.5
            else "similar")
    print(f"{f:<18}{qm:>+10.4f}{qt:>+9.2f}{mm:>+10.4f}{mt:>+9.2f}   {read}")

print("\nSame test, SMALL coins only (bottom liquidity third)")
small = df.filter(pl.col("liq_pct") <= 0.33)
print(f"{'feature':<18}{'quiet IC':>10}{'MOVED IC':>10}{'MOVED t':>9}")
for f in FEATS:
    sub = small.select(["date", f"x_{f}", f"d_{f}", "t1"]).drop_nulls()
    if sub.height < 2000:
        continue
    thr = sub[f"d_{f}"].quantile(0.90)
    if thr is None or thr <= 0:
        continue
    qm, _, _ = ic_of([(d, x, y) for d, x, dd, y in sub.iter_rows() if dd <= thr], f)
    mm, mt, _ = ic_of([(d, x, y) for d, x, dd, y in sub.iter_rows() if dd > thr], f)
    if qm is None or mm is None:
        continue
    print(f"{f:<18}{qm:>+10.4f}{mm:>+10.4f}{mt:>+9.2f}")