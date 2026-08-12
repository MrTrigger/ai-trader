# Stockholm portfolio — first exploratory model/backtest

> **SUPERSEDED:** This historical record included First North, contrary to the
> final Large/Mid/Small Main Market scope. Its results must not be used for
> model selection or capital decisions. See the corrected
> [`stockholm-reward-loss-study.md`](stockholm-reward-loss-study.md).

> **Result:** research signal only; **FAILED / NOT PROMOTABLE**  
> **Required Sharpe:** at least 2.0 after costs  
> **Run date:** 2026-08-10  
> **Data status:** `SURVIVORSHIP_CONTAMINATED`

This is the first end-to-end execution of the Rust-owned Stockholm feature,
label, matrix, and replay path. It answers whether the plumbing can train and
score a broad current Stockholm universe and whether a price/volume ranker is
worth further investigation. It does not establish a tradeable edge.

## Data and coverage

- Current universe: Nasdaq Nordic screener, Stockholm market, explicit Large
  Cap, Mid Cap, Small Cap, First North Premier, and First North GM segments.
- Adjusted research history: Yahoo Finance daily OHLCV and adjusted close,
  2016-08-08 through 2026-08-07.
- 745 current SEK share lines discovered; 744 histories accepted.
- 1,503,440 valid daily bars after rejecting internally inconsistent OHLC rows.
- One omitted history: `ELLOS` (`TX7384398`), only 22 observations.
- Segment counts discovered: 162 Large, 141 Mid, 109 Small, 60 First North
  Premier, and 273 First North.
- Final eligibility: at least 252 sessions of history and trailing 20-session
  median traded notional of at least SEK 1,000,000.
- Final Rust matrix: 607,288 rows, 40 inputs (20 ranked values and 20 explicit
  missing flags), five-session adjusted-next-open label.

Current 2026 constituents and their current segment/sector are projected
backward. Delisted securities are absent. This is survivorship and
classification leakage, is stamped into every matrix/model/report, and makes
the result ineligible for capital regardless of performance.

## Model and folds

Python only assembled the Rust-final matrix in manifest order, fit shallow
LightGBM trees, and exported their JSON tree dump. It did not calculate,
normalize, align, clip, impute, select, or rename model inputs or labels.

- objective: L1 regression of signed five-session absolute return;
- 250 trees, learning rate 0.025, 15 leaves, max depth 5;
- minimum 250 rows per leaf, L2 25, deterministic seeds;
- each decision date has total training weight one;
- five expanding fits, with more than a five-session embargo before each test;
- first fit uses roughly five years before its 2022-09-01 test start;
- scored periods never occur on or before the artifact's `trained_through`.

Test blocks:

| fold | test window | base return | base Sharpe | base max DD |
|---:|---|---:|---:|---:|
| 1 | 2022-09-01–2023-06-30 | +0.4% | 0.11 | -12.0% |
| 2 | 2023-07-01–2024-03-31 | +20.1% | 1.28 | -10.6% |
| 3 | 2024-04-01–2024-12-31 | +6.5% | 0.62 | -12.2% |
| 4 | 2025-01-01–2025-09-30 | -6.4% | -0.33 | -10.1% |
| 5 | 2025-10-01–2026-07-30 | +9.0% | 0.64 | -20.3% |

## Portfolio and costs

At each five-session rebalance every instrument competes in its predicted best
direction. Long and short edges are compared after direction-specific costs;
there is no direction quota, net target, or neutrality penalty. The book holds
at most 20 positions at 5% each, so gross exposure is at most 100% and unused
capacity remains cash.

Base candidate hurdle:

- 35 bps Main Market round-trip commission/spread/impact floor;
- another 35 bps on First North;
- shorts add 10 bps borrow per five-session holding period and 25 bps when
  short exposure increases, as a historical-availability uncertainty penalty;
- all candidates add a 10 bps safety margin.

Replay charges one-way transaction cost only on signed weight changes, borrow
on held shorts, availability penalty only on increased shorts, and one final
liquidation. It does not repeatedly charge a full round trip to an unchanged
position. The commission component assumes orders large enough that IB's 0.05%
tier, rather than its SEK 10 per-order minimum, binds.

The stress run doubles the round-trip spread/impact components and lets the
cost-aware constructor reject trades that no longer clear the hurdle. It is a
stressed policy replay, not merely the base trades with an accounting surcharge.

## Five-session results

| measure | base | doubled spread/impact |
|---|---:|---:|
| stitched return | +31.1% | +3.2% |
| Sharpe | 0.47 | 0.13 |
| max drawdown | -20.3% | -20.2% |
| positive folds | 4 / 5 | 2 / 5 |
| mean gross | 84.7% | 16.4% |
| mean net | +0.03% | -15.8% |
| long sleeve P&L contributions, summed | +21.8pp | +0.6pp |
| short sleeve P&L contributions, summed | +12.0pp | +5.0pp |
| modeled cost contributions, summed | -61.0pp | -26.7pp |

The near-zero aggregate base net exposure is accidental: fold means range from
+39.3% net long to -22.0% net short. The constructor did not request
neutrality. Under doubled costs it selected much less gross exposure and was
mostly short because few long forecasts cleared the larger hurdle.

## Predeclared holding-period challengers

The same Rust feature catalogue, eligibility rule, model family, five expanding
test blocks, and direction-unconstrained constructor were independently trained
for 10- and 20-session labels. Borrow cost scales with the holding period. This
was the cadence comparison declared before the first fit, not an unrestricted
hyperparameter search.

| holding period | base return | base Sharpe | positive folds | stressed return | stressed Sharpe | stressed positive folds |
|---:|---:|---:|---:|---:|---:|---:|
| 5 sessions | +31.1% | 0.47 | 4 / 5 | +3.2% | 0.13 | 2 / 5 |
| 10 sessions | +73.4% | 0.86 | 3 / 5 | +42.6% | 0.60 | 3 / 5 |
| 20 sessions | +91.9% | 0.99 | 5 / 5 | +59.4% | 0.74 | 4 / 5 |

Twenty sessions is the best and most stable member of this family, but its
Sharpe remains less than half the required 2.0 under stress and slightly less
than half under base costs. Fold 2 alone exceeds 2.0; adjacent folds do not, so
that episode is regime concentration rather than evidence of a stable 2.0
strategy. The broad improvement as turnover falls is useful design evidence,
but is not a capital gate pass.

## Broad Stockholm benchmark

The benchmark is **OMXSGI**, not OMXS30. Nasdaq defines OMXSGI as the OMX
Stockholm All-Share Gross Index: it contains all shares listed on Nasdaq
Stockholm and reinvests dividends. That gross-return treatment is the correct
counterpart to the adjusted stock labels. OMXSPI is retained as the
price-return reference name but is not used for performance comparison because
it omits dividends. Source: [Nasdaq OMXSGI overview](https://indexes.nasdaq.com/Index/Overview/OMXSGI)
and [official history](https://indexes.nasdaq.com/Index/History/OMXSGI).
First North is an alternative market and has a separate [First North Sweden
gross index, FNSESEKGI](https://indexes.nasdaq.com/Index/Overview/FNSESEKGI), so
OMXSGI is the broad Main Market reference rather than an exact replica of this
strategy's combined Main Market plus First North opportunity set.

The shared `equity-data` crate collects official Nasdaq start-of-day and
end-of-day levels. Rust aligns each portfolio decision to OMXSGI from the next
exchange session's start-of-day level through the same 5-, 10-, or 20-session
horizon as the stock label. Benchmarking neither changes the selected trades
nor imposes a beta or net-exposure target.

| holding period | portfolio base return | OMXSGI return | excess total return | portfolio Sharpe | OMXSGI Sharpe | information ratio | beta |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 5 sessions | +31.1% | +76.5% | -45.4pp | 0.47 | 0.99 | -0.30 | 0.08 |
| 10 sessions | +73.4% | +72.8% | +0.6pp | 0.86 | 0.97 | 0.03 | 0.19 |
| 20 sessions | +91.9% | +75.1% | +16.8pp | 0.99 | 0.99 | 0.14 | 0.40 |

The 20-session base case beats the broad gross index in total return, but the
low information ratio says the active return is weak relative to its tracking
risk. Under doubled spread/impact it returns +59.4%, trails OMXSGI by 15.7
percentage points, and has Sharpe 0.74. Benchmarking therefore strengthens the
failure verdict rather than rescuing the candidate.

## Frozen market/sector challenger

After the baseline folds were frozen, one predeclared Rust-only feature-group
challenger was run at the best 20-session cadence. It retained the exact
602,660 eligible rows, folds, training cutoffs, LightGBM parameters, portfolio
rules, and costs. The feature contract increased from 40 to 74 model inputs:

- sector-relative 5/21/63/126/252-session trend, volatility, illiquidity, and
  volume-surge ranks, calculated only from the decision-date cross-section;
- decision-date market median trend, dispersion, breadth, and volatility;
- an explicit missing flag for every value.

No result-dependent threshold or tree setting was changed.

| measure | original 20-session base | context v2 base | context v2 stress |
|---|---:|---:|---:|
| stitched return | +91.9% | +10.1% | -1.6% |
| Sharpe | 0.99 | 0.22 | 0.11 |
| max drawdown | -15.5% | -41.5% | -45.0% |
| positive folds | 5 / 5 | 2 / 5 | 2 / 5 |
| OMXSGI return | +75.1% | +75.1% | +75.1% |
| excess total return | +16.8pp | -65.0pp | -76.7pp |
| information ratio | 0.14 | -0.33 | -0.44 |

The context group is rejected. It improved the first fold but failed in later
regimes, which is consistent with unstable/overfit interaction effects rather
than a durable addition. Its code and reports remain versioned as a negative
experiment; it is not a promotion candidate.

## Frozen construction challengers

The original v1 forecasts were then frozen while the missing construction
layer was implemented in the strategy-neutral Rust `portfolio-construction`
crate. The crate preserves every accepted candidate's direction and applies no
long/short count, gross, or net quota. Two policies were declared before their
fold reports were opened:

1. take the top 24 by net edge and size capped weights in proportion to net
   edge divided by trailing volatility;
2. rank all candidates by net edge divided by trailing volatility, take the
   top 20, and retain the original equal 5% weights.

Both keep the 100% gross and 5% single-name caps. Forecasts, model fits, costs,
folds, and OMXSGI alignment are unchanged.

| policy | base return | base Sharpe | stress return | stress Sharpe | base excess vs OMXSGI |
|---|---:|---:|---:|---:|---:|
| original edge rank / equal 20 | +91.9% | 0.99 | +59.4% | 0.74 | +16.8pp |
| edge rank / edge-volatility top 24 | +54.9% | 0.79 | +30.9% | 0.52 | -20.2pp |
| edge-volatility rank / equal 20 | +53.4% | 0.86 | +20.4% | 0.39 | -21.7pp |

Both construction challengers are rejected. Risk adjustment reduced several
single-fold drawdowns, but diluted the strongest forecasts, raised aggregate
benchmark dependence, and reduced performance after costs. Construction
searches on these observed folds are closed. The shared implementation remains
available for strategies that independently select those policies.

## Verdict and next experiment

This run fails because:

1. no holding period reaches the required 2.0: the best, 20 sessions, has
   Sharpe 0.99 base and 0.74 under doubled spread/impact;
2. current survivors are projected backward and delisted outcomes are absent;
3. historical borrow quantity is unavailable and historical fee-rate coverage
   was not joined to this public-data study;
4. spreads and impact are conservative floors, not instrument/date observations;
5. the price/volume baseline, market/sector context expansion, and two frozen
   construction challengers all fail the target, so further reuse of the same
   observed folds would turn them into model-development data without creating
   new out-of-sample evidence.

The baseline is strong enough to justify completing the data work, not paper
orders. The next controlled study needs genuinely new information: licensed
point-in-time membership/delistings plus point-in-time fundamentals and
earnings revisions, IB fee-rate history, observed spreads, and historical
availability or a declared availability scenario. Proper trailing-beta,
idiosyncratic-risk, and market/sector-residual features remain valid candidates,
but their final verdict must use a new time holdout or newly acquired historical
coverage. The original 20-session model is the frozen baseline to beat. Do not
tune further on these folds; both baseline and context results have now been
observed.
