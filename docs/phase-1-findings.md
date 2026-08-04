# Phase 1: first candidate, and what the gate said

> Recorded because a negative result is the asset. §9 states gates as evidence,
> and evidence that is not written down gets re-litigated by whoever arrives
> next with the same idea.

**Run:** 2021-10-01 → 2026-08-01, weekly rebalance, 253 decisions.
**Universe:** 656 Binance USDT assets including 174 whose series end before
2026-07 — the delisted are present, so this is not a survivor sample.
**Verdict:** **NOT PASSED.** Three of four criteria failed.

## The candidate

Cross-sectional momentum: rank the eligible universe on the return from
**t−30d to t−7d**, hold the top 10 long, size by conviction tilt.

The skip period is deliberate — short-horizon reversal is well documented in
crypto, and a plain 30-day measure contains last week's reversal, so the two
effects partially cancel. The stated mechanism was slow information diffusion in
a market with high retail participation.

Long-only, because shorting spot needs margin and §9.2 puts leverage above 1×
out of scope before Phase 3.

## The result

| | n | return | CAGR | vol | Sharpe | maxDD | turnover | cost bps |
|---|---|---|---|---|---|---|---|---|
| candidate | 253 | −86.87% | −34.30% | 37.5% | −0.93 | −91.89% | 28.16% | 356 |
| at 2× slippage | 253 | −89.14% | −36.83% | 38.8% | −0.98 | −93.20% | 29.01% | 367 |
| baseline (`liquidity_top`) | 253 | −60.52% | −17.49% | 52.5% | −0.10 | −79.24% | 13.20% | 167 |

**It lost, and it lost substantially worse than the baseline.** That second half
is the informative part. A long-only crypto book beginning near the 2021 cycle
top was going to lose over this window regardless — the baseline lost 60% too.
What the ranking added was a further 26 points of loss, at **twice the turnover
and twice the cost drag**.

Costs are not the explanation. Total cost drag is 356bps — 3.6% — against an
86.87% loss. The signal is the explanation.

## Robustness: it is not the risk limit

`max_benchmark_beta = 1.00` rejected 101 of 253 plans, and the typical breach
was 1.0007 against 1.00 — 0.07%. That threshold sat *exactly* on the strategy's
natural operating point (0.80 gross × ~1.25 average beta ≈ 1.00), so 40% of
plans were vetoed on rounding noise.

That is a badly chosen number, and it was corrected. It is **not** why the
strategy lost:

| beta limit | rejected | return | Sharpe |
|---|---|---|---|
| ≤ 1.00 | 101 | −86.87% | −0.93 |
| ≤ 1.50 | 14 | −86.64% | −0.78 |
| unenforced | 0 | −86.94% | −0.75 |

Invariant to within a quarter of a percent. The gate's verdict stands under any
of the three.

## What §6.2 predicted, and what actually happened

The stated failure mode was that long-only crypto momentum is a leveraged BTC
bet whose attribution would call it alpha. The measured average beta of the held
book is ~1.25, and at 0.80 gross that puts portfolio beta at ~1.00 — so the
diagnosis was right about the *exposure*. It was optimistic about the outcome:
the book did not quietly earn beta and call it alpha, it underperformed beta.

A corollary worth keeping: **for a long-only book, `max_benchmark_beta` is
close to a restatement of the gross limit.** Its value arrives with shorts.
Until then it constrains little and can bind on noise, which is what happened.

## What this does not establish

- **Not "momentum does not work in crypto."** One window, one horizon (30/7),
  one holding count (10), one rebalance cadence (weekly), one venue's universe.
  The plateau sweeps exist precisely so those are not asserted from one point.
- **Not a statement about edge.** §7.5 is blunt that no run of this length
  establishes a modest edge in either direction. What it *can* establish is
  gross failure, which is far cheaper to detect — and that is what this is.
- **The window is unkind.** It starts near the 2021 cycle top. A different start
  would give a different number; the *relative* comparison against the baseline
  travels better than the absolute one.

## The next diagnostic, and why it is not "try another strategy"

**Measure the signal, not the portfolio** (§7.5). Portfolio P&L yields one
observation per rebalance — 253 here. The information coefficient, the rank
correlation between the score and subsequent returns, yields one *per asset per
period*: ~30 names × 253 periods is ~7,500 observations rather than 253. That
is roughly 30× the information per unit of calendar time, and it answers the
question that actually matters — does this rank assets better than chance?

The gate reports the portfolio and says so. It does not compute the IC. That is
the cheapest next thing to know, and it splits the outcome cleanly:

- **IC ≈ 0** → the signal is dead. Delete it and pick a different hypothesis.
- **IC > 0 but the portfolio loses** → the ranking works and the *construction*
  destroys it: sizing, holding count, rebalance cadence or the constraint set.
  That is a different and much more tractable problem.

Doing the sweeps before knowing which of those is true would be tuning a
strategy whose signal might have no content. The IC comes first.
