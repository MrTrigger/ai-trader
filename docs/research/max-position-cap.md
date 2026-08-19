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

## Results (same day, run as registered)

| `max_position` | mean Sharpe | compounded | 9/9 | mean maxDD | turnover | days largest name >15% (post-fill) | largest \|w\| p99 | bootstrap Δ vs 0.25 |
|---|---:|---:|---|---:|---:|---:|---:|---|
| 0.25 (frozen) | 2.113 | +1189% | yes | −11.6% | 0.809 | 31 | 15.4% | — |
| 0.15 | 2.096 | +1155% | yes | −11.7% | 0.808 | 29 | 15.0% | −0.014 [−0.027, −0.004] |
| 0.10 | 1.979 | +917% | yes | −11.5% | 0.785 | 1 | 10.0% | −0.115 [−0.181, −0.049] |

**Reading.** The >15% days the frozen book shows are mark-to-market drift
after the fill — the plan never asked for more than ~15% in a name — so a
plan-time cap of 0.15 removes almost none of them (31 → 29) while costing a
small, measurable 0.014. A cap of 0.10 does remove the tail and costs 0.12
Sharpe and ~270 points of compounded return by flattening the edge/vol
sizing on roughly a fifth of days. Neither meets the pre-registered bar
(0.15's interval excludes zero and it does not achieve the goal; 0.10 is
not within noise).

**Decision.** `max_position` stays at 0.25. The single-name tail the book
runs (p99 ≈ 15%, ~1.5% of rebalances) is intraday drift bounded by the daily
cycle, not a construction choice; the construction itself sizes to 8–12% at
most. The E/F concentration failure belongs to the rejected labels, which
thin a side; the frozen ranker does not. Closed; next risk item is the one
the capital plan already names — the impact coefficient against the new
denominator.
