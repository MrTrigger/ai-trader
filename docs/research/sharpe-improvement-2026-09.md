# Improving recent net Sharpe, 2026-09-08

A combined candidate reaches recent pooled Sharpe **1.541** with zero
commission and a 2.2 bp half-spread, narrowly clearing the numerical target.
It selects 24 names by per-risk model score. The original policy under the
same costs scores 1.459; the original-fee baseline scores 1.132.

This is a development cost counterfactual on existing historical data. It
has not established performance on Lighter or on unseen dates. The 20-day
Monte Carlo Sharpe interval is 0.686 to 2.521, and the selection improvement
over the same-cost control has an interval spanning zero.

## Evaluation contract

The target is pooled daily Sharpe above 1.5 over original folds 6–9, December 17,
2023 through July 30, 2026, with impact costs and $100,000 initial cash per fold.
The last fold must have a positive return. Gross targets, data, dates and
risk limits stay fixed. Rounds 1–3 use the same frozen models; the horizon
experiment below retrains a matched pair. Execution stress and one-hour delay are reported
separately. All dates were already exposed during prior research.

Each round declares its candidates before replay. Selection uses only folds
6–8, 711 days: require positive return in every fold, then choose the highest
pooled Sharpe, using the ID to break a tie. Confirmation includes fold 9 and
replays all four recent folds, 957 days, under impact, stress and delay.
This ordering limits additional selection, but cannot turn reused dates into
an independent holdout.

The runner revalidates the complete screen register before selection, rejects
an arbitrary candidate and hashes the selected ID, declaration and screen
register into confirmation sources. It writes an explicit target verdict and
returns a failure if the confirmation misses the target.

## Round 1: funding, turnover and fees

The [declaration](sharpe-target-r1.json) contains five noncontrol candidates.
All use the same frozen model and impact assumptions. Only the named fee
counterfactual changes commission.

| Three-fold screen | Pooled Sharpe | Result |
| --- | ---: | --- |
| Control, 4.5 bp commission | 1.389 | Baseline |
| Subtract adverse estimated next-day funding | 1.401 | Small improvement |
| Skip same-side resizes below 20% of position | 1.342 | Worse |
| Double the cost-relative resize threshold | 1.369 | Worse |
| Funding adjustment plus 20% resize band | 1.366 | Worse |
| Zero commission | 1.950 | Selected |

The funding penalty is the adverse side of trailing `funding_7d / 7`.
It never rewards favorable carry or changes a forecast's direction. This fixed
one-day horizon matches the existing model recipe; the legacy artifacts do not record
label horizon, and historical missing funding remains zero-imputed. Newly
trained artifacts record the horizon; the funding flag rejects explicit
non-24-hour horizons. The flag
`funding_cost_adjustment` is off by default. This round did not establish an
economic reason to adopt it. The frozen recipe ranks its per-risk labels,
so its volatility-scaled score is not a calibrated expected return. This
funding subtraction is a heuristic score penalty, not a calibrated forecast
of net profit.

The resize controls saved little commission and lost more in price performance.
For example, the 20% band reduced commission by only about $156–203 per screen
fold. Broader prior studies had already rejected trend tilts, unbalanced books
and tighter position caps; they were not presented as new hypotheses here.

## Zero-commission confirmation

Commission changes in both the planner's cost estimates and simulated fills.
Funding, spread, impact and participation constraints remain. Holdings can
change, so this is a policy replay rather than adding old commissions back to
an unchanged NAV series.

| Four-fold comparison | Current fees | Zero commission |
| --- | ---: | ---: |
| Pooled Sharpe, impact scenario | 1.132 | **1.647** |
| Last-fold return | +8.09% | +16.95% |
| Compounded return across reset folds | +90.52% | +164.14% |
| Maximum daily drawdown | 18.27% | 18.35% |
| Pooled Sharpe, stressed spread and impact | 0.419 | 0.930 |
| Pooled Sharpe, one-hour execution delay | 0.909 | 1.398 |

The target passes for the central impact assumptions. It does not pass under
the stress or one-hour delay scenarios. The one-hour test is a sensitivity,
not a model of Lighter's subsecond account latency. The drawdown barely changes,
so eliminating fees does not eliminate the strategy's market risk.

With 10,000 Monte Carlo paths, seed 42 and 20-day blocks, the zero-fee historical
Sharpe percentile interval is 0.77 to 2.64. The paired improvement interval is
0.40 to 0.65 Sharpe. Simulated one-year loss frequency falls from 8.03% to 1.53%,
and the 95th percentile drawdown falls from 23.66% to 20.64%.

For zero commission, 5-, 20- and 60-day blocks give one-year loss frequencies
of 4.41%, 1.53% and 0.41%, respectively. The corresponding 95th percentile
drawdowns are 25.81%, 20.64% and 18.35%. These are conditional scenarios, not
forecasts. The interval for Sharpe does not establish that underlying future
Sharpe exceeds 1.5, and no multiple-testing adjustment was applied.

## What Lighter changes

Lighter's Standard account lists zero fees for ordinary market and limit
orders, with 300 ms taker latency. Native TWAP, RFQ and Chase Limit orders carry
a 1 bp fee. [Account types](https://apidocs.lighter.xyz/docs/account-types),
[official API updates](https://t.me/s/lighter_api_updates).

The dated public-market check found exact active-perpetual matches for 19 of
25 holdings in the original policy's July 30, 2026 impact-replay portfolio. PEPE may map to `1000PEPE`
after unit conversion. ALLO, DEXE, OPN, SYN and XAUT have no established
matching perpetual in this check. Today's market list is not historical
eligibility evidence. See the [venue feasibility report](lighter-feasibility.md).

Public depth and funding endpoints worked. The global trade archive starts in
January 2025 and requires authenticated access plus a 100 LIT payment. No
purchase or account action occurred. It cannot provide venue history for the
earlier recent folds. [Historical data](https://apidocs.lighter.xyz/docs/historical-data).

A viable route needs explicit instrument mapping, a declared policy for
unsupported names, venue-specific funding and depth measurements, and a paced
Standard-account executor using the intended order types. Filtering the book
or splitting it across venues changes the strategy and needs its own replay.

## Round 2: normalize the selection score

The [second declaration](sharpe-target-r2.json) changes only which 24 names the
balanced constructor selects. With per-risk model labels, the scorer converts
predictions to expected returns by multiplying by volatility. The original
selector ranks those returns. The new `risk_adjusted_score_selected` constructor
ranks conviction divided by volatility, then uses the existing balanced sizing.
Both retain the expected-return cost floor and require two names on each side
after selection; if that requirement fails, the target is flat.

Screen Sharpe improved from 1.389 to 1.531. Across all four recent folds it rose
from 1.132 to only **1.226**, missing the target. The last fold returned +8.30%
versus +8.09%. Stress Sharpe was 0.534 and delayed Sharpe was 1.041.

The first screen fold accounts for much of the benefit: long price P&L rises
from $39 to $1,187 and short price P&L from $11,950 to $15,943, while commission
barely changes. This is a constituent change, not a fee saving. It also changes
subsequent partial-fill state. This adaptive second round adds selection pressure
on already exposed data and was not promoted to the default configuration.

## Round 3: broaden the book

The [third declaration](sharpe-target-r3.json) adds two hypotheses: select up to
40 names by expected return or by per-risk score. The 24-name original and
score-selected policies are repeated controls. Both wider variants return
screen Sharpe 1.481 and produce identical paths on this sample. The previous
24-name score-selected policy wins again at 1.531. Its completed four-fold
confirmation remains 1.226; repeating that replay would add no evidence.
Rounds 1–3 contain eight distinct noncontrol policy hypotheses.

## Lighter quote sensitivity

On September 8, the public depth probe measured a median half-spread of
2.165 bp across 19 exact matching perpetuals. Replaying with zero commission,
a constant 2.2 bp half-spread and the original impact/funding assumptions gives
pooled Sharpe **1.459**, last-fold return +13.98% and drawdown 18.35%.
The 20-day-block Monte Carlo Sharpe interval is 0.579 to 2.435. The paired
Sharpe improvement over the original-cost control is 0.327, with an interval
of 0.251 to 0.412. One-year simulated loss frequency is 2.79%.

This is a sensitivity anchored to one current quote snapshot. It uses the
original historical universe, including instruments without an established
Lighter mapping. The quote median is neither a historical nor an order-weighted
cost estimate. See the [depth evidence and rerun instructions](lighter-feasibility.md).

## Forecast horizon experiment

The [declared comparison](sharpe-target-horizon.json) fits fresh 24-hour and
72-hour models with the same 67 features, LightGBM settings, training-date
cutoffs and original portfolio construction. Both use a four-day purge before
each evaluation fold. A 72-hour label entered one hour after the final
training decision ends 23 hours before evaluation begins. The daily decision
cadence stays fixed. Longer-horizon return magnitudes still pass through the
same absolute cost threshold, so the comparison includes that policy effect.

Before fitting, we found and corrected future-label availability affecting
training feature ranks. The old builder removed assets lacking future entry
or exit prices before normalizing today's features. The new builder normalizes
the decision-time cohort first and then attaches available labels. A regression
test removes one future exit and verifies unchanged feature vectors for the
remaining assets. Both new models use this correction. Comparisons with the
legacy frozen models also change training history and label transformation.
The [frozen recipe](absolute-label-unbalanced.md) uses ranked per-risk labels;
both fresh models use raw demeaned return-per-risk targets. Frozen fold 6
records 46,258 rows over 1,641 dates; fresh fold 6 uses 41,301 rows from the
available matrix coverage starting February 10, 2020. Only the fresh pair
isolates the horizon comparison.

Rust now emits final per-risk targets and label horizon metadata. Python only
fits the supplied targets. Legacy per-risk matrices must be rebuilt; Python
label-ranking switches now fail rather than silently transform the target.
The runtime refuses to use a new model until its training label window has
ended. Legacy artifacts retain their previous date check.

The actual matrices contain 65,514 shared rows with identical feature vectors.
Screen Sharpe is 1.223 for 24 hours and 1.395 for 72 hours. The latter narrowly
beats the declared frozen-control threshold of 1.389 and has positive returns
in all screen folds, so it advances under the declared rule.

Four-fold confirmation rejects it: 72-hour Sharpe is **0.960**, with a last-fold
return of **-3.67%**. The matched 24-hour control scores 1.195 and returns +15.00%
in the last fold. Three-day stress Sharpe is 0.334 and delayed Sharpe is 0.505.
No model change was promoted. The training corrections remain because their
causality and target-ownership tests pass independently of trading performance.

The first runner initialization failed before fitting because the environment
has no pip module. A later screen failed closed when the new model embargo
received the completed feature date instead of the decision date. The corrected
runner reran both arms under a fresh output prefix, preserving the initial
partial evidence. It now compares Python dependency versions on every resume.

## Round 4: combine selection and lower costs

The [fourth declaration](sharpe-target-r4.json) tests the existing score-selected
policy with zero commission and a 2.2 bp half-spread. Its matched control uses
the original selection with those same costs. This adds one combined policy
hypothesis after observing the separate selection and fee results.

| Four-fold result | Original selection, same costs | Score-selected candidate |
| --- | ---: | ---: |
| Pooled Sharpe | 1.459 | **1.541** |
| Latest-fold return | +13.98% | +13.92% |
| Maximum daily drawdown | 18.35% | 19.10% |
| Pooled Sharpe, one-hour delay | 1.208 | 1.323 |
| Pooled Sharpe, severe cost stress | -1.062 | -0.886 |

Here severe stress means 10 times the 2.2 bp spread, or 22 bp, and four times
the impact coefficient. Earlier rounds with a 0.5 bp spread stress it to 5 bp;
the stress rows therefore represent different absolute spread assumptions.
The delay test remains a one-hour sensitivity, not Lighter's subsecond latency.

With 10,000 paths, seed 42 and 20-day blocks, the candidate's Sharpe interval
is 0.686 to 2.521, simulated one-year loss frequency is 1.78%, and 95th percentile
drawdown is 21.24%. Its paired Sharpe improvement over the same-cost control is
0.082, with an interval of **-0.049 to +0.234**. The data do not establish a
reliable selection improvement. Against the original-fee baseline, the combined
improvement is 0.409, with an interval of +0.236 to +0.611. Fee savings explain
most of the observed gain. No correction for repeated strategy selection was
applied, and the data were already exposed.

The numerical target passes under the central cost assumptions. Venue-specific
validation remains necessary: supported instruments, historical eligibility,
funding, spread and actual execution behavior. The corrected candidate audit finds 19 of 25 holdings matched to current
active Lighter perpetuals, covering **64.55% of gross notional**. Missing names
are ALLO, DEXE, OPN, PEPE, SYN and XAUT. The initial fill-only reconstruction
incorrectly retained TON after its July 1 settlement; the corrected audit
applies the recorded closure and reconciles the candidate's gross notional
exactly. The old depth sample covers 17 of the candidate's 19 matched symbols;
BNB and TRX were not sampled. It does not calibrate this candidate's costs.
The next useful work is to collect candidate-specific Lighter execution evidence
and evaluate a fixed policy for unsupported instruments before paper deployment.


## Artifacts and reruns

```sh
bin/research-sharpe.sh screen
bin/research-sharpe.sh confirm
```

The horizon runner requires a current binary:

```sh
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio
bin/research-horizon.sh screen
bin/research-horizon.sh confirm
bin/check-horizon-feature-parity.sh
```

Its default declaration is `sharpe-target-horizon-final.json`; the initial
horizon declaration and partial outputs remain as failed-run evidence.
`HORIZON_DECLARATION` can select a copied declaration with a fresh output prefix.
Confirmation returns failure after writing its verdict if the target is missed.
Use `bin/replay-lighter-quoted-cost.sh --reports-only` to verify the saved quote
sensitivity. An additional candidate-versus-original-fees Monte Carlo comparison
is saved as `monte-carlo/paired-original-fees.json` in the round-4 confirmation.

The policy runner's default declaration is round 1. Set `SHARPE_DECLARATION` to another declared
round to run it. Generated configs, suite manifests, input hashes, daily
attribution, experiment registers and Monte Carlo reports are under
`var/research/sharpe-target-r1-*`. Public Lighter responses and the Rust coverage
audit are under `var/research/sharpe-target-probes/lighter/`.

Changing the executable, data or configurations requires a fresh output prefix
in a copied declaration. Existing outputs reject mixed inputs. Round 1 was
completed before the round-2 constructor changed the executable, so reproducing
round 1 with the final code requires such a fresh prefix. Retain the copied
declaration for both screen and confirmation.

The decision trail is `.audit/sharpe-target.tsv`. The original-fee control's
NAVs, fills and attribution match the previous impact replay exactly over the
three screen folds. No live orders, transfers or default portfolio changes
were made.

Verification passed: 104 Rust portfolio tests, six Python trainer tests, the
workspace check, shell syntax checks, actual-matrix feature parity and the
changed-environment rejection test. All four repeated quote-cost controls match
the earlier NAV, cash, fills, attribution and exposure paths. Independent review
by gpt-5.6-sol found no remaining discrepancy between this report and the saved
evidence.

The [buy-and-hold comparison](buy-and-hold-2026-09.md) adds BTC, ETH and a fixed
50/50 spot portfolio over the same dates and daily valuation grid.
