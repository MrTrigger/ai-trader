# Exempting risk reduction from the turnover budget

Changed 2026-08-14 in `bc45c8a`. Exits were already exempt from
`turnover_budget`; reductions now are too, and neither consumes the allowance
that additions compete for.

## Why, and it is not the Sharpe

The budget came out fully spent every single day of the paper run — 0.4989 of
0.50 — deferring around 0.36 of intended weight. On 2026-08-13 the model made
KAITO its highest-conviction long in the book (0.2534, rank 1 of 24, target
+4.90%) and bought 7,449 units. The next morning it wanted +0.22%, rank 19 of
22. The planner asked to go 4.54% → 0.22% and the cap refused, because the
day's allowance was already gone. $869 of loss sat in a position the model had
abandoned.

A cost control that will not let you reduce risk is wrong on its own terms.
That is the argument, and it holds whatever the backtest says.

## What the folds say, and what they do not

Nine folds from `var/research/wf-rank-2020`, replayed with the same per-fold
models via `bin/replay-folds.sh`, funding charged, 1× slippage. Only the
turnover rule differs.

| | old cap | new cap |
|---|---|---|
| mean Sharpe | 1.96 | 2.21 |
| compounded | +1043% | +1450% |
| folds positive | 9/9 | 9/9 |
| mean max drawdown | -12.1% | -11.5% |
| turnover / rebalance | 0.488 | 0.802 |
| modelled cost drag | 522 bps | 859 bps |

Per-fold Sharpe delta: `+1.71 +1.51 +0.25 +0.02 +0.15 -0.94 -0.46 -0.05 +0.04`.
Six of nine improve.

**The mean delta is +0.25 with a standard deviation across folds of 0.85, so
±0.57 at two standard errors. It straddles zero.** The compounded figure looks
decisive only because compounding nine folds amplifies a small per-fold edge.
What the evidence supports is "not harmful, directionally positive, and it
removes a demonstrated failure" — not "+0.25 Sharpe".

## A correction

The commit message for `bc45c8a` says fold 6 got worse because "the suppressed
reductions were accidentally protective" over 2023-12 to 2024-08. **That causal
claim was unsupported and should not be relied on.** It was inferred from a
single fold and written into the record as though it were a finding.

Checked afterwards, against BTC's return over each fold window:

| fold | BTC | Sharpe delta |
|---|---|---|
| 1 | +354% | +1.71 |
| 2 | -8% | +1.51 |
| 3 | -53% | +0.25 |
| 4 | +39% | +0.02 |
| 5 | +54% | +0.15 |
| 6 | +47% | **-0.94** |
| 7 | +37% | -0.46 |
| 8 | +8% | -0.05 |
| 9 | -29% | +0.04 |

Folds 4 and 5 saw comparable rallies and improved; the largest gain of all is
fold 1 at +354%. The +0.51 correlation is one outlier's doing. Fold 6 is 1.4
standard deviations from the mean of a nine-point sample, and fold 1 is 1.7 in
the other direction — nobody would call that one a regime either.

So there is no regime effect here to find, and nothing to design around. Had
there been one, the answer would still not have been to keep a cap that holds
positions through a regime it knows nothing about: that is an invisible,
untestable trend filter wearing a cost control's clothes. A real regime effect
belongs in the model, as something it can see and act on deliberately.

## The bootstrap, which is the number to quote

`crypto-portfolio compare` pairs the two runs by date and block-bootstraps the
Sharpe difference over 2,133 steps, resampling contiguous 20-day blocks so the
runs that make a drawdown a drawdown survive the resampling.

| change | observed | 90% interval | ahead in |
|---|---|---|---|
| funding charged | **-0.313** | **[-0.546, -0.099]** | 1% |
| turnover cap exemption | +0.248 | [-0.099, +0.610] | 88% |
| 2× slippage stress | +0.071 | [-0.120, +0.274] | 72% |

Only funding is conclusive, and it is the one that costs Sharpe. The cap
exemption is favoured in 88% of resamples and its interval still includes
zero — better resolved than the ±0.57 the nine folds gave, and still not proof.
Ship it on the argument that a cost control must not refuse to let you shed
risk, and treat +0.25 as a direction rather than a magnitude.

The slippage row is the clearest verdict of the three: the stress cannot be
distinguished from no stress at all, which is what happens when "2×" scales
only the 0.5bp spread term and leaves the 4.5bp commission alone.
