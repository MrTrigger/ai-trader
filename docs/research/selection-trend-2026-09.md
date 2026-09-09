# Selection and higher-timeframe momentum, September 2026

The subsequent [Lighter baseline correction](lighter-historical-baseline-2026-09.md)
rebuilds dated venue availability before ranking and uses native execution data.
The static-membership figures below remain sensitivity results, not the deployed
Lighter strategy's historical baseline.

This round found no verified improvement. Retain the current selection and equal
long and short exposure budgets. The best screening candidate, a 30/90-day
momentum adjustment to coin conviction, reduced full-period Sharpe from 1.541 to
1.446. The small trained market model failed to improve its screening control.

The venue sensitivity exposed a larger limitation: restricting historical
selection to assets listed on Lighter today reduced the control Sharpe to 0.516.
The broad-universe 1.541 result does not establish the Lighter paper bot's edge.
This is a current-membership sensitivity with historical price and funding
proxies, not a historical Lighter performance estimate. No policy was promoted.

## What the current split means

The score-selected ranker targets equal long and short exposure. At the configured
0.80 gross exposure, each side targets 0.40 of NAV. It selects the strongest 24
per-risk scores globally, so equal exposure does not mean equal numbers of names.
Within each side, conviction divided by volatility determines weights. A binding
position cap scales the whole book down. Turnover limits, partial execution, and
price moves can leave the held book away from the target.

The unchanged baseline reproduced every financial step and metric from the prior
R4 replay. Its pooled daily Sharpe is 1.540502 across 957 days, December 17, 2023
through July 30, 2026. Each of four walk-forward periods starts with $100,000.
All comparisons retain zero commission, 2.2 basis points half-spread, impact,
participation, funding proxies, historical eligibility, and the asset ranker.

The latest baseline period earned $53,502 from short price moves and lost $16,493
on longs before costs. Funding cost another $17,760. These are reconciled Rust
attribution totals, not evidence that a permanent short bias would improve Sharpe.
The long sleeve also offsets market exposure from shorts.

## Declared screen

[The declaration](selection-trend-2026-09.json) fixed six alternatives before
measuring them. Screening uses original folds 6–8, 711 days ending November 26,
2025. A candidate must earn a positive return in each period. Highest pooled daily
Sharpe wins, with the ID breaking ties. The fourth period is used only for the
selected candidate's confirmation in this round, but all dates have been used in
earlier research. This is development, not untouched validation.

| Policy | Mechanism | Screen Sharpe |
|---|---|---:|
| Current control | Global top 24, equal exposure per side | 1.8706 |
| Select per side | Strongest 12 longs and 12 shorts | 1.6989 |
| Select 16 | Global top 16 | 1.7627 |
| HTF conviction | Adjust each coin's conviction with 30/90-day momentum | 1.9477 |
| Breadth allocation | Small net tilt from market breadth | 1.8944 |
| Confidence allocation | Small net tilt from relative sleeve scores | 1.8946 |
| Trained allocation | Small net tilt from a purged seven-day market model | 1.8609 |

The winning adjustment multiplies conviction by
`1 + 0.25 * direction * mean(sign(return_30d), sign(return_90d))`.
Direction is +1 for longs and -1 for shorts. Both trends agreeing with the trade
increase conviction by 25%. Both opposing it reduce conviction by 25%. Opposite
trend signs cancel. Missing either horizon leaves conviction unchanged.
This affects the cost threshold, selected names, and weights together. It retains
equal long and short exposure budgets and does not retrain the asset ranker.

Allocation candidates retain the selected names. Their target net exposure is
bounded to 0.10 NAV, with long and short budgets `(gross + net)/2` and
`(gross - net)/2`. Breadth averages the existing 30/90-day breadth scores.
Confidence uses the difference between long and short per-risk score sums,
divided by their total. No candidate gets cheaper execution than the control.

## The trained trend model

The fixed `selection-timer-7d-v1` profile predicts the volatility-scaled seven-day
market return. It uses seven Rust-computed inputs: market returns over 7, 30, and
90 days, 30-day market volatility, 30-day and 90-day breadth, and mean seven-day
funding. It excludes external series with uncertain publication vintages.

LightGBM fits at most 50 two-leaf trees with at least 180 training rows per leaf.
There is one fixed profile, with no parameter search. Each expanding fit purges
the seven-day label horizon plus two days. Its last training label ends four days
before that period's first decision. Runtime inference uses the completed D-1
feature date and checks the model cutoff, matrix hash, feature order, and stale
inputs. Future matrix labels never enter inference.

| Original fold | Training rows | Last training label ends |
|---|---:|---|
| 6 | 2,193 | 2023-12-13 |
| 7 | 2,430 | 2024-08-06 |
| 8 | 2,667 | 2025-03-31 |
| 9 | 2,904 | 2025-11-23 |

The bounded response ranges from -0.0702 to +0.0912 over all evaluation dates.
At the declared 0.10 net band, the largest implied net adjustment is under 0.92%
of NAV. The weak signal was not enlarged after observing that scale. Its screen
Sharpe of 1.8609 fails to improve on the 1.8706 control. This rejects this profile
and sizing rule, not every possible trend model.

## Confirmation rejects the screening winner

The full comparison adds original fold 9, November 27, 2025 through July 30, 2026.
It compares only the declared winner with the control. It does not search the
other candidates after seeing the fourth-period outcome.

| Full-period measure | Current control | HTF conviction |
|---|---:|---:|
| Pooled daily Sharpe | 1.5405 | 1.4459 |
| Compounded stitched return | 147.72% | 133.83% |
| Maximum stitched drawdown | 19.10% | 21.07% |
| Latest-period return | 13.92% | 4.43% |
| Latest-period Sharpe | 0.8189 | 0.3723 |
| One-hour delayed execution Sharpe | 1.3234 | 1.2147 |
| Severe execution stress Sharpe | -0.8860 | -0.9632 |

Stitched returns compound each saved daily return across separately reset books.
They are not the return of a single continuously funded account. Severe stress
uses 22 basis points half-spread and four times the impact coefficient for both
policies. Funding and participation assumptions remain matched.

The latest-period HTF long price loss improved to $14,102 and funding cost fell
to $15,566, but short price P&L fell to $39,379 from $53,502. The adjustment gave
up more on shorts than it recovered elsewhere. Latest-period turnover per
rebalance increased from 0.7021 to 0.7162. Neither central policy had a rejected
decision. These are existing Rust replay and attribution outputs.

Monte Carlo uses 10,000 within-fold circular block-bootstrap samples, seed 42.
Standalone runs use 5-, 20-, and 60-day blocks. The paired 20-day estimate of
HTF-minus-control Sharpe is -0.0946, with a 95% interval of [-0.2788, +0.1285].
The standalone HTF Sharpe interval is [0.5758, 2.4842], versus [0.6857, 2.5214]
for the control. There is no supported improvement. These intervals are
conditional on the selected policies and do not correct for this screen or
earlier research trials.

The confirmation command exits with status 1 because the development gate
failed. Replays, input verification, the experiment register, and Monte Carlo
completed successfully. The verdict requires Sharpe above 1.5, above the matched
control, and a positive latest-period return. The candidate fails the first two.

## Lighter membership sensitivity

The helper intersects every historical universe snapshot with the 216 active
native perpetual symbols in the September 8 Lighter response. It retains each
remaining member's historical eligibility and rank. It changes no source data,
uses no aliases or price rescaling, and preserves historical price and funding
proxies. Both arms use the same filtered snapshots and execution assumptions.

| Full-period measure | Current control | HTF conviction |
|---|---:|---:|
| Pooled daily Sharpe | 0.5160 | 0.4606 |
| Compounded stitched return | 23.58% | 20.27% |
| Maximum stitched drawdown | 15.91% | 18.16% |
| Latest-period return | -4.89% | -8.01% |

The paired 20-day 95% interval for HTF-minus-control Sharpe is
[-0.2474, +0.1154]. This also gives no reason to adopt the HTF policy.

Today's membership cannot establish historical listing eligibility. The test
also does not reproduce Lighter's historical liquidity, funding, or the current
paper bot's full execution path. Its useful conclusion is narrower: the measured
edge depends heavily on which assets are available, and zero fees alone do not
make the broad-universe result portable to Lighter.

The next research priority is a Lighter-specific baseline and a fixed ranker fit
evaluated on that eligible universe, followed by complete data collected after
the model freeze. Keep the paper run collecting observations. A larger direction
model is a lower priority than verifying that the tradable selection retains an
edge. This round stops at its declared rejection instead of optimizing runners-up
on the same fourth-period returns.

## Validation and evidence boundaries

All 110 crypto-portfolio Rust tests and three training tests pass. The tests cover
selection balance, gross and net exposure, missing momentum,
configuration constraints, stale inputs, model cutoff dates, and matrix mutation.
Changing post-cutoff training features and labels leaves fitted trees unchanged.
The actual timing and breadth replays produce different first-day fills.
An independent code, artifact, and result review found no remaining blocking
correctness issue after the timing integration was completed.

The historical source configurations do not enforce `max_net_exposure`, whereas
the deployed Lighter configuration has a 0.10 limit. The optional allocation rules
bound target net exposure, but held exposure can drift after fills. These research
numbers are not a replay of every deployed safeguard.

Local daily prices extend into August 8 and hourly data into August 6, but funding
ends August 4 for most current names. The deployed ranker artifact was created
August 9. There is no complete local period after that artifact's creation to use
as a subsequent forward test. A few early-August steps cannot validate Sharpe.

## Reproduction

The research commands do not read exchange credentials or submit orders.
Training writes to a new output directory and refuses to replace existing fits.
For a fresh run, create the timing artifacts at the declaration's expected path:

```bash
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio
bin/train-selection-timer.sh var/research/selection-timer
SHARPE_DECLARATION=docs/research/selection-trend-2026-09.json bin/research-sharpe.sh screen
SHARPE_DECLARATION=docs/research/selection-trend-2026-09.json bin/research-sharpe.sh confirm
bin/prepare-lighter-replay.sh
bin/research-selection-venue.sh \
  var/research/selection-trend-confirm-htf-conviction-suite.json \
  var/research/selection-trend-lighter-data var/research/selection-trend-lighter
```

The existing run's timing artifacts are already present. Their hashes are in
`var/research/selection-timer/artifacts.sha256`, with archived training evidence
under `training-evidence/`. Use new declaration output paths for a new experiment.

Primary result artifacts are the screen, confirmation, and venue `register.json`
files under `var/research/selection-trend-*`, with fold reports, input hashes, and
Monte Carlo outputs alongside them. The decision trail is
`.audit/selection-trend.tsv`.
