# Lighter strategy improvement

Research is ongoing. The user rejected the earlier stopping point: a good
strategy must improve returns materially while retaining Sharpe above 1.5
and beating BTC. R8 through R42 test signal, execution, training and risk changes.
R22 failed its locked later confirmation: it lost 10.72% while BTC gained
23.83%. No candidate is accepted. Research continues with native training labels and fixed model ensembles.
The native five-seed absolute-return model earns 18.71% with Sharpe 1.5070
over all 285 development days. It clears the point threshold narrowly and
remains a provisional candidate, with weak uncertainty and rally-month evidence.
Routing the same predictions to BTC and ETH only (R38) raises this to 26.51%
with Sharpe 1.5516, and adding the existing 20% resize deadband (R39) to
28.53% with Sharpe 1.6478. Both take more risk, trade about three times the
turnover per rebalance, and lose most of the return under stressed costs.
Neither is accepted; the bootstrap Sharpe intervals still include zero.
Raising the per-position cap to 40% so the two-asset book can use its 80%
gross target (R40) gives 43.17% at Sharpe 1.8638, passes the joint condition
in both halves, and is the first bootstrap interval excluding zero. It still
fails under stressed execution costs and rests on 285 reused days, so it is
the strongest development result rather than an accepted strategy.
A three-year walk-forward of the same book on Binance proxies (R42, August
2023 to August 2026, retrained at six fold boundaries) loses 2.94% against
BTC's 122.31% and is negative in every fold before February 2025. The
285-day results are period specific and no candidate shows a multi-year edge.
The line then moved to the full-altcoin relative long/short ranker on a
reusable three-year walk-forward harness (R43 to R50). Widening the
point-in-time cohort to the top 120 names with a 40-name book and 1.5x
selection hysteresis gives Sharpe 1.85 and 236% over August 2023 to August
2026, and the same book, run as a holdout on September 2020 to August 2023,
gives Sharpe 2.21 and 288% with a 13.5% drawdown. Chained over six years it
earns 1,202% against BTC's 491% with a 23.6% drawdown. It still loses under
ten-times-spread stress, and it has not yet been replayed on native Lighter
data with a Lighter-listed cohort.
The current continuation plan is `.audit/lighter-optimization-plan.md` and
its decision trail is `.audit/lighter-strategy-continuation.tsv`.

The Lighter-cohort ranker reaches daily net Sharpe **2.5278**, return **9.89%**,
and maximum drawdown **4.12%** over November 27, 2025 through July 30, 2026.
The old model returns **-7.48%** with Sharpe **-1.0111** under identical native
hourly execution assumptions. Both start with $100,000.

This clears the measured development target of Sharpe above 1.5. The later
half was effectively flat, execution stress erased the profit, and the dates
were reused for selection. It does not establish future Sharpe above 1.5.
No paper policy or deployment changed.

The user subsequently tightened acceptance to **Sharpe > 1.5 and a return
above BTC buy-and-hold**, with a preference for a much larger return gap.
The evaluator now enforces that conjunction on matched native timestamps
and reports it for the full period and both chronological halves. Candidates
that meet the full-period minimum are compared by return above BTC.
There is no agreed numerical margin for "much better" or requirement that
every segment pass. Both scopes remain visible instead of silently choosing
the easier one.

| Period | Strategy Sharpe | Strategy return | BTC return | Both conditions |
|---|---:|---:|---:|---|
| Full | 2.5278 | 9.8899% | -29.5890% | Pass |
| First half | 4.1683 | 9.8853% | -26.5231% | Pass |
| Second half | 0.0230 | 0.0042% | -4.1726% | Fail |

The 39.48 percentage point full-period lead does not show that this model
competes well during BTC rallies. It trails BTC in every positive-BTC monthly
slice, including the partial November and July slices. April is the clearest
example: strategy +0.27%, BTC +13.44%. Monthly results preserve the actual
portfolio path and preceding NAV. They are descriptive, reused observations,
not independent validation or additional mandatory acceptance gates.

The rerunnable evaluator writes this evidence to
`var/research/lighter-optimization-r6-hourly/joint-evaluation/summary.json`.
The original R6 evaluation and verdict remain preserved. The R7 declaration
tests the existing 0.25 HTF conviction adjustment and 0.10 NAV breadth
allocation separately with the frozen venue-cohort model and native hourly
execution. It keeps the same capital, gross exposure targets and costs.

## R7 return improvement tests

| Policy | Central Sharpe | Return | Stress return |
|---|---:|---:|---:|
| R6 control rerun | 2.5278 | 9.8899% | -0.2519% |
| HTF conviction weight 0.25 | 2.0888 | 8.8508% | -1.5547% |
| Breadth net allocation band 0.10 | 2.3607 | 9.9365% | -0.6855% |

All three meet the full-period numerical minimum. Breadth has the highest
return, but adds only 0.0466 percentage points over the control and reduces
Sharpe while worsening stressed return. This is insufficient evidence of a
useful improvement. HTF selection reduces return. The R7 control reproduces
all 246 R6 financial steps and metrics exactly. No policy was promoted.

Breadth improves the later-half return to 1.0218% with Sharpe 0.7206.
HTF conviction returns 0.2311% with Sharpe 0.2004 in that half. Neither
passes the joint gate there, and both still trail BTC in every rising-BTC
monthly slice. The simple adjustments do not resolve the rally weakness.

The six runs and their source hashes are in
`var/research/lighter-optimization-r7-btc/register.json`. Matched benchmark,
half-period and monthly results for both alternatives are under that
directory's `evaluation/` tree. The evaluator's six tests and all 137 Rust
package tests pass. The unchanged twelve full/half benchmark summaries also
match the original evaluation exactly.

## R8 through R10 structural tests

R8 separates daily valuation and funding from the trading schedule. Every
three-day and weekly run still contains 246 daily NAV observations. The
daily control reproduces R6 exactly, and an independent audit checks all
1,637 fills and all daily valuations in the three-day central run.

| R8 policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| Three-day rebalance | 1.8370 | 8.2826% | 4.0384% |
| Seven-day rebalance | 1.4293 | 4.5116% | 3.3603% |
| Breadth net budget 0.40 | 1.3098 | 8.9831% | -0.4553% |

Three-day trading reduces cost sensitivity, but its later half loses 0.70%
with Sharpe -0.5517. The wider breadth budget changes directional risk and
does not clear the Sharpe minimum. None is a sufficient improvement.

R9 switches models on the first of each month from January through July,
keeping one continuous portfolio. Each training cutoff is three days before
activation, with the last 25-hour label ending 47 hours before activation.
The expanded matrix preserves all 6,486 original rows exactly. Seven refits
use the unchanged fixed training recipe. Schedule and model hashes are part
of replay provenance, and each decision records its selected model hash.

| R9 policy | Sharpe | Return | Stress return | Later-half return |
|---|---:|---:|---:|---:|
| Monthly refits, daily trading | 2.1011 | 10.4732% | 2.0114% | -3.9275% |
| Monthly refits, three-day trading | 0.6492 | 2.8205% | -0.6810% | -4.5902% |

The higher full-period return hides a worse later half. Retraining on this
schedule does not solve the observed weakness. The pre-January path matches
R6 exactly and all 246 model activations match the declared schedule.

R10 fixes one plan at midnight and retries only its remaining quantities
across four hourly VWAP windows. Each hour retains the 1% participation cap
against the lower of prior midnight ADV/24 and that hour's native volume.
Funding uses the actual held quantities before each hourly fill. Both the
one-hour control and four-hour candidate are marked at 04:00.

| R10 policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| Daily, one-hour execution, 04:00 mark | 2.5364 | 9.8206% | -0.3634% |
| Daily, four-hour execution, 04:00 mark | 1.0351 | 5.3056% | -8.5391% |
| Three-day, one-hour execution, 04:00 mark | 1.8852 | 8.2207% | 3.9740% |
| Three-day, four-hour execution, 04:00 mark | 1.6773 | 6.9597% | 2.0901% |

The daily four-hour book raises mean gross exposure from about 28.6% to
40.2%, but earns less. More of the frozen ranker's intended orders do not
improve its edge. The independent Rust auditor reconstructs cash, native
funding and NAV exactly across six reference/control/extended folds, including
164 no-fill days in the three-day policy. It rejects deliberately corrupted
cash, NAV and funding fixtures. Its code and results are under
`var/research/lighter-window-audit-r10/`.

Review found that a missing final valuation mark could suppress an earlier
fill. The loader now rejects missing/nonpositive final marks and the replay
refuses planned orders without final valuation, rather than recording them
as unfilled. The recorded R10 trials precede that validation hardening and
remain rejected candidates. Their actual executed cash and funding pass the
independent audit. No strategy was promoted or deployed.

## R11 through R14 directional tests

These trials trade BTC with the existing slow momentum, Donchian or purged
market-timing rule. Target gross remains 0.80. Position, net and cluster caps
rise to 0.80 to permit a directional BTC book. R11 inherited the ranker's
0.50 NAV turnover allowance, which deferred whole entries and reversals.
R12 raises only that allowance to 2.00 so the intended positions can trade.
The replay still applies native hourly volume caps and transaction costs.

| Later-period policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| R11 BTC momentum, inherited turnover limit | 1.4447 | 23.50% | 22.93% |
| R12 BTC momentum, executable reversals | 0.9717 | 15.64% | 13.95% |
| R12 BTC Donchian | 0.8562 | 9.53% | 6.82% |
| R12 BTC market timer | 1.2730 | 20.58% | 19.62% |

R11's stronger momentum result partly depends on accidental order deferrals.
It is not evidence for the intended momentum policy. No trial clears 1.5.
R12 momentum loses 4.09% in the later half; the timer gains 4.71% there but
has Sharpe only 0.77. Both use the same November 27–July 30 dates as R6.

R13 tests the three executable rules on April 4–November 26, 2025. The timer
uses the existing model trained through April 2; momentum and Donchian need
no fit. A declared BTC/ETH daily catalog keeps the original native pages and
records its parent catalog hash. BTC is the trade instrument; ETH supports
the benchmark comparison. Both have complete native hourly candles and
funding. These earlier dates appeared in other research and are not a fresh
holdout.

| Earlier-period policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| R13 BTC momentum | 1.4671 | 23.19% | 20.05% |
| R13 BTC Donchian | 0.1971 | 1.20% | -1.32% |
| R13 BTC market timer | -0.7019 | -12.76% | -17.74% |
| R14 momentum/timer agreement | 0.5792 | 5.01% | 1.07% |

R14 holds BTC only when the existing momentum and timer directions agree,
otherwise cash, retaining momentum volatility sizing. Its later-period
Sharpe is 1.1207, return 18.29%, stress return 16.87%. No new model was fit
and no horizon or threshold was tuned. The agreement rule fails too.

R15 declares a continuous 483-day BTC/ETH momentum test with BTC-only and
ETH-only controls. This measures diversification without resetting the book
at the earlier/later boundary. The combined first attempt failed before
replay because the feature cache requires one benchmark per suite; ETH's
control now has its own suite. The first split attempts also stopped before
replay because the generated universe directory lacked links to the signal
bars and funding. Those links now point to the unchanged source data.
All failed logs remain in `.audit/`; no measured trial was discarded.

The continuous R15 results do not qualify:

| Policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| BTC momentum | 1.1002 | 36.64% | 31.08% |
| ETH momentum | 0.8774 | 29.82% | 25.15% |
| BTC/ETH momentum | 1.0917 | 35.64% | 27.63% |

BTC buy-and-hold returns -23.30% over the same native 01:00 marks.
BTC momentum beats it in both chronological halves but misses Sharpe1.5
in each:1.3113 then0.8935. Adding ETH does not improve diversification here.
R16 tests existing conviction-preserving sizing because normalizing by total
conviction cancels its magnitude in a single-asset vol-parity book.

The zero-commission assumption applies to Standard accounts under Lighter's
[current fee schedule](https://docs.lighter.xyz/perpetual-futures/fees).
The replay retains hourly funding, as described in Lighter's
[funding documentation](https://docs.lighter.xyz/trading/funding), plus
spread, impact and observed-volume limits.

## R16 through R18 signal and portfolio construction tests

R16 retains conviction magnitude in position sizing with existing constructors.
It does not improve results over the full483days:

| Signal and universe | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| Sign ensemble, BTC | 0.6193 | 14.86% | 8.51% |
| Continuous z ensemble, BTC | 0.1306 | 1.11% | -1.50% |
| Sign ensemble, BTC/ETH | 0.7311 | 16.40% | 11.08% |
| Continuous z ensemble, BTC/ETH | 0.2021 | 2.14% | -0.09% |

R17 changes one fixed horizon set from30/90/180days to7/30/90days, retaining
existing feature calculations, eligibility, volatility sizing and execution.
R18 combines independent ranker and BTC momentum targets in one account.
Declared BTC capital fractions0.50 and0.20 leave the remainder for the ranker.
Each component has a0.80 gross target budget. Ranker construction retains its
original position/net/cluster limits, and component risk checks cannot be
hidden by BTC target cancellation. Combined targets receive one account risk
check and one order plan. Shared turnover allowance2.00 and deadband0 are
explicit; independently maintained sleeve deadbands are not represented.
No component leverage increase is implicit in the blend.

TheR18 ranker-heavy allocation is informed by the observed difference in
component volatility. It is another development trial, not an allocation
chosen before seeing component returns. Both fractions will be reported.
The frozen R6 control will be replayed with the new executable.

Independent review by gpt-5.6-sol found no material defect invalidatingR11–R16.
It confirmed completed feature timestamps, benchmark eligibility, consensus
matching, continuous portfolio state, matched benchmark clocks and the
missing-final-mark failure. The legacy consensus timer loader does not itself
enforce all purge/checksum metadata. ActualR14 model artifacts satisfy those
conditions and are hashed in provenance; the loader must be tightened before
any future consensus promotion. R14 remains rejected.

R17 fails: BTC fast momentum has Sharpe0.0660 and return-1.97%; BTC/ETH
has Sharpe0.4187 and return9.81%. Stress returns are-17.00% and-7.25%.
Shorter horizons increase costly reversals without producing a better signal.

R18 also fails the broader improvement requirement:

| Allocation | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| Frozen R6 control | 2.5278 | 9.89% | -0.25% |
| 50% ranker / 50% BTC momentum | 1.3107 | 12.17% | 3.15% |
| 80% ranker / 20% BTC momentum | 2.0785 | 11.38% | 0.29% |

The ranker-heavy blend loses1.31% in the later half, Sharpe-0.5030.
It is not accepted merely because its full-period minimum passes.
The new executable reproduces the frozen control's financial metrics exactly.
Cross-model review confirms sleeve sizing, BTC netting, component risk
rejection and the single shared execution/funding path. A follow-up config
guard now rejects active blends declaring a gross target other than the fixed
0.80 mandate; the recordedR18 configs already use0.80. A100%BTC fraction
still requires a valid ranker artifact because scoring precedes construction;
that endpoint is not used in these experiments.

R19 retests the16-name global and12-per-side selectors with the current
frozen venue-cohort model. They return6.75% withSharpe1.6576 and7.84% with
Sharpe1.9824 respectively, below the original9.89% return. Stress returns
are1.64% and-0.39%. Neither is promoted. R20 fits one120-hour ranker label horizon, with a November20 training
cutoff, for daily and three-day rebalance comparisons. Daily trading returns
7.74%,Sharpe1.5553,stress1.59%; three-day trading returns7.73%,Sharpe1.8172,
stress5.25%. The latter loses0.32% in the later half, Sharpe-0.1585.
The model clears the numerical full-period minimum but does not improve
the broader result. Independent review verifies its47-hour training embargo,
unchanged features/recipe and exact default24-hour matrix compatibility. All models and parameters remain recorded even when
a trial fails.

## R21 native funding and R22 exposure

R22 doubles the gross target from0.80 to1.60 and doubles the related position,
net, cluster and turnover limits. Hourly participation stays1%, capital stays
$100,000 and costs remain unchanged. This is a risk-sizing experiment with
the same forecasts. It replays new orders and partial fills; it does not
multiply the saved return series.

| Policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| One-day ranker, doubled exposure target | 2.5941 | 17.83% | 4.24% |
| Five-day ranker, doubled target, three-day trading | 1.7236 | 12.35% | 8.99% |

The one-day policy has6.75% maximum drawdown. Its later half gains1.24%
withSharpe0.5685, so later-period weakness is reduced but remains visible.
BTC returns-29.59%, a47.42 percentage point gap. The20-day block bootstrap
Sharpe95% interval is[-0.3571,5.2176], without correction for strategy
selection. Independent raw fill/funding/cash audits pass both central and
stress. Central actual gross exposure averages 46.42% and peaks at 117.69%.
Partial fills let actual net exposure range from -34.48% to +46.90%, beyond
the target portfolio's net limit. These are measured holdings, not target weights.

The candidate was locked before collecting the August 1 through September 7
confirmation outcomes. Both continuous 285-day replays reproduce every
financial field of the original 246-day development prefix exactly.

| Confirmation period | Strategy return | BTC return |
|---|---:|---:|
| Central costs | -10.72% | +23.83% |
| Stressed costs | -12.27% | +23.83% |

R22 is rejected. Its full extended central Sharpe is 0.6686 and return is
5.08%. The independent audit reconciles every fill, cash balance, funding
payment and NAV across both 285-day runs. Funding was positive and central
costs were only $195.63 during confirmation; the positions caused the loss.
The 38-day confirmation Sharpe is descriptive because the window is short.
All later experiments must label these now-exposed dates as development.
The original lock and failed evaluations remain immutable under
`var/research/lighter-locked-confirmation/`. Independent evidence and a
rerunnable audit are under `var/research/lighter-confirmation-audit/`.

R21 adjusts the frozen one-day ranker's conviction with native funding after
unchanged price scoring and feature normalization. The expected next-day
long funding cost is the sum of signed fractional hourly rates over exactly
[D−7days,D), divided by7. Positive long funding cost lowers long conviction
and raises short conviction. Directions remain price-confirmed; the rule
cannot flip a signal or revive a zero price forecast. Realized funding still
uses the API's per-unit payment values and is included once in cash.

The supplemental funding collection preserves364 existing pages and adds45
warmup pages. All363 overlapping events match. Of7,351 eligible asset-days,
7,339 have168 preceding observations; the12 exceptions precede recorded
creation ofDASH,AXS orBIO. Those explicit unavailable windows retain price
conviction and emit warnings. There are no interior funding holes.
At normal risk, the carry adjustment returns 9.43% with Sharpe 2.2342;
stress returns -0.34%. At doubled risk it returns 18.30% with Sharpe 2.6844;
stress returns 3.90%. The doubled price-only control returns 17.83% centrally
and 4.24% under stress. The small central gain and worse stress result did
not justify choosing carry over the simpler price-only candidate. Both
price-only controls reproduce their original measured results exactly.

## R23 and R24 market context

R23 tests a fixed rule: positive completed BTC 30-day momentum selects a BTC
long target at 80% of NAV; otherwise the original ranker supplies targets.
The comparison includes both the original ranker and a ranker with the same
larger turnover allowance needed to switch into BTC. Every policy trades
through one continuous account with native partial fills and funding.

R24 appends four raw completed BTC inputs after the ranker's unchanged
cross-sectional normalization: 7-, 30- and 90-day returns and 30-day
volatility. The original training dates, targets and fixed fitting recipe
remain unchanged. An unchanged-model control uses the same executable and
extended data. The training driver checks every original feature and target
against R6 before fitting.

Both experiments use all 285 days through September 7 as development data.
They were declared before their outcomes, but the dates have already been
examined. No subsequent result on this period is an unseen confirmation.

| Extended-period policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| Original ranker | 0.9870 | 4.39% | -6.58% |
| Ranker with turnover 2.00 | 0.5878 | 3.08% | -9.29% |
| Positive BTC monthly trend switch | 0.4812 | 6.49% | -7.95% |
| Ranker with raw BTC context | 1.4283 | 7.36% | -2.89% |

Neither new policy passes. The switch improves the second half to 15.59%
with Sharpe 1.7204, but loses 7.88% in the first half. BTC context improves
the full-period ranker result while its second half still loses 6.37%.
All 6,486 original training rows preserve their existing features and
targets exactly. The unchanged ranker control reproduces its full extended
financial path under both executables.

Independent side attribution identifies the later short book as the main
failure. At doubled risk, later-half altcoin shorts lose $14,237 while
altcoin longs gain $2,466. ZEC and PUMP shorts account for most of the loss.
R25 therefore tests removing positive-monthly-momentum short targets after
unchanged construction, without redistributing their weight.

R26 separately tests whether predicting absolute per-risk returns with the
new raw market inputs can choose useful long/short proportions. It compares
relative and absolute models under the same existing directional sizing
rule. The earlier broad-market absolute-label study failed with normalized
inputs; that negative evidence remains in `absolute-label-unbalanced.md`.

## R25 through R28 directional selection

| Policy | Sharpe | Return | Stress return |
|---|---:|---:|---:|
| Ranker with matched wider net cap | 0.4687 | 2.07% | -5.93% |
| Positive-month short veto | 0.4315 | 2.31% | -10.37% |
| Relative model, directional volatility sizing | 0.2158 | 0.72% | -6.63% |
| Absolute model, directional volatility sizing | 1.3211 | 16.09% | 8.60% |
| One-day absolute model, three-day trading | -0.0228 | -1.18% | -3.83% |
| Five-day absolute model, three-day trading | -0.2053 | -3.11% | -5.19% |

The veto improves the later half to 1.51%, but does not meet the goal.
Its matched control widens the net limit from 0.10 to 0.400001. That also
changes the planner's release of deferred hedging orders beyond the turnover
budget, so this control is not financially identical to R6. Both veto and
control share the widened limit. The extra decimal quantum avoids rejecting
an exactly 0.40 mathematical sleeve because six Decimal weights sum to
0.4000000000000000000000000002; a regression test preserves that case.

The one-day absolute model is more promising. Both halves are profitable:
8.24% with Sharpe 1.1375, then 7.25% with Sharpe 1.9203. It still fails the
full-period minimum. Its model uses the four common BTC inputs in 3,119 of
4,200 tree splits, compared with 289 for the relative model. This describes
the model structure; it does not establish that those splits generalize.

R27's slower trading fails despite lower turnover. All 6,254 five-day
training rows retain the earlier five-day features and targets exactly
after excluding the appended context inputs. The earlier cutoff preserves
the five-day label embargo.

R28 keeps the one-day absolute model and daily trading. It compares the
existing cost-aware joint allocation with a version that selects by
expected return per unit of volatility. The trend-sizing control is rerun
alongside both. All three share target gross 0.80, position cap 0.25,
turnover 2.00, and gross/net/cluster limits of 1.00. Net and cluster limits
therefore differ from R26, which used 0.80; the control measures that effect.
No new model is fitted for R28.

## R28 through R32: sizing, execution and native training labels

All 285 days from November 27, 2025 through September 7, 2026 are exposed
research dates. The following tests use $100,000 and the same actual funding,
measured hourly volume limits and execution costs. BTC returns -11.39% over
this interval at the standard 01:00 marks.

| Trial | Change | Net Sharpe | Return | Stressed return |
|---|---|---:|---:|---:|
| R26 | Absolute daily model, volatility sizing | 1.3211 | 16.09% | 8.60% |
| R28 | Cost-aware return-ranked sizing | 0.1843 | 0.99% | -5.32% |
| R28 | Cost-aware risk-ranked sizing | -0.0814 | -4.63% | -11.66% |
| R29 | One-hour execution, 04:00 marks | 1.3234 | 15.76% | 8.30% |
| R29 | Four-hour execution, 04:00 marks | 0.8923 | 11.17% | 0.53% |
| R30 | Add relative and absolute model targets | 1.2409 | 12.60% | -1.21% |

None meets the minimum. R30 adds the two original mandates at full account
NAV, with explicit combined risk limits and one shared execution ledger.
An idealized average of the saved return streams suggested Sharpe 1.64;
the actual account does not reproduce it. Its later half loses 4.38% while
BTC gains 6.97%. The idealized series is only a diversification diagnostic.

The R26 capacity audit regenerates all 570 central/stress plan IDs exactly
and reconciles cash, funding and NAV. Only 32.64% of requested central
notional fills. Capacity binds on 280 of 285 days; $6.48 million of the
$6.62 million cancelled notional has a binding measured hourly limit.
Average constructed gross exposure is 39.51%, versus 27.76% actually held.
The later half fills only 20.88% of requested notional. Four-hour execution
was tested because of this evidence, but R29 shows that extra fills also
increase exposure to losing forecasts and costs.

R31 changes only training return labels to native Lighter 01:00 opens.
The frozen source has 630 pages and all 6,486 required pairs. APT on June 24
at 16:00 and DOGE on July 29 at 14:00 have invalid OHLC and remain unchanged
in the raw collection. The distinct label reader validates source hashes,
metadata and every timestamp, then validates OHLC and coverage at 01:00.
It cannot supply execution history. The full execution reader remains strict.

The native matrix preserves all 6,486 row identities, 241 dates, 45 assets,
71 features and volatility inputs exactly against the SHA-pinned R24 matrix.
6,485 raw return labels change. Exact missing entry or exit prices abort the
build; no external fallback or row deletion is allowed. An independent review
found the first parity script did not pin the reference or assert the declared
counts. A stricter audit passes before the first replay; the initial model had
already been fitted on this same verified matrix. Both records remain.

R32 averages five fixed seeds equally for each label source, with 300 trees
per seed and unchanged hyperparameters. It compares both ensembles against
their seed-zero controls. No best-seed selection is performed. R31 native single-seed training returns 15.35% with Sharpe 1.2583, slightly
below its external control. The five-seed external ensemble returns 19.08%
with Sharpe 1.5120, and the native ensemble returns 18.71% with Sharpe 1.5070.
Both clear the full-period point threshold, with small margins. Both controls
reproduce every financial step of their prior runs exactly.

The native ensemble earns 10.49% in the first half and 7.44% in the later
half, compared with BTC -17.17% and +6.97%. Later-half Sharpe is 2.0001.
Its full stressed return is 11.14%, but stressed Sharpe is only 0.9560.
The 10,000-resample, 20-day block-bootstrap 95% Sharpe interval is
[-0.588, 3.759]. This interval does not account for strategy selection across
all the research trials and does not establish a future Sharpe above 1.5.
April, July and August still trail BTC by substantial amounts.

An independent Rust audit reproduces native ensemble cash, funding, NAV,
exposures and performance. It checks 7,677 central and 7,665 stressed fills,
with only negligible Decimal rounding in capacity/cost identities. A copied
fold with saved cash changed by $1 fails the auditor. The candidate remains
provisional; no policy is deployed.

R33 tests the frozen single-seed R26 model at $1,000, $10,000 and $100,000.
Central Sharpe/return are 0.910/8.70%, 0.905/12.42% and 1.321/16.09%.
Smaller accounts do not rescue that model. The unchanged $25 minimum order
means this changes both liquidity constraints and effective order granularity.
Both $100,000 controls reproduce R26 exactly. Acceptance capital stays $100,000.

R34 isolates the economic filter from the other changes made in R28. It
keeps the native five-seed ensemble and original volatility sizing, removing
only forecasts below the configured round-trip commission/spread floor.
It adds no fitted threshold and consumes no realized execution cost or volume.
The declaration precedes implementation and replay.

Evidence: `var/research/lighter-r26-capacity-diagnosis/`,
`var/research/lighter-relative-absolute-diversification/`,
`var/research/lighter-optimization-r28-cost-aware/`,
`var/research/lighter-optimization-r29-control/`,
`var/research/lighter-optimization-r29-extended/`,
`var/research/lighter-optimization-r30-overlay/`,
`var/research/lighter-training-hourly/`, and
`var/research/lighter-ranker-r31-native-hourly/strict-matrix-parity.json`.

## R34 through R38: tests around the native ensemble

| Trial | Change from native ensemble | Net Sharpe | Return | Stressed return |
|---|---|---:|---:|---:|
| R32 | Fixed five-seed native ensemble | 1.5070 | 18.71% | 11.14% |
| R34 | Fixed round-trip cost filter | 1.5040 | 18.65% | 11.12% |
| R35 | Eight slow momentum/volatility inputs | 1.3674 | 17.66% | 10.42% |
| R36 | Double volatility and account risk budgets | 1.1881 | 26.96% | 14.66% |
| R37 | Training-scaled Huber loss | 1.3837 | 17.26% | 9.87% |

These changes do not improve the combined requirement. R36 raises return by
accepting more risk, with lower Sharpe; it does not change model predictions.
R35 improves later-half return to 10.09%, versus BTC 6.97%, but weakens the
full-period result. Every matched control reproduces R32's financial path.

The training-tail diagnostic finds that the top 1% of native observations
contributes 20.79% of initial squared error but only 6.37% of absolute gradient.
No individual row contributes 1% of squared error. Default Huber clipping at
0.9 would not bind anywhere along the 300-tree model's residual path.
R37 therefore uses one fixed training-only p95 residual rule, alpha
0.13750992596765854. It retains every row and label, and uses five equal seeds.
No alternative clipping thresholds are tested. Later-half return falls to
6.05%, below BTC, so this objective is rejected.

The equal-seed export was independently refitted from the frozen trainers.
Both external and native fits reproduce all 1,500 trees and all metadata
exactly, excluding the new training timestamp. The rerunnable command,
environment settings, package versions and hashes are recorded in
`var/research/lighter-r32-fit-reproduction/verified-v2/`.

R38 tests execution selection after full-cohort inference: trade BTC and ETH,
or BTC alone, with unchanged predictions, volatility budget and 25% position
cap. These assets are specified by liquidity and benchmark role, without
choosing them from measured per-asset strategy returns. It does not shrink
the model's normalization cohort or force an ineligible asset into the book.
It tests whether reduced altcoin exposure and deeper markets improve the
realized account.

| R38 book | Net Sharpe | Return | Later half | Stressed Sharpe | Stressed return | Turnover per rebalance | Cost drag |
|---|---:|---:|---:|---:|---:|---:|---:|
| All eligible assets (R32 control) | 1.5070 | 18.71% | 7.44% | 0.9560 | 11.14% | 10.32% | 81 bps |
| BTC and ETH | 1.5516 | 26.51% | 7.06% | 0.5113 | 6.83% | 28.55% | 200 bps |
| BTC only | 0.8865 | 7.32% | 2.18% | -0.1203 | -1.46% | 14.76% | 100 bps |

The later half spans the same 143 days in every row; BTC gains 6.97% there.
The all-asset control reproduces R32 exactly. BTC/ETH clears the full-period
conjunction with a 7.8 point return gain, but the later half now fails the
Sharpe minimum at 1.0105 and beats BTC by 0.09 points. It is also a riskier
book: realized volatility 20.8% against 15.4%, mean gross exposure 42.7%
against 28.0%, and a 25% cap on two assets means gross can never exceed 50%.
Turnover per rebalance almost triples, so stressed execution removes 19.68
points of return and the stressed later half loses 2.03%. BTC alone fails
everything. The Aug 20 style concentration is worse than R32: the largest
day is 24.98% of net gain, the five largest 75.03%.

## R39 turnover reduction on the BTC/ETH book

R39 keeps the BTC/ETH routing and tests a fixed two-by-two: the existing
71-feature native ensemble versus the R35 eight-slow-feature ensemble, each
with the existing 0% or 20% same-side resize deadband. No refit, no new
threshold. The deadband never suppresses entries, exits or sign changes. The
daily full-feature cell reproduces R38's BTC/ETH path exactly.

| R39 cell | Net Sharpe | Return | Later-half Sharpe | Later-half return | Stressed Sharpe | Stressed return | Turnover | Cost drag |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Full features, daily resize | 1.5516 | 26.51% | 1.0105 | 7.06% | 0.5113 | 6.83% | 28.55% | 200 bps |
| Full features, 20% deadband | 1.6478 | 28.53% | 1.0742 | 7.58% | 0.6527 | 9.30% | 27.04% | 190 bps |
| Slow features, daily resize | 1.5357 | 26.78% | 1.2067 | 8.80% | 0.5477 | 7.58% | 27.61% | 194 bps |
| Slow features, 20% deadband | 1.5779 | 27.85% | 1.2383 | 9.10% | 0.6312 | 9.13% | 26.51% | 187 bps |

The deadband removes only about 5% of turnover, because the BTC/ETH book
trades on direction changes and volatility resizing that the deadband does
not touch. Stressed returns stay between 6.8% and 9.3% in every cell, versus
11.14% for the all-asset control, so the liquid book does not fix cost
sensitivity. The slow-feature model helps the later half but lowers the full
Sharpe. The best cell, full features with deadband, has a 95% block-bootstrap
Sharpe interval of -0.25 to 3.55 and a 5.8% bootstrap probability of loss
over 285 days. The interval includes zero for all seven central books.
No R39 cell is accepted. Every cell's replay, evaluation and paired BTC
bootstrap are kept under `var/research/lighter-optimization-r39-liquid-turnover/`.

## R40 position cap on the BTC/ETH book

The user pointed out that with two tradeable assets the 25% per-position cap
holds gross at or below 50% and leaves the 80% gross target unused. R40 keeps
the R39 best cell (frozen R32 model, 20% deadband, BTC/ETH routing) and only
raises the cap: 40% fills the gross target exactly, 80% makes the cap
non-binding so the volatility budget and net/cluster limits govern size.
No refit, same capital, participation, execution and funding.

| Cap | Net Sharpe | Return | Later-half Sharpe | Later-half return | Max drawdown | Volatility | Stressed Sharpe | Stressed return | Turnover | Cost drag |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 25% (R39 control) | 1.6478 | 28.53% | 1.0742 | 7.58% | 10.83% | 20.8% | 0.6527 | 9.30% | 27.04% | 190 bps |
| 40% | 1.8638 | 43.17% | 1.6546 | 16.49% | 12.45% | 26.5% | 0.7663 | 14.01% | 38.77% | 274 bps |
| 80% | 1.7635 | 42.58% | 1.6022 | 17.53% | 12.91% | 28.0% | 0.6200 | 11.05% | 41.04% | 290 bps |

The control reproduces R39 exactly. The 40% cap is the first book in this
research line that passes the joint condition in the full period and in both
chronological halves, with the later half at 16.49% against BTC 6.97%. Mean
gross exposure rises from 42.7% to 55.2% and peaks at 80.6%. The Sharpe gain
comes from the later half: the model's later-period calls were right and the
lower cap had been truncating them. The 95% bootstrap Sharpe interval is
0.06 to 3.66, the first interval that excludes zero, with a 3.5% bootstrap
probability of loss and 2.5% probability of a drawdown beyond 25% over 285
days. The 80% cap adds no return and more risk; the volatility budget rather
than the cap governs above 40%.

Stressed execution still fails: the 40% book earns 14.01% at Sharpe 0.7663,
with 2,596 bps of cost drag against 274 bps central. Turnover per rebalance
is 38.77%, nearly four times the all-asset book. Monthly, the 80% book
trails BTC in April (9.36% against 13.44%), July (7.36% against 8.18%) and
August (9.65% against 20.42%), so it still does not fully participate in
rallies, though it now captures roughly half of August rather than an eighth.

R40 is not accepted. It is the strongest development-period result so far,
on 285 reused dates, with the stressed cost case unresolved. All artefacts
are under `var/research/lighter-optimization-r40-liquid-cap/`.

## R41 full gross on the BTC/ETH book

The user then asked for a 50% cap with the full 100% gross target. R41 raises
target gross to 100%, the cap to 50% and the net and cluster limits to 100%
so nothing binds before the cap. The R40 40%/80% book is the matched control
and reproduces exactly.

| Book | Net Sharpe | Return | Later-half Sharpe | Later-half return | Max drawdown | Volatility | Mean gross | Stressed Sharpe | Stressed return | Turnover |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Cap 40%, gross 80% | 1.8638 | 43.17% | 1.6546 | 16.49% | 12.45% | 26.5% | 55.2% | 0.7663 | 14.01% | 38.77% |
| Cap 50%, gross 100% | 1.8132 | 45.27% | 1.8077 | 20.66% | 13.60% | 28.6% | 59.6% | 0.6417 | 11.81% | 44.18% |

Full gross adds 2.1 points of return for a lower full-period Sharpe, a
deeper drawdown and 3,000 bps of stressed cost drag. The later half improves
to 20.66% against BTC 6.97%. The bootstrap Sharpe interval widens back to
include zero, -0.01 to 3.58. Mean gross stays near 60% because the
volatility budget, not the cap, sizes most days. This is user-directed
sizing on the same 285 reused days; it is recorded, not accepted.

## R42 three-year walk-forward on Binance proxies

The user asked for at least two years, then three. Lighter's data begins on
January 17, 2025, so a three-year test cannot use native fills or funding.
R42 runs the R41 book (BTC/ETH routing, 50% cap, 100% gross, 20% deadband,
full-cohort ranker inference) on Binance USD-M bars and funding from the
signal store, with the Lighter cost assumptions as proxies: 2.2 bps
half-spread, zero commission, impact 0.1, 1% participation, one-hour delay;
stress multiplies spread by 10 and impact by 4. The window is August 7, 2023
through August 6, 2026, the last complete day in the store, 1,096 days.

It is an expanding walk-forward with six equal test blocks. Each block is
scored by a model trained only on dates at least two days before it, with
the R32 recipe unchanged: per-risk absolute reward, regression, five equal
seeds of 300 trees, one-hour lag, 24-hour hold, BTC daily raw market
context, 71 inputs. The matrix command gained a `--market-context` flag so
the external-only build emits the same contract. No search was run. The
universe is the recorded point-in-time Binance liquidity snapshots, whose
top-N varies between 24 and 40 over the period. This is a signal robustness
check under proxy execution, not a Lighter performance estimate.

| Fold | Test window | Model through | Sharpe | Return | Max drawdown | Stressed return |
|---|---|---|---:|---:|---:|---:|
| 1 | Aug 7, 2023 to Feb 4, 2024 | Aug 5, 2023 | -1.491 | -20.66% | 26.14% | -35.45% |
| 2 | Feb 5 to Aug 4, 2024 | Feb 3, 2024 | -0.997 | -16.71% | 23.55% | -31.67% |
| 3 | Aug 5, 2024 to Feb 2, 2025 | Aug 3, 2024 | -0.283 | -5.98% | 20.19% | -20.28% |
| 4 | Feb 3 to Aug 3, 2025 | Feb 1, 2025 | 1.042 | 13.83% | 14.00% | -4.30% |
| 5 | Aug 4, 2025 to Feb 1, 2026 | Aug 2, 2025 | 1.664 | 24.11% | 16.17% | 4.13% |
| 6 | Feb 2 to Aug 6, 2026 | Jan 31, 2026 | 0.874 | 10.58% | 23.15% | -11.16% |

| Three-year series | Return | Max drawdown | Pooled Sharpe |
|---|---:|---:|---:|
| Strategy, central costs | -2.94% | 45.38% | 0.110 |
| Strategy, stressed costs | -68.87% | 71.96% | -1.191 |
| BTC buy and hold | +122.31% | 52.97% | 0.803 |
| ETH buy and hold | +4.30% | 67.55% | |
| 50/50 BTC/ETH buy and hold | +63.30% | 58.01% | |

The book loses money for the first eighteen months and only recovers from
February 2025. The 95% bootstrap Sharpe interval is -1.06 to 1.32, the
bootstrap probability of a three-year loss is 52.7%, and daily correlation
with BTC is 0.13. Under stressed costs the book loses more than two thirds
of its capital, because turnover per rebalance stays between 45% and 58%
in every fold. The chart with equity, buy-and-hold and drawdown panels is in
`var/research/lighter-optimization-r42-three-year/chart/equity.png`; the
Rust `equity-series` example chains the fold NAVs and computes drawdown, and
the plotter only draws.

The three folds that overlap the native development period (February 2025
onward) are positive, which is consistent with the 285-day results, but the
earlier three show that the same recipe, retrained honestly at each boundary,
had no edge in 2023 and 2024 and trailed BTC by 125 points over the full
window. The late-2025 strength in the 285-day tests is therefore period
specific. R42 is rejected as a three-year strategy, and it removes the
basis for treating the R40/R41 development results as evidence of a durable
edge.

## R43 three-year baselines for the full-altcoin books

The user asked to keep improving and to prefer the full-altcoin book over
BTC/ETH. The three-year harness is now a reusable runner,
`bin/research-three-year.sh`, that takes a declaration with folds, trainer
recipe, cost cases and candidates, fits one model per fold per candidate,
replays, registers, exports buy-and-hold, chains the fold NAVs and runs the
paired bootstrap. R43 declares the two existing all-asset books as controls,
same matrix, folds and costs as R42, nothing tuned.

| Book | Sharpe | Return | Max drawdown | Folds positive | Stressed return |
|---|---:|---:|---:|---:|---:|
| Absolute book, R32 recipe (tsmom_vol_parity, per-risk absolute reward) | 0.29 | 11.3% | 23.3% | 3 of 6 | -36.6% |
| Relative long/short ranker, deployed config (risk_adjusted_score_selected, demeaned per-risk reward, 10% net) | 1.47 | 137.5% | 19.3% | 6 of 6 | -61.0% |
| BTC buy and hold | 0.80 | 122.3% | 53.0% | | |

The relative long/short book is the only multi-year result in this line that
holds: positive in every fold (19.0%, 17.0%, 33.0%, 6.2%, 19.4%, 1.1%),
daily correlation with BTC of -0.26, a bootstrap Sharpe interval of 0.51 to
2.43 and a 0.4% bootstrap probability of a three-year loss. Its chart is at
`var/research/lighter-optimization-r43-three-year-baselines/chart-relative-ls/equity.png`.
The absolute book is rejected on three years. The long/short book's weakness
is turnover: 70% to 85% of the book turns over every day, about 26 fills a
day, roughly 370 bps of cost drag per half year at central costs, and a 61%
loss under the ten-times-spread stress case.

## R44 turnover levers on the long/short book

R44 tests three existing levers separately, model and selection unchanged,
and adds a moderate cost case (3x spread, 2x impact) to every scenario for
information. The control reproduces R43 exactly.

| Lever | Sharpe | Return | Max drawdown | Turnover per day | Moderate return | Stressed return |
|---|---:|---:|---:|---:|---:|---:|
| Control | 1.470 | 137.5% | 19.3% | 75.0% | 56.0% | -61.0% |
| 20% resize deadband | 1.418 | 130.0% | 19.6% | 74.2% | 53.4% | -60.9% |
| Cost deadband 6x round trip | 1.448 | 134.3% | 20.0% | 74.9% | 54.8% | -61.2% |
| Turnover budget 0.25 | 1.113 | 77.1% | 15.6% | 46.3% | 40.7% | -40.3% |

Resize and cost deadbands do nothing because the churn is membership: in a
representative fold the book records 1,128 entries and 1,188 exits over 182
days, about six names in and six out every day of a 24-name book. Pacing
with a smaller budget halves turnover but discards 60 points of return.
None qualifies. The next lever is selection hysteresis, which did not exist
in the code and is added as `ranker_hold_rank_multiple` for R45.

## R45 selection hysteresis

`ranker_hold_rank_multiple` keeps an incumbent in the score-selected book
while it ranks inside the top 24 times the multiple and its held side still
agrees with the signal; new names fill only vacated slots from the top.
Sizing is unchanged. It is unit tested and validated to the score-selected
ranker with global selection.

| Multiple | Sharpe | Return | Turnover per day | Moderate return | Stressed return |
|---|---:|---:|---:|---:|---:|
| Control | 1.470 | 137.5% | 79.6% | 56.0% | -61.0% |
| 1.5 | 1.536 | 149.0% | 76.6% | 63.4% | -56.4% |
| 2.0 | 1.529 | 147.9% | 76.6% | 64.6% | -57.0% |
| 3.0 | 1.529 | 147.9% | 76.6% | 64.6% | -57.0% |

Hysteresis is a small, consistent gain, but it removes only three points of
turnover. Multiples 2 and 3 are identical because the eligible
cross-section is about 40 names, so any window beyond two covers all of it.
The fills explain why: names exit and re-enter on the opposite side within
a day or two, for example ETH exiting long on October 4, 2024 and entering
again two days later, and BTC flipping from short to flat to short. The
churn is the sign of a one-day forecast, and rank hysteresis cannot hold a
name whose side has flipped. R46 therefore tests a slower target: the same
recipe fitted on 48-hour and 72-hour forward labels, still trading daily.

## R46 forecast horizon

Each horizon has its own matrix from the same Rust command, and every
candidate uses a uniform five-day purge so the longest label never overlaps
its test block. The 24-hour control therefore differs from R43 only by three
days of training cutoff.

| Label horizon | Sharpe | Return | Turnover per day | Moderate return | Stressed return |
|---|---:|---:|---:|---:|---:|
| 24h (control, 5-day purge) | 1.313 | 117.6% | 80.3% | 44.7% | -63.2% |
| 48h | 0.981 | 77.2% | 63.5% | 26.8% | -58.7% |
| 72h | 0.909 | 69.4% | 54.5% | 27.1% | -51.5% |

Slower targets trade less but lose more edge than cost, and stress stays
deeply negative. The effect the ranker captures is a one-day cross-sectional
one. Two side observations matter. Moving the cutoff by three days changes
the control from Sharpe 1.47 to 1.31, so single-run differences of that size
are within fit noise. And the cohort the ranker sees is only 24 to 40 names
while the book holds 24, which leaves almost no selection room; R47 widens
the point-in-time cohort to the top 80 and top 120 by trailing liquidity in a
separate research data root.

## R47 cohort breadth

The live universe snapshots mark only the top 24 to 40 names eligible. R47
records new point-in-time snapshots with the same Rust liquidity ranking at
top 80 and top 120 into `var/research/three-year-data-top{80,120}/`, which
symlink the live bars and funding and carry their own universe directory,
so the live store is untouched. Eligibility is dated: 57 names clear the
screens in August 2023, 105 in 2025, and only 41 by August 2026 as altcoin
volumes fall. Each breadth gets its own matrix and per-fold refits, because
rank-normalised features change with the cohort.

| Cohort and book | Sharpe | Return | Max drawdown | Folds positive | Turnover per day | Moderate return | Stressed return |
|---|---:|---:|---:|---:|---:|---:|---:|
| Top 40, 24 names (R43 control) | 1.470 | 137.5% | 19.3% | 6 | 75.0% | 56.0% | -61.0% |
| Top 80, 24 names | 1.850 | 255.4% | 25.0% | 5 | 71.0% | 137.0% | -40.9% |
| Top 80, 40 names | 1.666 | 189.1% | 25.0% | 5 | 69.6% | 97.1% | -49.9% |
| Top 80, 40 names, 1.5x hysteresis | 1.727 | 202.2% | 25.0% | 5 | 66.7% | 105.1% | -41.9% |
| Top 120, 24 names | 1.698 | 233.9% | 23.5% | 6 | 66.4% | 124.2% | -37.3% |
| Top 120, 40 names | 1.725 | 207.9% | 23.6% | 5 | 66.3% | 113.9% | -40.9% |
| Top 120, 40 names, 1.5x hysteresis | 1.848 | 236.0% | 23.6% | 5 | 63.4% | 136.9% | -31.8% |

Breadth is the first step change in this line. The top-120 book with 40
names and hysteresis earns 236% at Sharpe 1.85 against BTC's 122%, keeps
Sharpe 1.33 and 137% at three times the measured spread, and carries a
bootstrap Sharpe interval of 0.90 to 3.10. The sixth fold, February to
August 2026, is flat in every cell (-3% to 0%) as the eligible cohort
shrinks to about 40 names; the strategy needs breadth and 2026 offers less.
Stress at ten times spread remains negative in every cell. Chart:
`var/research/lighter-optimization-r47-breadth-top120/chart-sel40-hold15/equity.png`.
This cell is carried forward as the working book. R48 adds staggered
sleeves, new code, so that each name holds its one-day forecast for N days
while the book still trades daily.

## R48 and R49 staggered sleeves

`ranker_tranches` assigns every asset to one of N fixed sleeves by a stable
symbol hash; only the sleeve whose turn it is may change weight, and every
other position keeps its current weight, including a pending exit. The
first implementation (R48) let the mixed book breach the 40-position count
and the 10% net band, so the risk check rejected the plan on about a
quarter of days and nothing traded; that run is retained as a failed trial.
R49 drops the smallest fresh entries to respect the count and scales one
side of the traded sleeve so the whole book nets what the constructor
asked for; held positions are never touched.

| Sleeves (R49, top 120, 40 names, 1.5x hysteresis) | Sharpe | Return | Max drawdown | Turnover per day | Mean gross | Cost drag per fold | Moderate return | Stressed return |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 (control, reproduces R47) | 1.848 | 236.0% | 23.6% | 62.3% | 62.6% | 314 bps | 136.9% | -31.8% |
| 2 | 1.164 | 82.8% | 20.2% | 26.1% | 45.8% | 129 bps | 53.5% | -9.2% |
| 3 | 0.931 | 43.1% | 10.7% | 18.9% | 44.7% | 95 bps | 30.4% | -8.9% |
| 4 | 0.759 | 41.2% | 26.5% | 13.5% | 41.6% | 67 bps | 30.3% | +0.6% |

Sleeves remove more than half the cost but more than half the edge, and a
sleeve sized for a whole book leaves the mixed book under-invested, mean
gross 46% against 63%. They win only under the ten-times-spread case and
lose under central and moderate costs. Rejected; the daily book stands.

Every design choice so far, breadth, book size and hysteresis, was made on
August 2023 to August 2026. R50 runs the chosen book on September 2020 to
August 2023, a period never used to evaluate any candidate in this line,
as a holdout of those choices.

## R50 holdout on September 2020 to August 2023

Same expanding walk-forward, recipe, costs and research data root; six
folds of about 175 days. The first fold's model has under nine months of
training rows. The narrow live-store cohort runs as a matched control.

| Book | Sharpe | Return | Max drawdown | Folds positive | Turnover per day | Moderate return | Stressed return |
|---|---:|---:|---:|---:|---:|---:|---:|
| Top 120, 40 names, 1.5x hysteresis | 2.213 | 287.5% | 13.5% | 6 | 63.4% | 189.8% | +1.1% |
| Top 120, 24 names | 1.988 | 286.6% | 19.0% | 6 | | 168.8% | -14.8% |
| Top 40 live store, 24 names | 1.979 | 217.6% | 23.5% | | | 118.3% | -36.2% |
| BTC buy and hold | | 165.7% | 76.6% | | | | |

The choices transfer. The chosen book is positive in every holdout fold
(80.2%, 28.5%, 7.6%, 24.3%, 13.2%, 10.6%), including the 2022 bear market,
and the wider cohort beats the narrow control on every measure again. The
bootstrap Sharpe interval is 1.26 to 3.24.

Chaining the twelve folds, September 2020 to August 2026, each dated by a
model that never saw it:

| Six years | Return | Max drawdown | Pooled Sharpe |
|---|---:|---:|---:|
| Strategy, central costs | +1,202% | 23.6% | 2.02 (bootstrap 1.31 to 2.82) |
| Strategy, moderate costs | +586% | 24.6% | |
| Strategy, stressed costs | -31% | 67.0% | |
| BTC buy and hold | +491% | 76.6% | 0.8 |
| ETH buy and hold | +390% | 79.3% | |

Daily correlation with BTC is -0.22 and the bootstrap probability of a
six-year loss is zero to four decimals. Chart:
`var/research/lighter-optimization-six-year-chain/chart/equity.png`.

What is still not established. The ten-times-spread stress case loses in
every period; the strategy's profit depends on execution near the measured
2.2 bps half-spread and 1% participation. The 2026 fold is flat as the
eligible cohort shrinks toward 40 names. And all of this is Binance proxy
execution with a Binance cohort: Lighter lists fewer names, and the native
measured-hour harness has only been run on the narrow venue cohort. The
next step is a native Lighter replay of this book on the widest
Lighter-listed cohort, which is the only evidence that would justify paper
trading it.

The plotted native ensemble account and BTC comparison are in
`var/research/lighter-optimization-r32-ensembles/chart/`. The plot only reads
saved Rust NAV and exposure values.

## R32 event and data-integrity audit

The audit in `var/research/lighter-r32-event-diagnosis/report.md` checks the
largest moves against native and external prices and measures return
concentration. No dates were removed. The Aug 20 BTC jump agrees across
Lighter and Binance within 0.08%, moves through traded hourly bars, and native
daily OHLC matches the 24 hourly bars for Aug 18 through 21. The largest
positive BTC day is actually Feb 7 at 10.15% on both sources.

R32 earns 2.60% on Aug 20, which is 15.37% of its full-sample net gain and is
spread across ETH, SOL, XRP, UNI and BTC longs. The largest single day is
17.64% of net gain and the five largest 76.20%. Leaving out one observed day
moves full Sharpe anywhere from 1.2764 to 1.9022, and 119 of 285 such
diagnostics fall at or below 1.5. The event is real, but the margin above the
threshold depends on ordinary observed days.

## R6 matched comparison

| Policy | Central Sharpe | Return | Maximum drawdown | Stress Sharpe |
|---|---:|---:|---:|---:|
| Original broad-cohort model | -1.0111 | -7.48% | 14.32% | -2.2757 |
| Lighter-cohort model, external labels | **2.5278** | **9.89%** | **4.12%** | -0.0329 |
| Same model, 20% relative resize deadband | 2.2206 | 8.95% | 3.91% | 0.1099 |

Central assumptions remain zero commission, 2.2 bps half-spread, impact
coefficient 0.1, and 1% participation. Stress raises half-spread to 22 bps and
multiplies impact by four. Under stress, the highest-Sharpe central policy
returns -0.25%; the deadband version returns +0.31%. Both are near flat.

The highest central result uses the simpler model without the deadband.
Long/short target sizing remains balanced; the tested directional trend rules
were weaker. Gross targets and position limits were not increased.

## What changed

The old ranker learned from 70,487 rows across 2,352 dates in a broad external
market cohort. Its inference cohort on Lighter was different. The replacement
uses 6,486 rows across 241 dates selected by historical Lighter creation and
prior activity, followed by the same external-liquidity top-30 and runtime
eligibility filters used at decision time. This also shortens the effective
training history, so the experiment does not isolate cohort membership alone.

Training decisions end November 24, 2025. External labels run from D+1h to
D+25h; all labels finish before evaluation begins November 27. The model keeps
the fixed 300-tree, 15-leaf LightGBM recipe and per-risk reward. Feature ranks
are computed over the decision cohort before unavailable future labels are
masked. Whole training dates affected by quarantine or insufficient cohort size
are recorded and omitted. Signal features and the selected model's training
labels remain external; evaluation prices, volumes and funding are native.

Execution now uses the native 00:00–01:00 hourly VWAP plus spread and impact.
Each fill is capped at 1% of the lower of prior native ADV/24 and observed
opening-hour quote volume. Observed volume affects realized fills only, never
selection or other orders. Zero-volume hours cannot fill orders. Remaining
quantity is cancelled after the hour.

Fills are booked at 01:00, and NAV uses the native 01:00 hourly open. Funding
applies to the old book over `(previous01:00, current01:00]` before replacement.
This completion-time convention approximates fills distributed through the
hour. The replay does not reconstruct individual queue positions, spreads or
within-hour funding ownership. Impact volatility still uses prior native daily
volatility converted to an hourly estimate.

The hourly collection has 585 declared successful requests across 45 selected
markets. Checksums, identities, OHLC, volume fields, timestamp grids and coverage
after first eligibility are validated. One inconsistent BTC VWAP at 15:00 on
February 25 is preserved and unusable; no traded midnight VWAP is affected.

## Validation and uncertainty

The 246-day path is continuous. Both halves retain the actual holdings path;
the second starts from the preceding NAV and preserves its first overnight
return. Observation labels are 00:00 decisions; actual valuations are 01:00.

| Part of the selected path | Sharpe | Return |
|---|---:|---:|
| November 27 to March 29, 123 days | 4.1683 | 9.8853% |
| March 30 to July 30, 123 days | 0.0230 | 0.0042% |
| Full 246 days | 2.5278 | 9.8899% |

Almost all profit came in the first half. A 10,000-draw, 20-day block bootstrap
with seed 42 gives a 95% full-period Sharpe interval of **[-0.539, 5.271]**.
The paired improvement over the original model is 3.5388 Sharpe units, with
95% interval **[-0.633, 7.548]**. These intervals do not correct strategy
selection bias. No dates here are presented as a fresh holdout.

The independent review verified model/config/suite hashes, all six R6 fold
sidecars, 246 decision dates, absence of the execution guard, prior-only
selection inputs, and financial reconciliation to about 1e-22 per step.
An independent raw-data audit verified every one of the selected path's 4,832
fills against hourly VWAP, 01:00 timestamps and the participation cap. Native
01:00 NAV reconstructs exactly; maximum numeric cap excess is 5.43e-23 dollars.
Commission is zero, spread costs total $952.11, and impact costs $234.22.

All 137 Rust package tests pass, including hourly capacity/VWAP/mark alignment,
exact 24-event funding boundaries and held-position reduction accounting.

## Buy-and-hold comparison

These benchmarks use native prices at the same 01:00 valuation timestamps.
They hold fixed quantities without rebalancing, funding, staking or one-off
trading costs. They are gross price references, not executable perpetual
strategies with identical constraints.

| Reference | Sharpe | Return | Maximum drawdown |
|---|---:|---:|---:|
| Selected strategy after modeled costs/funding | 2.5278 | 9.89% | 4.12% |
| BTC price | -0.9738 | -29.59% | 39.40% |
| ETH price | -0.8839 | -37.49% | 53.07% |
| Initially 50/50 BTC and ETH | -0.9691 | -33.54% | 46.16% |

## Earlier development results

These results use daily-open execution and ADV/24 capacity proxies. They are
retained for the trial record and must not replace the final measured-hour
comparison above.

| Candidate | Central Sharpe | Stress Sharpe |
|---|---:|---:|
| Corrected original ranker | -1.4108 | -3.1190 |
| Capacity-balanced target weights | -0.2268 | Not run |
| Continuous HTF momentum | -0.3575 | -0.5483 |
| Liquidity-selected breakout | 0.6252 | 0.4077 |
| Strongest-trend breakout selection | 0.9656 | 0.7067 |
| Reachable-capacity rank adjustment | -0.4561 | -2.0610 |
| Lighter cohort, external labels | 1.4993 | -1.5384 |
| Lighter cohort, native labels | 1.2321 | -1.6144 |
| Native labels plus capacity balance | 0.7224 | -2.3442 |
| External labels plus 20% resize deadband | 1.7869 | -1.2815 |

The deadband skips a same-side resize smaller than 20% of the existing position;
entries, exits and sign changes remain eligible. Its daily-proxy improvement
came from the holdings path, not lower aggregate execution costs. Total costs
increased slightly, from $1,683.62 to $1,692.42. An exact rerun reproduced every
financial step of that earlier result.

The hourly audit then found 2,247 of 4,798 positive-midnight filled asset-days
above 1% of observed turnover, including 26 above the entire hour. It also found
21 fills on zero-volume midnight bars totaling $478.10. This prompted R6;
the final result does not rely on those daily-proxy fills.

Three initial net-guard attempts failed closed before complete metrics because
a second quantity conversion could remove a closing tick. Review also found
final-day availability feeding batch allocation. Both issues were fixed and
covered by regression tests; the guard remains absent from the final policy.
A weekly-rebalance trial was deferred before measurement because the existing
suite requires daily valuation. All declarations and failed attempts remain.

## Reproduce and inspect

Run `bin/research-lighter-optimization.sh docs/research/lighter-optimization-r6-hourly.json`.
The runner snapshots its own source before execution and refuses to replace
results when input hashes change; a new executable requires a new output path.

- Final declaration: `docs/research/lighter-optimization-r6-hourly.json`.
- Final configurations: `var/research/lighter-optimization-r6-hourly/configs/`.
- Selected verdict: `var/research/lighter-optimization-r6-hourly/verdict.json`.
- Independent fill audit: `var/research/lighter-history-audit/r6-best-fill-audit.json`.
- Final register: `var/research/lighter-optimization-r6-hourly/register.json`.
- Selected raw replay: `var/research/lighter-optimization-r6-hourly/replay/cohort-external-hourly-central/fold-1.json`.
- Subperiods and native 01:00 benchmarks: `var/research/lighter-optimization-r6-hourly/evaluation/summary.json`.
- Paired bootstrap: `var/research/lighter-optimization-r6-hourly/paired-original.json`.
- First-fit models: `var/research/lighter-ranker-r1-2026-09-08/`.
- Reproducibility manifests: `var/research/lighter-ranker-r1-2026-09-08-final/`.
  Corrected checksum paths verify there. Predictive payloads match the original
  fits exactly after excluding `trained_at`; the models used in replay remain
  immutable.
- Decision trail: `.audit/lighter-optimization.tsv`.

Twelve policy candidates were measured or attempted before the measured-hour comparison,
including three failed guard implementations. R6 changes execution assumptions
for three already-declared policies; it does not fit new models or tune their
parameters. Prior broad-market and static-membership experiments remain
separate and are not presented as Lighter performance observations.
