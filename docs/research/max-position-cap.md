# `max_position` — how much single-name risk the book actually runs, and what a tighter cap costs

Pre-registered 2026-08-19 before the replays. Context: the E/F experiments
(`docs/research/absolute-label-unbalanced.md`) showed a label that thins one
side can put a single name at the 0.25 cap (DEXE −39% day). Question: does
the *frozen* book run that risk, and is 0.25 the right number?

## Measured first (frozen book, nine folds, settled replay)

Largest |weight| per rebalance (positions marked at last fill price):
p50 **8.4%**, p90 11.5%, p99 **15.4%**, max 49% (one LUNA day, pre-settlement
mark). Days with a name above 10% / 15% / 20% / 25%: 476 / 31 / 4 / 1 of
2,142. The plan's own "per-position cap binds" note fired **0** times: the
constructor never asked for more than 25% of NAV in a name. Thin side: median
11 names, ≤4 on 8 days. The cap at 0.25 is not a constraint on this book;
it is a backstop that has never been touched.

## What is run

`bin/replay-folds.sh` on the frozen models with `max_position` ∈ {0.15,
0.10} (0.25 = baseline), nothing else changed. Reported: Sharpe / return /
maxDD / turnover per fold, bootstrap Δ vs baseline, the largest-|w|
distribution, and how often the cap binds.

## Decision rule

This is a risk-appetite limit, not a signal: the question is cost. **0.15 is
adopted if its measured cost is within noise** (interval includes zero and
|Δ| < 0.05) — it removes the >15% tail the book has shown it can reach after
fills, at no measured price. 0.10 is reported for the curve and adopted only
if it is also within noise, which is not expected (it binds on ~22% of
days and flattens the edge/vol sizing). Nothing else is changed.
