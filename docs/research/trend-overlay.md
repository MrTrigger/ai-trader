# Trend overlay — pre-registered (2026-08-19, before any number exists)

## What this is

The ranker is a cross-sectional model with no timing skill (the label cannot
express direction; three labels that could were tested and failed —
`docs/research/absolute-label-unbalanced.md`). If the book is to lean with
the market, the lean must come from a component whose target *is* the
market, sitting in the construction layer: the ranker keeps choosing names
long and short; an overlay sets how much **net** the book runs. This note
fixes that overlay — inputs, rule, band, evaluation, decision — before it is
built or run. One rule, no scan, no learned model (≈2,000 daily rows is
where a learned timer overfits; the literature's surviving evidence at this
horizon is trend/vol rules — research round §4).

## The rule

All inputs are the planner's existing point-in-time daily features for the
eligible universe (newest feature date = decision date − 1; nothing new is
fetched, nothing is forward-looking).

- **Breadth trend.** For h ∈ {30, 90} days: `b_h = 2 · share(x_ret_h > 0) − 1`
  over eligible names (−1 = every name down over the window, +1 = every name
  up). `trend = (b_30 + b_90) / 2 ∈ [−1, 1]`. Two horizons, fixed a priori
  (7 is noise, 180 is a different strategy); equal weight; no smoothing.
- **Net target.** `N = band · trend`, **band = 0.30** of NAV (a risk-appetite
  number: at full tilt the book is 0.55 long / 0.25 short on 0.80 gross, or
  the mirror). No vol scaling (one part fewer to defend; vol enters only via
  the ranker's own edge/vol sizing).
- **Budget → sleeves.** `G = 0.80` unchanged. `long_sleeve = (G + N)/2`,
  `short_sleeve = (G − N)/2`. The ranker's selection is unchanged (top 24 by
  |E[r]| across both sides, cost floor, two-sided requirement); within each
  side, names are sized by edge/vol to the side's sleeve — i.e. `risk_adjusted`
  with asymmetric sleeves. Constructor name `risk_adjusted_tilted`. If the
  list has < 2 names on a side the two-sided rule still refuses (unchanged).
- **Guard.** For this test `max_net_exposure` is unset. If the overlay is
  ever adopted, the guard's quantity becomes `|net − N|` (deviation from the
  requested tilt), band 0.10, same release rule.

## What is run

`bin/replay-folds.sh` on the frozen fold models (`var/research/wf-rank-2020`,
the ranker is NOT retrained), constructor `risk_adjusted_tilted`, everything
else the frozen recipe; nine folds, funding charged, 1× slippage; baseline =
the same models under `risk_adjusted` (`wf-rank-2020-replay-base`, with the
settlement fix applied to both sides so they are binary-identical).

Reported: per-fold Sharpe / return / maxDD / turnover; realized net
p10/p50/p90 per fold; the overlay's own timing statistic —
`corr(N_t, EW-market return_{t+1})` and the share of days N < 0 inside
30-day drawdowns > 20% — so that if it helps we know *why*, and if it hurts
we know whether the rule timed nothing or timed badly; bootstrap 90%
interval (`crypto-portfolio compare`), 20-day blocks.

## Decision rule

Adopt the overlay only if it beats the flat book with a bootstrap 90%
interval excluding zero **and** the 2022 folds' max drawdown is not worse.
Indistinguishable → keep flat (simpler, has the paper track record). Worse →
closed; no band, horizon, weighting or smoothing is revisited against these
folds. One rule, one run, then the number is the number.

## Expectation, written now

Breadth trend at 30/90 days is a real but slow signal: it will be net short
through most of 2022 and net long through most of 2021 and 2024 — and late
at every turn (long into May 2021 and Nov 2021, short into the 2023 recovery).
Vol-targeted trend on liquid crypto is ~0.6–1.2 Sharpe stand-alone in the
better sources; bolted onto a Sharpe-2 flat book at 0.3 band it most likely
adds drawdown in the turns and a little return in the trends, netting near
zero. If it wins it wins on 2022.

## Results

*(none at the time of writing)*
