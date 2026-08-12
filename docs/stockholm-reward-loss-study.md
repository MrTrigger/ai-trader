# Stockholm Main Market reward/loss study

> **Result:** FAILED / NOT PROMOTABLE  
> **Last updated:** 2026-08-12  
> **Required Sharpe:** 2.0 after costs  
> **Data status:** `SURVIVORSHIP_CONTAMINATED`

This study corrects two defects in the first exploratory record:

1. the universe is Nasdaq Stockholm Main Market Large, Mid, and Small Cap only;
   First North is excluded by the Rust matrix builder;
2. reward and loss selection follows a reproducible purged expanding
   walk-forward protocol instead of assuming raw-return/L1 is correct.

The earlier [`stockholm-first-backtest.md`](stockholm-first-backtest.md) mixed
First North into the universe and is superseded. Its numbers are retained only
as historical provenance.

## Matrix and model contract

The Rust matrix manifest declares
`NASDAQ_STOCKHOLM_MAIN_LARGE_MID_SMALL`. A Rust test builds a mixed Main
Market/First North fixture and requires every emitted row to be Main Market.

- 531,885 final rows;
- 40 Rust-owned baseline inputs: 20 size-bucket ranks and 20 missing flags;
- adjusted next-session open to adjusted open 20 sessions later;
- one total unit of sample weight per decision date;
- two-sided 0.5% fitting-time target clipping, calculated from each training
  prefix only;
- shallow 250-tree LightGBM, one deterministic seed for the model-family
  comparison.

Two direction-preserving rewards were compared:

- `absolute_return`: signed forward return in return units;
- `return_per_risk`: signed forward return divided by trailing 60-session
  volatility. Rust multiplies the model score by the row's trailing volatility
  exactly once before applying return-unit cost thresholds.

The crypto bot's within-date demeaning/rank reward was not copied literally.
That transform always creates relative winners and losers and removes the
common market direction, which would conflict with the Stockholm bot's
no-neutrality requirement. L2, L1, and Huber losses were tested for both
direction-preserving rewards.

## Walk-forward protocol

`training/walk_forward_stockholm.py` derives boundaries from the actual matrix
sessions. Every fold retrains on an expanding prefix and purges 20 complete
decision sessions. A training label's exit is strictly before the first test
entry. Fold starts stay on one global 20-session rebalance grid.

Development folds were frozen before the six candidates ran:

| fold | trained through | test interval |
|---:|---|---|
| 1 | 2022-08-03 | 2022-09-01–2023-05-17 |
| 2 | 2023-04-18 | 2023-05-19–2024-02-01 |
| 3 | 2024-01-04 | 2024-02-02–2024-10-18 |
| 4 | 2024-09-20 | 2024-10-21–2025-09-30 |

The candidate-specific final block trained through 2025-09-02 and tested
2025-10-01–2026-07-08. It is only a **pseudo-holdout**: its candidate result was
not used in reward/loss selection, but earlier exploratory work had already
exposed this period and selected the 20-session horizon. It cannot provide
fresh capital-authorizing evidence.

## Development comparison

All results include the declared base costs. Stress doubles spread/impact and
lets the same cost-aware constructor reject trades.

| reward | loss | base Sharpe | stress Sharpe | base return | stress return | positive folds | mean rank IC |
|---|---|---:|---:|---:|---:|---:|---:|
| absolute return | L2 | **1.07** | **0.95** | +97.0% | +81.4% | 4/4 | +0.0381 |
| absolute return | Huber | 1.07 | 0.95 | +97.0% | +81.4% | 4/4 | +0.0381 |
| absolute return | L1 | 0.49 | 0.31 | +23.9% | +12.9% | 3/4 | +0.0557 |
| return per risk | L2 | 0.26 | 0.16 | +11.1% | +1.8% | 2/4 | +0.0157 |
| return per risk | L1 | 0.47 | 0.28 | +21.6% | +10.6% | 3/4 | +0.0341 |
| return per risk | Huber | 0.44 | 0.34 | +30.1% | +19.4% | 4/4 | +0.0013 |

Absolute-return/L2 was selected before opening the final block. Huber produced
different tree artifacts but the same selected portfolios and economic results;
L2 is the simpler representative. The crypto-like return-per-risk reward did
not transfer successfully to this equity problem.

## Candidate-specific final block

The selected candidate failed immediately outside the selection interval:

| measure | base | doubled spread/impact | OMXSGI |
|---|---:|---:|---:|
| return | **-10.8%** | **-12.6%** | +14.0% |
| Sharpe | **-0.37** | **-0.45** | 1.25 |
| max drawdown | -24.2% | -25.0% | — |
| rank IC | -0.0175 | -0.0175 | — |

This is a clean rejection of the development improvement. The model lost both
ranking skill and money while the broad Stockholm gross index rose.

For context only, stitching the four development folds and failed final block
produces +75.7%, Sharpe 0.71, and -26.6% maximum drawdown under base costs,
versus OMXSGI +70.4% and Sharpe 0.98. Under doubled spread/impact it produces
+58.6% and Sharpe 0.60. This stitched number is not a selection estimate because
the model family was chosen on the first four folds.

## Direction and calibration diagnostics

The selected model was effectively a long-only market exposure despite having
no direction quota:

- mean net exposure: +99.0%;
- combined beta to OMXSGI: 1.10;
- summed long contribution: +72.9 percentage points;
- summed short contribution: -4.7 percentage points;
- mean OOS rank IC: +0.0268 after including the failed block;
- directional accuracy: 51.6%;
- bottom score bucket realised -0.08%, while the top realised +1.43%.

The absence of a 50/50 constraint worked as intended; the portfolio was free to
choose all longs. The evidence says the model did so because it mostly learned
broad positive equity drift, not because it learned a durable signal capable of
capitalising on both market directions. Forcing shorts would not repair that.

## Fixed market-direction overlay ablation

After closing the reward/loss study, one predeclared, untuned direction baseline
was replayed over the same four development folds. This is a diagnostic
ablation, not fresh selection evidence. Rust calculated five causal OMXSGI
votes—EOD versus MA50, MA50 versus MA200, and 63/126/252-session returns—plus
20-session annualized volatility. A shared state machine applied hysteresis,
an 18% volatility ceiling, five-percentage-point daily exposure ramps, and
these symmetric maximum budgets:

| state | maximum gross | target net |
|---|---:|---:|
| strong up/down | 100% | +/-50% |
| up/down | 70% | +/-35% |
| neutral | 30% | 0% |

Unused long or short budget remained cash. The state was warmed on OMX history
before every fold rather than reset at the test boundary. Results:

| measure | original base | overlay base | overlay 2x costs |
|---|---:|---:|---:|
| return | +97.0% | +21.6% | +16.6% |
| Sharpe | 1.07 | 0.63 | 0.51 |
| max drawdown | -14.1% | -8.0% | -8.3% |
| mean gross | 100.0% | 43.0% | 43.0% |
| mean net | +99.2% | +42.3% | +42.3% |

The standalone direction sleeve, measured optimistically before execution
costs, returned **-4.8%**, Sharpe **-0.29**, with an -8.8% maximum drawdown. It
was positive in the third fold but negative in folds one, two, and four. The
combined overlay lowered drawdown by cutting exposure, but destroyed return and
did not improve risk-adjusted performance. It is therefore implemented only as
an explicitly enabled research baseline and is rejected for paper/live use.

The result should not trigger a threshold search over these closed folds. A
different direction model needs new history/information and a predeclared
comparison; simply tuning moving-average windows or regime cutoffs here would
be another form of test-set fitting.

## Decomposed residual-selection challengers

The subsequent implementation separated the two prediction problems instead
of asking one absolute-return model to learn equity drift and stock selection
at once. Rust now emits, for every decision date, the equal-weight eligible
market return and each stock's residual return. A residual model cannot replay
without an explicit direction layer. Portfolio gross/net budgets still use
`long=(G+N)/2` and `short=(G-N)/2` as maxima; neither sleeve is a quota.

Three fixed model-capacity checks used the same closed development folds:

| residual selector | base return | base Sharpe | 2x-cost return | 2x-cost Sharpe | rank IC |
|---|---:|---:|---:|---:|---:|
| LightGBM L2 | +7.6% | 0.27 | +1.8% | 0.11 | +0.0443 |
| weighted ridge, lambda 25 | +2.4% | 0.13 | -2.3% | -0.05 | +0.0372 |
| fixed 50/50 tree/ridge | +5.2% | 0.20 | -0.9% | 0.03 | +0.0448 |

The families failed in different folds. The tree achieved Sharpe 2.23 in fold
2 and the fixed blend achieved 2.20 in fold 3, but all aggregate results failed
and each had a severe negative fold. Positive mean IC did not produce stable
tail portfolios: in the final development fold the highest predicted decile
realized less residual return than several middle deciles.

The direction state audit also found an implementation defect: after a
confirmed reversal, the newly reported up/down regime could retain net
exposure in the old direction while the ramp crossed zero. Neutral could retain
stale net exposure too. The shared `portfolio-construction` state now drops the
invalid side to zero immediately and ramps only new exposure. Replaying the
tree challenger after this correctness fix improved base return to **+9.7%**
and Sharpe to **0.34** (2x costs: +3.8%, Sharpe 0.17), still a decisive reject.

## Residual-risk v3 pseudo-holdout

One candidate-specific feature challenger implemented the already documented
residual-risk inputs entirely in Rust: 252-session market beta, 60-session
market-plus-sector residual volatility, 20/60-session liquidity, 60-session
Amihud impact, multi-session range/close location, and 21/126-session market
and sector residual momentum. Its 531,885-row matrix has 62 finalized inputs;
prefix tests prove earlier features cannot change when future bars are added.

The 2025-10-01–2026-07-08 interval was opened only after the v3 contract and
three purged expanding folds were fixed. It is nevertheless a **pseudo-holdout,
not fresh evidence**, because the earlier absolute-return study had already
exposed the interval and selected the 20-session horizon. The unchanged
40-input residual model was replayed as the declared control:

| model | return | Sharpe | 2x-cost return | 2x-cost Sharpe | rank IC | positive folds |
|---|---:|---:|---:|---:|---:|---:|
| 40-input residual control | -8.6% | -0.75 | -10.7% | -0.94 | -0.0197 | 1/3 |
| 62-input residual-risk v3 | -9.9% | -0.54 | -11.7% | -0.66 | -0.0186 | 1/3 |
| OMXSGI | +14.0% | 1.25 | — | — | — | — |

Both selectors' ordering reversed sign in the first two blocks and recovered
only in spring 2026. V3 is rejected; the result also confirms that the baseline
price/volume signal is not currently promotable. Fold reports now include both
model family and feature-set version so the summarizer cannot silently stitch
unlike model contracts.

## FI public-short v4 and PDMR v5

Two official FI datasets were then archived through the shared `equity-data`
crate and converted to strictly causal Rust features. The net-short register
contains 38,190 holder events across 421 ISINs. It is a disclosed short-demand
proxy, not historical borrow availability. Its v4 pseudo-holdout lost 7.5%,
Sharpe -0.91 (2x costs: -9.5%, Sharpe -1.17), with rank IC -0.0078 and only
one positive fold. Public-short percentage became the most-used tree input even
though its out-of-sample relationship was negative, so v4 was rejected.

The FI PDMR archive is materially broader: 165,140 published rows from
2016-07-04 through 2026-08-10, 5,847 ISINs, and 82,633 predeclared qualifying
initial share acquisitions/disposals. Of the 412 current Main Market ISINs,
397 have PDMR history and 388 have a qualifying event. The shared collector
recursively splits dense publication intervals before FI's 1,000-row export
ceiling and never admits capped parent exports.

Feature set v5 adds 30/90-day signed transaction value, 90-day gross buy/sell
value, transaction and unique-buyer counts, and acquisition recency to v3. A
filing enters only after publication date; transaction date and today's
revised/cancelled status never move availability backwards. The resulting
matrix has 531,885 rows and 76 finalized Rust inputs.

On the unchanged four development folds, v5 improves the residual control but
still fails every promotion threshold:

| model | return | Sharpe | max drawdown | 2x-cost return | 2x-cost Sharpe | rank IC | positive folds |
|---|---:|---:|---:|---:|---:|---:|---:|
| residual control | +9.7% | 0.34 | -13.7% | +3.8% | 0.17 | +0.0443 | 2/4 |
| PDMR v5 | +15.9% | 0.47 | -10.0% | +10.0% | 0.33 | +0.0483 | 3/4 |
| OMXSGI | +49.4% | 0.92 | -12.8% | — | — | — | — |

Every v5 fold uses the PDMR columns heavily and consistently, led by 90-day
net value, gross sales/purchases, and unique buyers. The gain is therefore
incremental PDMR information, not an unused-feature artifact. Nevertheless,
fold four loses 8.8% with Sharpe -1.52, so the selector remains regime-unstable.

The already-exposed 2025-10-01–2026-07-08 diagnostic improves over v3/v4 but
does not validate v5: return -2.3%, Sharpe -0.10, max drawdown -9.7%, two of
three folds positive, and rank IC -0.0264 (2x costs: -4.3%, Sharpe -0.27).
OMXSGI returns +14.0%, Sharpe 1.25. V5 is retained as a research input and
rejected as a paper/live model.

For a genuinely separate direction study, official Nasdaq histories are now
archived from 2008-11-17 for OMXSGI/OMXS30GI/OMXSBGI and from 2011-06-30 for
eight current top-level Stockholm gross-return sector indexes. This adds new
pre-2016 direction/regime information; it does not reopen the closed stock
selection folds or justify tuning the rejected moving-average thresholds.

The corresponding fixed-capacity trained direction model also failed. Across
149 non-overlapping out-of-sample periods it returned 5.0%, Sharpe 0.11, and
maximum drawdown -13.6%. The fixed trend control returned 8.2%, Sharpe 0.13,
while long-only OMXSGI returned 243.8%, Sharpe 0.67. The learned direction
model is rejected; it is not used to rescue a stock selector.

## Report-event, ESEF, and macro challengers

Three further one-shot feature contracts used genuinely new public inputs,
kept the v5 relative-return/L2 fit and fixed direction constructor, and used
the same closed development folds only as an ablation surface:

| feature contract | base return | base Sharpe | max drawdown | rank IC | positive folds | later diagnostic |
|---|---:|---:|---:|---:|---:|---|
| PDMR + Nasdaq reports v6 | +26.1% | 0.69 | -13.9% | +0.0378 | 3/4 | -7.7%, Sharpe -0.69 |
| annual ESEF fundamentals v7, relative | +17.9% | 0.46 | -15.9% | +0.0351 | 3/4 | not opened |
| annual ESEF fundamentals v7, absolute diagnostic | +84.1% | 1.40 | -12.2% | +0.0465 | 4/4 | -11.8%, Sharpe -0.66 |
| PDMR + Riksbank FX/KIX v8 | +10.7% | 0.35 | -12.7% | +0.0462 | 3/4 | not opened |

V6 uses 31,617 official financial-report announcements. It failed the already
exposed later block with negative IC and return. V7 uses 1,400 ESEF filings,
417 entities, and 243,517 standardized IFRS facts with conservative
availability dates. Its attractive absolute development result was mostly
long equity exposure and reversed in the later diagnostic, so it is not a
candidate. V8 adds stock-specific sensitivities to official USD/SEK, EUR/SEK,
and KIX history; the new fields were used by every fitted tree but degraded
the v5 control. No variants were tuned and the unopened diagnostics remain
closed.

## Historical-universe and observed-liquidity acquisition

The shared data crate now contains two independent official event archives for
reconstructing membership. Skatteverket contributed 4,477 listing-history
rows for 1,648 companies, including 955 conservatively classified Main Market
rows and 150 delisting rows. Nasdaq contributed 1,071 Stockholm equity notices
from 2,935 official metadata rows; 1,055 notices carry a checksummed ISIN, and
189 of 190 delisting notices carry one. These records are not admitted to the
matrix until ordinary-share classification, effective identifier intervals,
terminal outcomes, and inactive price coverage are all validated.

Nasdaq's free Nordic instrument endpoint was tested directly with the official
order-book IDs for Swedish Match, Acando, and OX2. It returns `Instrument not
found` after delisting, so it is not the inactive-price solution. For current
shares it does expose approximately ten years of official daily OHLC, closing
bid/ask, turnover, and trade count. That archive is being treated as
survivor-contaminated observed-liquidity data, not as a replacement for a
licensed active-and-inactive security history.

The full resumable archive contains 878,723 valid sessions for 411 of 412
current/prior-snapshot Main Market lines, including 864,294 sessions with a
two-sided closing quote. The sole failed line is a new listing with only 25
sessions, below the collector's 30-session validity floor. During collection,
Nilörngruppen disappeared from the current screener after its 2026-08-10 final
session. Supplying the earlier universe snapshot preserved its still-resolvable
`TX1809018` history through the SEK 75.40 terminal close. Old delistings such as
Swedish Match, Acando, and OX2 remain unavailable, demonstrating both the value
and the limited grace period of prospective snapshots.

On 2026-08-12 those three official delisted ISINs also failed IB contract
resolution with error 200 before any history request, while Yahoo returned 404
for their former tickers. Neither source repairs the inactive-price gap.

One predeclared v9 contract added seven trailing microstructure values to v5:
one-day and 20-day spread, spread ratio, trade-count level and surge,
close-versus-average price, and typical trade value. Missing flags make 14 new
inputs and 90 finalized Rust inputs total. The feature and model contracts were
frozen before the unchanged relative-return/L2 walk-forward ran:

| model | return | Sharpe | max drawdown | 2x-cost return | 2x-cost Sharpe | rank IC | positive folds |
|---|---:|---:|---:|---:|---:|---:|---:|
| PDMR v5 | +15.9% | 0.47 | -10.0% | +10.0% | 0.33 | +0.0483 | 3/4 |
| PDMR + Nasdaq microstructure v9 | **+26.9%** | **0.76** | **-9.7%** | **+20.5%** | **0.61** | +0.0455 | 3/4 |
| OMXSGI | +49.4% | 0.92 | -12.8% | — | — | — | — |

The new data genuinely affected the fit: median spread, median trade count,
and median trade value received respectively 92–136, 111–128, and 43–71 tree
splits in every fold. Fold three achieved Sharpe 2.55, but fold four lost 7.7%
with Sharpe -1.26 and almost zero rank IC. Aggregate Sharpe remains far below
2.0 and below OMXSGI. V9 is rejected, its later diagnostic remains unopened,
and no variants are tuned on the closed folds.

## Predeclared borrow-cost and reward v10 study

Before the full fee archive or any v10 model result was available, the next
study was fixed as follows. IB historical `FEE_RATE` is collected for the full
current/prior-snapshot Main Market universe through the shared `ib` data source
with the documented historical-request pacing. It is a decimal annual borrow
rate, never a locate or historical quantity claim. Rust adds current fee,
20-session median and maximum, and 5/20-session changes. During replay an
observed fee replaces the fixed short holding-cost fallback using
`annual_rate * holding_sessions / 252`; the separate availability penalty
remains because fee history does not prove lendable quantity.

Three fitting responses are declared before evaluation, all on the identical
v10 Rust matrix, fixed LightGBM capacity, direction schedule, costs, and purged
expanding folds:

1. the existing relative-return/L2 response as the control;
2. relative-return-per-risk/L2, which transfers the crypto bot's validated
   reward lesson to the decomposed selection problem: Rust divides relative
   return by decision-time volatility and centers it within the decision date,
   then inference multiplies by that same volatility exactly once before
   return-unit cost gates;
3. a robust cross-sectional rank/L2 response, now semantically appropriate for
   the decomposed stock-selection layer.

Rust owns every response. In particular, Python no longer calculates the older
absolute return-per-risk response either. Rust computes average forward-return
rank within each decision-date universe
and maps it to `[-1, 1]`. Python does not rank or transform it. The trainer fits
one positive training-prefix slope from rank units to relative-return units;
Rust applies that scale before cost gates. Thus rank fitting changes ordering
robustness without turning maximum sleeves into quotas or admitting every name
as a fake high-confidence trade. The return and per-risk arms retain the fixed
training-prefix 0.5% two-sided clipping; rank-label clipping is disabled. These closed
folds remain a diagnostic surface and cannot make the survivor-contaminated
dataset promotable regardless of the resulting Sharpe.

The completed IB archive contains 406 fee-rate series and 749,715 daily
observations from 2016-08-12 through 2026-08-12. Four current contracts did not
resolve and two resolved without fee history. Among the latest observations,
the annual fee is 1.75% at the median, 27.2% at the 90th percentile, and
270.76% at the maximum. The v10 matrix has 531,885 rows and 100 finalized Rust
inputs; 405 histories map to its current/prior-snapshot universe.

The three frozen v10 responses all fail:

| response | return | Sharpe | max drawdown | 2x-cost return | 2x-cost Sharpe | rank IC | positive folds |
|---|---:|---:|---:|---:|---:|---:|---:|
| relative return | **+28.9%** | **0.85** | -9.7% | +22.3% | 0.69 | +0.0433 | 3/4 |
| centered relative return per risk | +23.8% | 0.61 | -12.3% | +17.2% | 0.47 | +0.0434 | 2/4 |
| cross-sectional rank | +7.1% | 0.32 | -11.0% | +1.8% | 0.11 | **+0.0570** | 2/4 |
| OMXSGI | +49.4% | 0.92 | -12.8% | — | — | — | — |

Observed borrow fees affect the fit and replay rather than merely decorating
the report: the final control model uses current and median fee inputs, and
92.1–94.9% of matrix rows plus 89.9–95.0% of selected short-periods have an
observed rate across folds. Nevertheless, all three responses fail the 2.0
target. The rank response improves ordering IC while making less money, and
the correctly decomposed crypto-style risk response is worse than the control.
The last fold remains the common failure: relative-return selection loses 8.1%
at Sharpe -1.37, while centered relative-return-per-risk loses 12.3% at Sharpe
-2.03. V10 is rejected and the later diagnostic interval remains unopened.

## Predeclared complete issuer-news v11 study

After closing v10, one genuinely new public-data contract was frozen before a
v11 matrix or model result was generated. The shared `equity-data` provider
archived the complete official Nasdaq Main Market Stockholm company-news feed:
141,032 announcements, 718 issuer names, and 32 original categories from
2016-01-01 through 2026-08-12. Coverage is roughly 9,700–16,200 events per
complete calendar year. The collector preserves disclosure ID, provider
category, headline, publication timestamp, venue, and issuer; it recursively
splits date ranges at Nasdaq's 10,000-result ceiling and enforces the requested
venue locally.

Feature set `fs-rust-stockholm-11` adds exactly 13 raw values to v10 (and 13
missing flags):

- all-company-news counts over 30 and 90 calendar days;
- inside-information recency, 30/90-day counts, and causal 1/5/21-session
  reaction to the latest disclosure;
- 90-day counts for own-share changes, board/management/auditor changes,
  prospectuses, major-shareholder announcements, and tender offers.

These groups use only Nasdaq's declared categories. No headline keywords,
sentiment model, or category subset was chosen from forward returns. Same-day
events are unavailable to the decision; reaction features use prices only
through the decision date; and Rust prefix tests require future announcements
and prices not to change an earlier row.

Exactly one model arm is declared: relative-return/L2 LightGBM with the same
capacity, 0.5% training-prefix clipping, costs, 20-session cadence, fixed
direction schedule, four purged expanding folds, and OMXSGI comparison as the
v10 control. The hypothesis is that timestamped price-sensitive disclosures
and capital/ownership events stabilize residual tail selection, especially in
the failed final development regime. There is no constructor or threshold
search. Failure of the one arm rejects v11; success on these already-used folds
would still be research evidence only because the universe is survivorship
contaminated.

The frozen v11 arm returned +33.8%, Sharpe 0.95, and -13.7% maximum drawdown
at the original uniform 35 bps cost assumption (2x costs: +26.1%, Sharpe
0.77). It remained below OMXSGI's +49.4%, Sharpe 0.92 and its fourth fold
lost 13.6% at Sharpe -2.44, so v11 was rejected.

An execution-model audit then carried each row's causal 20-session median
Nasdaq closing bid/ask spread outside the model feature map. The corrected
base cost is IB's 10 bps percentage commission round trip plus measured spread
and a 5 bps impact floor; missing spreads use 20 bps. Stress doubles only
spread and impact, not commission. The same four frozen v11 models and rows
were replayed without refitting. Base return fell to +7.1%, Sharpe 0.32, and
-16.2% drawdown; stress returned -2.2%, Sharpe -0.08. All selected fold-four
positions had observed spread. The earlier fold-two gain had depended heavily
on a small number of Small Cap trades that the measured cost gate rejects.

## Predeclared official report-text v12 study

Before generating a v12 matrix or model result, the shared `equity-data`
provider archived the official HTML body for 31,611 of 31,617 Nasdaq Stockholm
financial-report announcements from 2016 onward, plus 43,053 attachment links;
six attachment-only/empty-body disclosures remain explicit failures. The
archive is resumable by disclosure ID and preserves the 31,611 raw issuer
pages, publication metadata, language, normalized body text, and attachment
metadata. PDFs are not interpreted.

Feature set `fs-rust-stockholm-12` adds exactly eight raw values to v11 (and
eight missing flags): days since the latest report body, then exact-label
bilingual current/comparative changes for sales, order intake, EBIT, operating
margin level and change, EPS, and dividend. The parser accepts only declared
English/Swedish accounting labels followed by an explicit current/prior
numeric pair. It does not infer values from arbitrary numbers, sentiment, or
future price reactions. Translation selection maximizes field coverage and
then prefers English on a tie; same-timestamp translations are one event, and
same-day text is unavailable to the decision. Rust tests own these rules.

Exactly one exploratory arm is frozen: relative-return/L2 LightGBM, one seed,
0.5% training-prefix clipping, 20-session horizon/cadence, the unchanged fixed
direction schedule, the same four purged expanding folds, and the corrected
measured-spread base/stress costs. No parser label, model capacity, constructor,
or threshold search is permitted after seeing the result. These folds are
already exposed, so even a pass is diagnostic evidence only; it cannot
authorize capital without a new forward interval and survivorship-safe data.

V12 was stopped at its first independent fold because the predeclared
stability gate had already failed: -5.4%, Sharpe -0.85, and -9.5% drawdown
versus OMXSGI +18.6%, Sharpe 1.18. The model did use the new fields (report
recency 94 splits, EPS growth 83, EBIT growth 60, order-intake growth 15,
sales growth 7), so this is not an integration failure. The short sleeve lost
5.6 percentage points gross and Large Cap positions lost 6.9 points gross;
all 180 selected position-periods had observed spread. Remaining fits were
not run because no aggregate can repair the already-failed all-fold stability
requirement without turning the exercise into closed-fold selection.

## Verdict

No candidate approaches the required 2.0 aggregate Sharpe, the development
winner fails its candidate-specific final block, every residual challenger
fails, and OMXSGI has the higher pseudo-holdout Sharpe. The public dataset also
omits delisted securities and historical borrow quantity. No model from this
study may enter paper execution as an edge candidate.

The shared provider now has a licensed-data path for the first omission:
`collect-eodhd-delisted` reads EODHD exchange `ST` inactive common stocks and
admits only checksum-valid ISINs present in the official Nasdaq delisting
notices. The API token is request-only and never serialized. This collector is
implemented and tested but not run because no `EODHD_API_TOKEN` is currently
configured. Its output will still require point-in-time segment classification
and terminal-outcome treatment before it can remove the survivorship warning.

The observed 2016–2026 folds are closed for reward, loss, feature, ensemble, and
constructor tuning. New feature contracts are run once and rejected unless
they clear all gates; their results do not authorize subsequent threshold
searches. A new controlled study requires genuinely new information:
point-in-time Main Market membership and delistings, point-in-time fundamentals
and revisions, observed spreads, and historical borrow availability or a
declared availability scenario. The decomposed market-direction plus residual
ranker has now been implemented and rejected on the available information; its
next verdict requires genuinely new data or new elapsed time.
