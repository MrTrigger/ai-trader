# Stockholm Long/Short Portfolio Bot — Design Specification

> **Status:** Phase 0 foundations in implementation; no capital authorization  
> **Date:** 2026-08-10  
> **Scope:** Nasdaq Stockholm shares through Interactive Brokers (IB), starting
> with a paper account  
> **Parent design:** [`design-spec.md`](design-spec.md). Its deterministic money
> path, fail-closed behavior, replay/live parity, append-only evidence, and phase
> gates remain non-negotiable.

## 1. Decisions in one page

The bot manages a cross-sectional long/short portfolio over **Nasdaq Stockholm
Main Market Large, Mid, and Small Cap**, not OMX Stockholm 30 and not First
North. The Rust matrix and live-universe contract reject First North rows.

The model predicts each share's signed forward return. The constructor turns
that forecast into two possible trade edges:

```text
long_edge(i)  =  predicted_return(i) - long_cost(i)  - uncertainty(i)
short_edge(i) = -predicted_return(i) - short_cost(i) - borrow_cost(i)
                - uncertainty(i)
```

Only the better direction may be a candidate for a share, and only when its net
edge is positive by a configured safety margin. All candidates then compete for
capital regardless of direction. There is **no 50/50 long/short target and no
market-neutrality target**. If there are 25 credible longs and two credible
shorts, the result may be strongly net long; it must not manufacture shorts to
fill a quota. If neither direction clears the threshold, the portfolio holds
cash.

Risk limits still apply. “No forced neutrality” does not mean unlimited market,
sector, issuer, liquidity, or borrow risk. Net exposure and beta are reported
outputs and hard-bounded safety variables, not optimization targets.

Operational cadence:

- ingest, validate, and score after every Stockholm trading session;
- make ordinary portfolio changes every five trading sessions initially;
- allow unscheduled risk-reducing changes every day for stale data, corporate
  actions, borrow loss/recall, limit breaches, or reconciliation failures;
- retrain on a quarterly schedule initially, promoting a new model only after
  walk-forward validation. Rebalancing never implies automatic retraining.

The five-session holding cadence is a starting hypothesis, not a truth. Daily,
five-session, ten-session, and twenty-session alternatives must be compared
after realistic turnover and short costs. The production cadence is the centre
of a stable out-of-sample plateau, not the best backtest point.

## 2. What data is available now

### 2.1 Confirmed through the paper Gateway

The current Nordic Equity L1 subscription is active through the API. A
read-only probe on 2026-08-10 confirmed:

- Volvo B resolves to IB contract `conid=917920`, local symbol `VOLV B`, primary
  exchange `SFB`, currency `SEK`;
- daily `ADJUSTED_LAST` history is available;
- a ten-year request returned 2,512 daily bars from 2016-08-15 through
  2026-08-10;
- ten years of daily borrow-fee history are available for the tested contract;
  a request returned 2,369 bars from 2016-08-15 through 2026-08-07;
- no order was submitted by either probe.

The last dated bar in a request made during an open session may still be
forming. It proves availability, not finality; the collector accepts a session
bar for features only after the official close and completeness checks.

IB documents `ADJUSTED_LAST` stock bars as adjusted for splits and dividends.
It documents ordinary `TRADES` bars as split-adjusted but not dividend-adjusted.
Both series are required: adjusted bars for total-return-like signals and raw
trade bars for historically meaningful price, range, and traded-notional
measurements.

### 2.2 Available from IB, to be proven across the universe

- current contract metadata and stable `conid` mapping;
- historical daily adjusted and trade OHLCV;
- live top-of-book bid, ask, last, and size under the Nordic Equity L1
  subscription;
- exchange schedules and trading sessions;
- current account, cash, positions, open orders, fills, margin, and permissions;
- current shortability/availability and fee rate where IB supplies them.

The collector must run a bounded breadth/depth audit before training. A single
successful Volvo request proves the connection and one contract, not all
Stockholm instruments.

### 2.3 What IB does not solve

IB explicitly says historical data is not available for securities that are no
longer trading, may be absent before an exchange move, may change as IB adjusts
or filters it, and is subject to pacing limits. Consequently, IB alone cannot
produce a survivorship-free historical Stockholm universe.

The following are still required before a production-quality backtest can pass:

1. **Point-in-time universe membership.** For every session: instruments that
   were listed, delisted, suspended, or transferred, including their then-valid
   venue and share class.
2. **Delisting outcomes.** The final cash/share treatment and return of names
   that disappeared.
3. **Point-in-time issuer and sector mapping.** Current classifications must not
   be projected backwards without an effective date.
4. **Historical borrow availability.** IB's TWS/Gateway API supplies deep
   fee-rate history, but it does not supply the corresponding historical
   quantity that this account could actually borrow. Fee history prices a
   feasible short; it does not prove that the short was feasible.
5. **Point-in-time fundamentals**, if fundamentals are later added. Report date
   is not enough; the data needs the timestamp when the value became observable.

Therefore the answer to “do we have the data?” is:

- **yes** for building the collector, proving the broker path, and training an
  exploratory price/volume baseline on current survivors;
- **no** for claiming an unbiased, production-ready long/short edge until the
  historical-universe and short-borrow gaps are closed or the test window is
  restricted to prospectively collected data.

An exploratory current-survivor backtest must be labelled
`SURVIVORSHIP_CONTAMINATED` in its artifact and UI. It cannot pass the capital
gate regardless of its Sharpe ratio.

## 3. Universe contract

### 3.1 Coverage universe

Store all Stockholm-traded instruments discovered from the Nasdaq listings
source and resolve each against IB. The instrument master is effective-dated
and contains at least:

```text
instrument_id       internal stable id
conid               IB contract id
isin
symbol
local_symbol
issuer_id
share_class
security_type
listing_venue       STO_MAIN
primary_exchange
currency
sector_icb
listed_at
delisted_at
metadata_valid_from
metadata_valid_to
```

`symbol` is display metadata, not identity. Orders and joins use the internal
id and IB `conid`.

### 3.2 Phase-one tradable universe

Include ordinary shares and depositary receipts whose primary listing is on
Nasdaq Stockholm Main Market Large, Mid, or Small Cap.

Exclude by default:

- ETFs, funds, ETPs, warrants, certificates, rights, subscription instruments,
  SPAC units, preferred shares, and auction-only instruments;
- non-SEK listings in phase one;
- a secondary/dual listing when Stockholm is not the primary price-discovery
  venue;
- instruments without at least 252 valid daily observations;
- instruments with an unresolved corporate action or ambiguous IB contract;
- instruments failing configurable liquidity, spread, price, or capacity
  tests.

When an issuer has multiple ordinary share classes, aggregate risk at issuer
level. The constructor must not be long one class and short another. By default,
only the most liquid eligible class enters the candidate set; an explicit
economic reason and separate test are required to trade both.

### 3.3 Liquidity eligibility

Initial defaults are deliberately conservative and configurable:

```text
median_traded_notional_20d >= SEK 5,000,000
zero_volume_days_60d       <= 3
median_spread_bps_20d      <= 75       # once snapshots exist
price                      >= SEK 5
target_position_notional   <= 1% of 20d average daily traded notional
single_order_notional      <= 5% of expected session volume
```

The backtest must sweep these thresholds. A profitable result that exists only
in names too illiquid for its simulated positions is a failed result.

First North may remain in a provider's raw reference archive, but it is outside
this bot's train, score, backtest, and trade universe. The final Rust matrix
manifest records `NASDAQ_STOCKHOLM_MAIN_LARGE_MID_SMALL` and tests reject any
First North leakage.

## 4. Storage and data lineage

Daily data is immutable and versioned rather than overwritten when IB later
revises history:

```text
instrument_master      effective-dated metadata and identity
universe_members       session, instrument_id, bucket, eligible, reasons
bars_adjusted_daily    session, adjusted OHLCV, source, retrieved_at, content_hash
bars_trade_daily       session, trade OHLCV, source, retrieved_at, content_hash
quotes                 timestamp, bid/ask/last/sizes, source
borrow_snapshots       timestamp, shortable, available_qty, fee_rate, source
corporate_actions      effective date, type, terms, source
features               session, instrument_id, feature_set_version, values/masks
labels                 entry_session, exit_session, horizon, value, label_version
model_artifacts        model/version/features/training interval/calibration
plans/fills/nav        inherited append-only trading records
```

Rules:

- a daily bar is keyed by the Stockholm exchange session, not the machine's
  local date;
- a feature row records the latest input timestamp and cannot see a bar whose
  session was not closed at decision time;
- all external datasets retain source, retrieval timestamp, effective timestamp,
  and hash;
- a training run pins exact data versions and point-in-time universe snapshots;
- historical corrections create a new data version and a new research artifact;
- collection is globally rate-limited and restartable to respect IB pacing;
- missing or partial universe data fails the live run closed.

The exchange calendar, including exceptional sessions and half-days, is data.
Jobs trigger relative to the official session close rather than a hard-coded
wall-clock time.

## 5. Training problem

### 5.1 Observation and decision timing

At decision session `t`:

1. use only data finalized by the close of `t`;
2. produce and persist a plan after the close;
3. simulate and execute the change in session `t+1` using the same declared
   execution policy;
4. measure the label from the modeled executable entry in `t+1` to the modeled
   executable exit after `H` sessions.

The phase-one horizon is `H=5` trading sessions. Research must also report
`H={1,10,20}` and corresponding turnover; it may not choose a horizon from a
single best result.

Backtests initially model next-session open plus spread and impact. Before live
capital, this is replaced or bounded by a model fitted from collected intraday
quotes and actual paper fills. The live execution window must match the label's
entry convention closely enough that the backtest remains about the strategy
we actually trade.

### 5.2 Label

The primary target is the share's **absolute signed forward return**:

```text
raw_forward_return(i,t,H) = executable_exit / executable_entry - 1
target(i,t,H)             = raw_forward_return(i,t,H)
```

This choice is essential. A target demeaned within each date—or fully
residualized against the realized future market—can rank winners against losers
but removes the common market direction. It cannot honestly decide whether the
whole opportunity set is attractive to own or short, which would reintroduce an
implicit market-neutral portfolio. The model therefore sees the common regime
features in §6.2 and predicts an absolute return. Each date receives equal total
training weight so a date with more listed shares does not dominate the fit.

The matrix also emits a secondary research label:

```text
residual_return = raw_forward_return
                  - beta(i,t) * future_market_return
                  - future_sector_excess_return
```

It is used for rank diagnostics and a decomposed-model challenger, not as the
only production target. The beta and sector definitions are frozen by
`label_version`. Labels are clipped using training-only quantiles to reduce
corporate-action/data-error domination. Raw forward return is always retained
so the portfolio simulator—not the model-fitting script—calculates economic
P&L.

Costs are not hidden in the label. The model estimates return; the constructor
subtracts the direction-specific commission, spread, impact, turnover, and
borrow estimates. This makes a score reusable across account sizes while making
the decision explicitly cost-aware.

### 5.3 Model family

Start with two models, not a model zoo:

1. regularized linear regression as the auditable baseline;
2. shallow LightGBM regression, using the repository's existing fit/export
   path.

Rust owns instrument alignment, feature calculation, normalization, missing
handling, labels, folds, and the final training matrix. Python may only fit the
final Rust-emitted matrix and export the model. Production Rust rejects an
artifact whose feature names, feature-set version, label version, or
`trained_through` timestamp disagree with runtime expectations.

The selected model predicts signed absolute return `mu`. Out-of-sample
validation predictions are calibrated to expected basis points by score bucket;
the constructor consumes the conservative lower confidence estimate, not the
raw tree score. A five-seed ensemble may be used only if it improves fold
stability; model disagreement becomes part of `uncertainty(i)`.

Default refit cadence is quarterly. A refit is a candidate artifact, not an
automatic promotion. The previous model remains active until the candidate
passes the same gates and a deterministic promotion command records the change.

## 6. Feature set v1

The feature set is intentionally price/volume-only because those inputs can be
made causal now. These are hypotheses to test, not assumed edges. Momentum,
short-horizon reversal, and liquidity measures are included because they have
longstanding empirical motivation, including momentum documented by Jegadeesh
and Titman, short-horizon return dependence documented by Jegadeesh, and the
daily return/traded-value illiquidity measure proposed by Amihud. That research
does not establish profitability after 2026 Swedish costs; only our walk-forward
test can do that.

### 6.1 Exact asset-level candidates

All returns use dividend/split-adjusted closes. Range and traded-notional
features use `TRADES` bars. Every rolling value is computed using rows at or
before `t`.

| Group | Feature | Definition / purpose |
|---|---|---|
| medium trend | `ret_21`, `ret_63`, `ret_126`, `ret_252` | adjusted close return over 1, 3, 6, and 12 months |
| skip momentum | `ret_252_skip_21` | return from `t-252` to `t-21`; separates medium momentum from recent reversal |
| short trend | `ret_5` | one-week movement; model may learn continuation or reversal |
| reversal | `ret_1` | last-session return |
| trend quality | `efficiency_63` | absolute 63-day return divided by sum of absolute daily returns |
| price location | `dist_high_252` | adjusted close / trailing 252-session high - 1 |
| price location | `dist_sma_200` | adjusted close / trailing 200-session mean - 1 |
| total risk | `vol_20`, `vol_60`, `vol_252` | annualized realized daily volatility |
| downside risk | `semivol_60` | annualized volatility of negative daily returns |
| drawdown | `drawdown_252` | adjusted close / trailing 252-session peak - 1 |
| asymmetry | `skew_60` | trailing daily-return skewness, minimum observation count enforced |
| market risk | `beta_252` | trailing beta to the point-in-time eligible-universe market return |
| idiosyncratic risk | `idio_vol_60` | volatility of residual returns after market and sector components |
| capacity | `log_adv_sek_20`, `log_adv_sek_60` | log average daily traded notional; primarily a capacity/cost feature |
| price impact | `amihud_20`, `amihud_60` | mean `abs(return) / traded_notional_sek` |
| activity | `volume_surprise_20` | log current volume / trailing median volume |
| discontinuity | `gap_1` | raw open / prior raw close - 1 |
| intraday range | `range_frac_14` | mean `(high-low)/close` over 14 sessions |
| close location | `close_location_5` | mean close position within the daily high-low range |
| relative trend | `market_resid_ret_21`, `market_resid_ret_126` | return after trailing-beta market component |
| relative trend | `sector_resid_ret_21`, `sector_resid_ret_126` | return relative to effective-dated ICB sector peers |

`log_adv` must not be mistaken for a free small-cap premium. Its main job is to
help the model distinguish signals that occur in different execution regimes;
hard capacity limits remain outside the model.

### 6.2 Common regime context

The following are common to every instrument on a decision date:

- eligible-universe market return over 21 and 126 sessions;
- market realized volatility over 20 and 60 sessions;
- breadth: fraction above the 200-session mean;
- cross-sectional return dispersion over 20 sessions;
- fraction of eligible names with positive 63-session return.

These values are trailing-time-series standardized and are **not**
cross-sectionally ranked. Ranking a value that is identical for every share
destroys the information. They allow the model to learn that an asset signal may
behave differently in broad advances, selloffs, and high-dispersion regimes.

### 6.3 Normalization and missing values

For each session and universe bucket:

1. winsorize each asset-level feature at training-derived bounds;
2. percentile-rank the available cross-section to `[-1, 1]`;
3. emit an explicit missing indicator for every optional feature;
4. set the normalized missing value to zero only after the indicator exists.

Required slow features make a share ineligible until enough history exists.
Optional sector-relative features may be missing when a sector has too few
eligible members. No backward fill is permitted. No normalization statistic may
use a future row or the test fold.

### 6.4 Inputs deliberately excluded from v1

- raw nominal share price as an alpha feature;
- ticker or `conid` identity;
- current market cap, shares outstanding, or sector copied backwards;
- current borrow fee copied backwards;
- news, social sentiment, or LLM opinions;
- fundamentals without publication/availability timestamps;
- any technical indicator whose source inputs are not already represented and
  whose incremental value has not survived ablation.

Borrow data is initially an eligibility and cost input, **not a predictive
feature**. IB fee-rate history may be used to charge historical short costs after
a universe-wide coverage audit. Availability remains a separate conservative
gate. Borrow variables may enter a later predictive feature set only through an
independent ablation and walk-forward test.

Point-in-time fundamentals are the most promising v2 expansion: valuation,
quality, profitability, leverage, accruals, earnings revisions, and reporting
surprise. They require a separate licensed/effective-dated data contract and are
not approximated from today's values.

### 6.5 FI PDMR research challenger

The shared `equity-data` crate archives FI's official PDMR transaction exports
in bounded publication-date intervals. Dense intervals are split automatically
before FI's 1,000-row export ceiling can truncate them. Raw UTF-16 exports are
immutable and resumable; the normalized dataset retains publication date,
transaction date, ISIN, initial/amendment flags, current status, volume, price,
currency, and transaction type.

Feature set `fs-rust-stockholm-5` is a predeclared ablation on top of the v3
residual inputs. `features-stockholm`, not Python or the bot, calculates:

- signed SEK PDMR transaction value over 30 and 90 calendar days;
- gross SEK acquisition and disposal value over 90 days;
- transaction count over 30 days and unique acquirers over 90 days;
- days since the latest qualifying acquisition.

Only initial notifications of positive-price, positive-quantity share
acquisitions/disposals outside an option programme qualify. Cash-value features
require SEK; counts, unique acquirers, and recency retain other currencies. An
observation is
available only on a decision date strictly after its **publication date**. The
transaction date never moves it backwards. V1 deliberately ignores the
register's current revised/cancelled status and later amendment rows: using
today's status to remove an earlier filing would leak future knowledge, while
counting both versions would duplicate it. This preserves the signal that was
actually public at the time, with the limitation recorded in the matrix.

The challenger is not production evidence. It must beat the unchanged v3
control on the same purged expanding folds, base costs, doubled-cost stress,
OMXSGI comparison, and fold-stability gates. It is rejected unless the gain is
both economic and stable; adding a plausible data source is not itself alpha.

## 7. Portfolio construction

### 7.1 Candidate selection

For every eligible share, calculate conservative long and short edge in expected
basis points. The short side includes expected borrow over the intended holding
horizon and a hard-to-borrow uncertainty surcharge. A share contributes at most
one directional candidate.

Discard a candidate when:

- calibrated edge does not exceed all modeled costs plus the safety margin;
- the current quote is stale or the expected spread breaches the limit;
- order or position size breaches its capacity limit;
- the share has a pending corporate action the system cannot model;
- a short has no confirmed availability, too little available quantity, or an
  unknown fee;
- model disagreement or missing inputs exceed configured bounds.

Rank the survivors by net edge adjusted for marginal portfolio risk. Direction
does not receive a quota or bonus.

Market and single-name trends are model inputs, not eligibility gates. A hard
trend gate would duplicate momentum already visible to the ranker and remove
reversal/value candidates precisely when they can add independent information.
Only tradability, data-integrity, cost, capacity, and borrow conditions are
mandatory gates.

### 7.2 Objective

The direction layer and stock-selection layer solve different problems. A slow
market regime signal proposes maximum gross exposure `G` and target net
exposure `N`; the stock ranker proposes which names deserve exposure and their
pre-constraint absolute weights. The allocator converts direction into sleeve
maxima:

```text
maximum long budget  L = (G + N) / 2
maximum short budget S = (G - N) / 2
required invariant       |N| <= G
```

`L`, `S`, and `G` are ceilings, never quotas. The allocator may only scale a
preliminary weight down. It may not normalize candidates to spend a sleeve,
redistribute a capped name's excess, create the missing direction, or recycle
unused short capacity into longs. Borrow scarcity, a lack of positive
after-cost edge, caps, and minimum trade values therefore leave cash and cause
realized net/gross to differ visibly from the requested regime exposure.

Before the regime layer is fitted, research uses a direction-free gross
ceiling: either sleeve can consume gross, but the combined book cannot exceed
it. This preserves the requirement to choose the strongest valid candidates
irrespective of sign rather than assuming a neutral target.

Preliminary equal, conviction, and risk-adjusted sizing use fixed calibration
anchors learned or declared before the evaluated fold. They do not divide by
the scores of whichever candidates happen to be present that day. Per-name
capacity, issuer caps, and cost-derived minimum trade values are carried as
candidate-specific bounds into the shared allocator.

The future covariance risk layer uses shrinkage/EWMA or a small factor model,
including a down-market correlation estimate, to impose a scale-down-only
portfolio-volatility ceiling. It does not scale a quiet book upward to hit a
volatility target.

Choose signed weights `w` to maximize:

```text
sum(w_i * mu_i)
- risk_aversion * predicted_portfolio_variance(w)
- turnover_cost(current_weights, w)
- borrow_cost(short_weights)
- concentration_penalty(w)
```

subject to hard limits. Cash is a valid and often correct output.

Initial paper limits, configurable and revalidated for the funded account:

```text
gross exposure                 <= 1.00 NAV
absolute net exposure          <= 1.00 NAV   # no forced neutrality
absolute predicted market beta <= 1.25       # catastrophe ceiling, not target
single issuer gross            <= 0.05 NAV
single short issuer            <= 0.03 NAV
sector gross                   <= 0.25 NAV
position notional              <= 0.01 * ADV20
portfolio daily turnover       <= 0.25 NAV outside risk exits
```

The beta bound is a catastrophe guard, not a beta target. It may be relaxed only
through evidence and configuration review. There is no minimum number of longs,
shorts, or holdings. There is no requirement to spend all available gross
exposure.

Each rebalance report records requested `G`/`N`, maximum long/short budgets,
proposed and realized long/short/gross/net, unused budgets, capped names, and
positions dropped below their economic minimum. That attribution is part of
the replay contract rather than a frontend reconstruction.

Example acceptance case: when 25 long candidates and two short candidates clear
the same net-edge threshold, the plan may contain 25 longs and two shorts. A
test must prove that the constructor does not add weak shorts, discard strong
longs merely to balance counts, or force net exposure to zero.

## 8. Short-sale design

Shorting is a first-class direction, but it is not symmetric operationally with
buying:

- the account must have the required Swedish-stock trading and margin
  permissions;
- availability and fee are fetched immediately before planning and again before
  submission;
- the planned quantity is capped below confirmed available quantity with a
  configurable buffer;
- expected borrow, commissions, spread, impact, and dividend liability are in
  the short cost;
- borrow loss, recall, or a fee jump can generate a risk-reducing cover outside
  the ordinary rebalance cadence;
- covering an existing short remains allowed when opening shorts is disabled;
- corporate actions and cash dividends on borrowed shares are reconciled
  explicitly;
- the engine never silently substitutes another share or instrument when the
  intended borrow is unavailable.

Paper fills and paper shortability are plumbing evidence, not proof of live
borrow behavior. Before normal live size, run a shadow phase against live quotes
and borrow responses, followed by tiny live positions whose fee accruals,
rejections, recalls, and fills are compared with the model.

Until historical borrow coverage exists, research publishes at least three
short-cost scenarios and an availability haircut. A backtest that assumes every
historical loser was freely shortable cannot pass the capital gate.

## 9. Evaluation, rebalance, and execution cadence

### 9.1 Daily evaluation

After each official Stockholm session closes and all expected bars arrive:

1. snapshot the universe, bars, quotes, borrow, account, and positions;
2. validate completeness, corporate actions, and broker reconciliation;
3. materialize features and scores;
4. persist a proposed target and risk report;
5. if it is not a scheduled rebalance, execute only required risk reductions.

This daily run detects deteriorating borrow and risk without paying daily
portfolio turnover.

### 9.2 Scheduled changes

The default scheduled rebalance occurs every fifth valid Stockholm trading
session. It is calendar-counted, not hard-coded to a weekday, so holidays and
half-days do not distort it.

The cadence is promoted only after comparing:

| Cadence | Main question |
|---|---|
| daily | does faster response survive spread, impact, and turnover? |
| 5 sessions | initial balance between signal decay and cost |
| 10 sessions | does lower turnover retain most of the edge? |
| 20 sessions | is the signal primarily medium-horizon momentum? |

Compare identical models, folds, universes, and cost assumptions. Select a
stable region, not the maximum Sharpe cadence.

Research checkpoint: the predeclared 5/10/20-session comparison favored 20
sessions (Sharpe 0.99 versus 0.86 and 0.47 before later corrections), so the
v5–v10 studies use non-overlapping 20-session decisions. This is not promoted
policy—the later selectors remain below the Sharpe gate—but it supersedes the
five-session starting hypothesis for the current experiment. Daily evaluation
and immediate risk/borrow reductions remain unchanged.

### 9.3 Order policy

The decision uses session `t`; no order may claim a fill at that same close.
Phase-one simulation uses next-session open plus a conservative spread/impact
model. Paper execution uses bounded, price-protected orders in a declared
next-session window and cancels unfilled residuals rather than chasing without
limit.

Every order references an immutable Plan id. Before submission the executor
refreshes quote, buying power, positions, open orders, and shortability. A price
move or cost increase that removes the predicted edge cancels that leg. Partial
fills are recorded and reconciled; the next plan starts from broker truth.

## 10. Backtest and validation

### 10.1 Causality

Use the exact Rust feature and decision code in training-matrix generation,
replay, paper, and live. Prefix-invariance tests rebuild features with future
rows removed and require all earlier rows to remain byte-equivalent within the
declared numeric tolerance.

Use purged walk-forward splits. If the label horizon is `H`, remove overlapping
labels at fold boundaries and embargo at least `H` sessions. Hyperparameters,
normalization bounds, calibration, and feature selection are fit inside each
training/validation interval only.

With the currently observed ten-year depth, the initial study should use at
least five years before its first evaluated fold, followed by rolling validation
and untouched test intervals. Fold definitions are calendar artifacts committed
with the report.

### 10.2 Fill and cost model

The simulator includes:

- IB commissions and exchange fees from effective-dated configuration;
- half-spread plus an observed-or-conservative spread floor;
- nonlinear market impact as a function of order/ADV participation;
- delayed or partial fills under a limit-order policy;
- short borrow fee accrued by actual holding days;
- short availability haircuts and forced-cover scenarios;
- dividends and corporate-action cash flows;
- no fills during halts or outside valid sessions.

Run base, 2x spread/impact, and adverse-borrow scenarios. Current broker fee
schedules are configuration inputs, not numbers embedded in model code.

### 10.3 Baselines and reports

Compare against:

- cash;
- the broad eligible Stockholm universe, equal- and liquidity-weighted;
- a simple `12-1` momentum long/short rule;
- a simple short-term reversal rule;
- top-ranked long-only;
- the existing model with the candidate feature group removed.

Every fold and aggregate report contains:

- rank information coefficient and calibrated return by score bucket;
- long and short sleeve returns separately, before and after costs;
- portfolio return, volatility, Sharpe, maximum drawdown, and recovery;
- gross, net, beta, sector and issuer exposure through time;
- turnover and commission/spread/impact/borrow drag separately;
- borrow rejects, availability haircuts, recalls, and forced covers;
- capacity at different NAV values;
- performance by year, market regime, market-cap segment, liquidity bucket,
  sector, and universe bucket;
- observation and independent-decision counts.

### 10.4 Evidence gate

A candidate may enter shadow/paper operation only when all are true:

- the dataset is point-in-time or the report is explicitly non-promotable;
- aggregate out-of-sample Sharpe after base costs is at least **2.0**;
- aggregate return is positive after base costs and remains positive at 2x
  spread/impact;
- a majority of independent walk-forward folds are positive after costs;
- combined performance is not dependent on one year, one issuer, or one sector;
- long and short sleeves are individually reported and the short sleeve is not
  structurally negative after borrow;
- capacity exceeds the proposed account size with margin;
- feature ablations show that complexity adds stable out-of-sample value;
- reconciliation, corporate actions, and borrow simulations pass their tests.

Paper trading then validates data timing, orders, fills, costs, reconciliation,
and failure behavior. It does not repair a failed historical edge test.

## 11. Runtime architecture

Add asset-class-specific components without copying either the money path or
broker/data plumbing:

```text
features-stockholm      sole causal feature, label, matrix, and normalization implementation
stockholm-portfolio     universe policy, scores, constructor, replay, reports
ib                      all IB stock contract/data/order/borrow support
venue                   broker-neutral stock/order/shortability contracts where needed
stockholm-bot           observe -> decide -> plan -> execute -> reconcile orchestration
training/               Python fit/evaluation orchestration over the final Rust matrix only
```

The boundaries are enforced as follows:

- no IB request, response decoding, contract resolution, pacing, reconnect,
  account, position, order, quote, bar, or borrow implementation may live in
  `stockholm-portfolio` or `stockholm-bot`; extend `ib` and, when genuinely
  broker-neutral, `venue` instead;
- no broker-specific data type crosses into model or portfolio logic; the IB
  crate converts it to owned, serializable domain records first;
- every feature, label, alignment rule, missing-value policy, cross-sectional
  normalization, and final ordered training matrix is Rust in
  `features-stockholm` or a Rust portfolio module that calls it;
- Python may select folds, invoke LightGBM, compute evaluation summaries from
  the frozen matrix, and export the model artifact. It may not calculate,
  impute, lag, align, rank, winsorize, normalize, select, or rename production
  feature values;
- live inference and training-matrix generation link the same Rust feature
  functions and feature catalogue version. Artifact loading rejects a version
  or ordered-name mismatch;
- reuse an existing implementation only when it is genuinely instrument
  agnostic. If a second consumer reveals a common abstraction, extract it into
  a shared crate with parity tests rather than copying it into either bot.

Reuse from the existing project:

- Plan, event log, account/bot binding, reconciliation, and arming semantics;
- the shared `VenueAdapter` where its abstraction fits;
- LightGBM JSON export and strict artifact compatibility checks;
- research artifact/report conventions;
- conditional IB Gateway lifecycle: the Gateway may run only while an active
  IB-backed bot requires it.

Do not import `features-crypto` as the Stockholm feature library. Reuse patterns
and, where truly generic, extract tested utilities; retain a separate named
feature set and version because calendars, corporate actions, traded notional,
sectors, share classes, and short borrow are equity-specific.

Suggested bot identity and broker binding:

```text
bot_id:          stockholm-portfolio
protocol:        ib
account:         dedicated paper binding, then dedicated live binding
base currency:   SEK exposure accounting
orders armed:    false by default; two-flag paper/live gates retained
```

The deployed identity is `stockholm-portfolio`. The registry row is created
disabled and without a launch command until the runtime exists. Because the
frontend renders one navigation tab per registered bot, this gives Stockholm a
separate `/bot/stockholm-portfolio` tab and page from the start; it is not a
mode or panel inside the crypto or futures bot.

Implemented foundation as of 2026-08-11:

- `ib::stocks` owns account-verified stock contract resolution, raw/adjusted/
  fee-rate daily history, and bounded current L1/shortability snapshots;
- `features-common` owns the previously crypto-local cross-sectional rank
  transform, with the crypto API re-exporting the same implementation;
- `features-stockholm` owns the first versioned Rust-only daily feature
  catalogue, raw-versus-adjusted price semantics, validation, selection, and
  final cross-sectional normalization. It also owns the causal, provider-neutral
  OMX trend observation used by direction-layer ablations;
- records migration `0010_stockholm_portfolio.sql` registers the disabled bot
  and extends IB's supported asset classes so the existing generic frontend
  produces the separate tab.
- `equity-data` owns the first reproducible public current-universe research
  collector, including provider decoding, OHLC validation, and official Nasdaq
  OMXSGI SOD/EOD benchmark history. It also owns resumable FI, Skatteverket,
  Nasdaq notice/report, ESEF, Riksbank, and Nasdaq daily market-history
  collectors; provider response types do not cross into bot or feature code;
- `lightgbm-json` owns strategy-neutral tree evaluation;
- `portfolio-construction` owns strategy-neutral no-quota ranking, fixed-anchor
  preliminary sizing, `(gross, net)` to long/short maximum-budget conversion,
  scale-down-only allocation, and the reusable hysteresis/volatility/ramp state
  machine. It never water-fills cap excess or unused sleeve budget;
- `features-stockholm` now emits the final Rust labels, missing indicators,
  sample weights, absolute market and residual-return label components, and
  ordered training matrix, with a tested Main Market-only Large/Mid/Small
  universe filter. Its versioned residual-risk v3 contract adds causal beta,
  idiosyncratic risk, liquidity/impact, intraday-structure, and market/sector
  residual momentum inputs with prefix-invariance coverage. The v9 contract
  adds provider-neutral completed-session Nasdaq spread/trade observations;
  its closed-fold Sharpe improved to 0.76 but still failed the 2.0 gate and
  remains research-only;
- `stockholm-portfolio` owns no-quota direction selection and turnover-aware
  replay, including exact-horizon OMXSGI attribution and an explicitly enabled
  direction-overlay ablation with separate direction-only metrics. Relative
  models cannot replay without that explicit market-direction layer. The common
  direction state immediately removes stale old-side net after a confirmed
  neutral/reversal decision, then ramps only new risk;
- the training exporter and Rust runtime support version-bound LightGBM,
  weighted-ridge, and fixed hybrid research artifacts. Every fold records its
  model family and feature-set version, and unlike specifications cannot be
  stitched. The corrected reward/loss and residual challenger results are
  recorded in
  [`stockholm-reward-loss-study.md`](stockholm-reward-loss-study.md); all are
  survivor-contaminated and fail the pseudo-holdout and Sharpe evidence gates.

## 12. Failure policy

No new exposure is permitted when any of these is true:

- universe snapshot missing or incomplete;
- adjusted/trade bar mismatch is unexplained;
- data is stale or the exchange session is ambiguous;
- model artifact and runtime feature contract differ;
- broker positions, cash, or open orders do not reconcile;
- live quote or FX/account valuation input is stale;
- shortability cannot be refreshed for a proposed short;
- a corporate action is unresolved;
- risk-model covariance is invalid or hard limits cannot be evaluated;
- IB Gateway, data farm, or account binding is unavailable.

Risk-reducing exits remain possible through an explicitly tested path. The
system alerts and remains observable while halted; it does not continually
retry orders into an unknown state.

## 13. Implementation phases

### Phase 0 — data proof and prospective archive

- enumerate Main Market Large, Mid, and Small Cap and reject other segments;
- resolve and persist IB contracts;
- audit ten-year daily depth and quality across the coverage universe;
- start daily immutable universe, quote, borrow, and corporate-action snapshots;
- acquire or construct licensed point-in-time listings/delistings history.

**Exit:** coverage report names every omission; current daily collection is
complete and restartable; survivorship status is explicit.

### Phase 1 — deterministic research baseline

- implement `features-stockholm` and prefix-invariance tests;
- implement labels, cost/borrow scenarios, purged folds, and linear baselines;
- replay simple momentum/reversal and long-only baselines.

**Exit:** causal, reproducible report with no unidentified data leakage.

### Phase 2 — learned ranker and constructor

- emit the final Rust matrix and fit shallow LightGBM;
- calibrate signed forecasts and implement no-quota directional selection;
- test cadence, feature groups, costs, constraints, and capacity.

**Exit:** passes §10.4, including 2x costs and direction-separated reporting.

### Phase 3 — shadow and paper

- run after every session without orders, then paper orders;
- compare planned vs executable edge, spread, fills, and borrow responses;
- test reconnects, restarts, recalls, partial fills, and reconciliations.

**Exit:** operational results fall within predeclared backtest error bars and all
failure drills fail closed.

### Phase 4 — tiny live

- dedicated live account binding and explicit arming;
- size below both risk and measured liquidity capacity;
- verify actual commissions, dividends, borrow accruals, rejects, and recalls.

**Exit:** expand only through a new reviewed configuration. No model may increase
its own leverage or limits.

### Phase 5 — optional expansions

- point-in-time fundamentals and estimates;
- intraday execution model improvements;
- broader Nordic venues as separate universe buckets.

Each expansion receives a new feature/data version and repeats the appropriate
evidence gates.

## 14. Required acceptance tests

1. Feature prefix invariance for every column.
2. Rust training/live matrix parity on a fixed fixture.
3. No feature or label uses session `t+1` at decision `t`.
4. A delisted fixture remains in historical membership through its effective
   delisting and receives the correct terminal outcome.
5. A split/dividend fixture produces correct adjusted returns and raw traded
   notional.
6. Sector and issuer changes are effective-dated.
7. A/B share classes cannot create opposite issuer exposures.
8. Twenty-five valid longs and two valid shorts do not become a forced 50/50
   portfolio.
9. Zero valid shorts does not prevent a valid long plan; zero valid candidates
   yields cash.
10. An unavailable or unknown borrow rejects only new/increased short exposure;
    covering remains possible.
11. Borrow recall generates a bounded cover plan and alert.
12. Stale/missing/incomplete input produces no new order.
13. Duplicate run/Plan execution is idempotent.
14. Partial fills and restart reconcile to IB truth.
15. Same inputs and artifact produce the same Plan in replay and live mode.
16. A model trained through or after its decision timestamp is rejected.

## 15. Data acquisition decision

Historical prices, membership, borrow cost, and borrow availability are four
different data products. No source is assumed to solve all four.

### 15.1 Delisted securities and point-in-time membership

The preferred turnkey candidate is **FactSet Prices & Returns plus its
reference/corporate-action data**. FactSet explicitly advertises active and
inactive securities, long European price history, splits, dividends, and stable
symbology. It is not accepted merely on that global claim: procurement must
obtain an ISIN-level sample that proves coverage of inactive securities from
Stockholm Main Market Large, Mid, and Small Cap.

If institutional or academic access already exists, **Compustat Global through
WRDS** is the alternative to test. Its historical identifiers and active and
inactive security data are useful for survivorship-free research, subject to
the institution's export and derived-data licence.

The lower-cost construction path is:

1. use Nasdaq's annual changes-to-the-list notices and historical listing
   notices to reconstruct listing, transfer, suspension, and delisting events;
2. use Skatteverket Aktiehistorik to audit Main Market corporate actions and
   terminal events;
3. start archiving Nasdaq Nordic Reference Data Files every day from phase 0;
4. add a licensed EOD-price feed only after it demonstrates inactive-security
   coverage. Börsdata is a candidate for current and long-history Nordic EOD
   data, but its public API description does not promise a complete inactive,
   point-in-time universe, so it is not yet the survivorship solution.

Nasdaq's reference files cover Stockholm and First North but expose only the
most recent 30 days. They are suitable for the prospective archive, not by
themselves for a ten-year backfill. Skatteverket is an excellent independent
event audit, not a bulk daily-price source, and generally stops following a
company after delisting.

Acquisition checkpoint, 2026-08-12: the complete Skatteverket archive contains
1,648 company pages and 4,477 parsed history rows. Conservative text
classification identifies 955 Main Market rows across 473 names, including
150 delisting rows. The official Nasdaq equity-notice archive contains 1,071
Stockholm notices from 2016-01-01 through 2026-08-12. There are 190 delisting
notices, 189 with a checksummed ISIN, 176 with an order-book ID, and 177 with
an explicit last trading date. The notice category also contains ETPs, rights,
and other instruments, so these counts remain discovery/reference data until
intersected with an effective-dated ordinary-share security master.

The public Nasdaq Nordic instrument API was also tested against official
delisted order-book IDs for Swedish Match, Acando, and OX2. All return
`Instrument not found`; Nasdaq's public search likewise omits them. For active
shares, the endpoint exposes approximately ten years of daily raw OHLC,
closing bid/ask, turnover, and trade count. The shared collector archives those
fields resumably and labels the result `SURVIVORSHIP_CONTAMINATED`. They improve
liquidity and transaction-cost measurement but do not satisfy the inactive-
price or terminal-return acceptance tests.

The 2026-08-12 archive has 878,723 valid sessions for 411 of 412 current or
explicitly supplied prior-snapshot Main Market lines; 864,294 sessions have a
two-sided closing quote and all valid sessions have a trade count. One new
listing has only 25 sessions and is reported below the 30-session validity
floor. Nilörngruppen disappeared from the current screener immediately after
its 2026-08-10 final session, but its earlier `TX1809018` identifier was still
temporarily resolvable. Supplying the prior universe snapshot preserved 2,508
valid official sessions through the SEK 75.40 terminal close. This is a useful
prospective recovery path, not evidence that older inactive IDs remain
available.

The same three official delisted ISINs were then queried against the logged-in
IB paper Gateway on 2026-08-12. IB contract-details resolution returned error
200, “No security definition has been found,” for all three, before a price
request could be made. Yahoo's public chart endpoint likewise returned 404,
“symbol may be delisted,” for all three tickers. The free Nasdaq/Yahoo/IB stack
therefore cannot supply the inactive-price panel; that remains a licensed-data
procurement requirement rather than an engineering retry.

Before choosing a provider, submit a fixed acceptance pack containing at least
twenty delisted Main Market ISINs across Large, Mid, and Small Cap, including
ticker changes, venue transfers, acquisitions for cash/shares, bankruptcy,
suspension, and multiple share classes. The returned sample must contain:

- effective-dated venue, ticker, ISIN, share class, and listing status;
- unadjusted OHLCV and adjustment factors, or both raw and total-return series;
- dividend, split, rights, spin-off, merger, and terminal consideration terms;
- the last tradable session and a defensible terminal return;
- redistribution and derived-data rights compatible with stored model and
  backtest artifacts.

No provider passes on name count alone. Missing dead names, missing final
consideration, or mapping old tickers to today's issuer without effective dates
is a failed sample.

### 15.2 Historical securities lending

IB TWS/Gateway `FEE_RATE` is the primary historical **borrow-cost** source. The
2026-08-10 probe returned 2,369 Volvo B daily observations over roughly ten
years. Phase 0 must repeat the test across every current Main Market candidate
and record coverage by size bucket and year.

The reusable audit is implemented in the shared IB data source and its
stock_coverage_audit example. It resolves each share class by ISIN on SFB,
archives versioned raw TRADES, ADJUSTED_LAST, and FEE_RATE records, and writes
a coverage report. Completed archives are validated and reused, making a
multi-hour full-universe run resumable. The audit deliberately samples each
size bucket before a full run. IB documents historical requests as paced
traffic (at most 60 queries per 600 seconds), so the command rejects a
universe-scale run with an unsafe pause:

    set -a
    source .env
    cargo run --manifest-path service/Cargo.toml -p ib \
      --example stock_coverage_audit -- \
      var/stockholm/research-public/universe.json \
      var/stockholm/ib-audit-sample 10 3 1000

Because availability is not backfilled, the shared `stock_borrow_snapshot`
collector also captures current L1, IB shortability tier, and account-visible
lendable quantity for the complete Main Market universe. It writes immutable
timestamped snapshots plus `latest.json`; missing ticks remain missing. This
prospective archive is broker data infrastructure and contains no bot feature
or portfolio logic.

The first capture at 2026-08-12 10:45 Stockholm time resolved and snapshotted
408 of 412 lines. Account-visible quantity was present for 342: 140/162 Large,
119/141 Mid, and 83/109 Small Cap inputs (the denominators include four
unresolved lines). Median visible quantity was 295,700 shares and the 90th
percentile was 2,687,601. This directly disproves an assumption that only the
OMXS30-like subset can ever be shorted, while remaining only one paper-account
snapshot—not historical availability and not a future locate guarantee.

Audit checkpoint, 2026-08-10: after correcting the configured paper account,
a representative evenly spaced sample of ten Large, ten Mid, and ten Small Cap
shares resolved 30/30 contracts and returned FEE_RATE history for 30/30.
History length ranged from 51 sessions for a new listing to 2,358 sessions,
with a median of 1,965; every series ended on 2026-08-07 or 2026-08-10. Raw
latest values ranged from 0.0075 to 0.5144 (median 0.012), while the largest
historical observation was 5.6476.

The values are empirically consistent with decimal annual rates (for example,
0.012 representing 1.2%), including plausible general-collateral and
hard-to-borrow tails. IB's public short-sale-cost page formats a raw quote of
0.75 as 75%, while the historical API table describes the OHLC fields as Fee
Rate without restating the scaling. V10 therefore freezes the shared adapter
contract as a decimal annual rate and tests that 0.012 remains 1.2% rather than
being divided a second time.

The first TRADES and ADJUSTED_LAST requests failed because IB reported a
trading session connected from a different IP address. Cluster inspection
showed that the Kubernetes native sidecar had no demand leases, no Java/Gateway
child, and had logged only its idle state since the pod started. A subsequent
three-name recheck resolved 3/3 contracts and returned 250 one-year TRADES plus
250 ADJUSTED_LAST bars for every name. The restriction was transient and is no
longer a price-entitlement blocker.

On 2026-08-12 a stopped futures launcher was found stuck in its pre-bot warmup
loop. Its lease had restarted the cluster Gateway even though bot control said
`stopped`. The orphan was terminated and the sidecar stopped Java, Xvfb, and
socat after its 45-second idle delay. The API now launches each bot in its own
process group and Stop terminates that group only when it has never advanced
the pre-launch heartbeat; a bot that has published must still flatten and exit
itself. Cluster Gateway remained idle while a temporary local read-only paper
Gateway resumed the paced full-universe fee archive.

It does not establish historical **availability** or account-specific lendable
quantity. For that gap:

- **ORTEX** is the first vendor to trial because its API exposes cost to borrow
  and short availability. Require a historical Main Market ISIN-level extract
  across all three size buckets before subscribing.
- **S&P Global Securities Finance** is the high-coverage institutional fallback.
  It advertises daily global supply, demand, fees, availability, utilization,
  and roughly twenty years of daily history. It is the preferred source if the
  ORTEX sample is incomplete or research precision justifies institutional
  pricing.
- FIS Securities Finance Market Data and EquiLend DataLend are secondary
  institutional candidates if commercial terms or Nordic coverage are better.
- Finansinspektionen's historical net-short register is free and useful as a
  lagged short-demand/squeeze feature. It contains reportable investor
  positions, not lendable inventory, so it must never be labelled or used as
  borrow availability.

Even the best securities-lending feed describes the wider lending market, not
a guarantee that IB will lend this account a requested quantity. The live
pre-trade gate remains authoritative: refresh IB shortability, available
quantity, and fee immediately before every new or increased short. Unknown or
insufficient availability produces no short order.

Until a historical availability feed passes acceptance, backtests publish
explicit 100%, 50%, and adverse Small Cap availability scenarios.
They may use observed IB fee history for cost, but may not silently treat every
negative signal as borrowable.

### 15.3 Acquisition order

1. Run the automated IB price and `FEE_RATE` coverage audit across the current
   Main Market Large, Mid, and Small Cap universe.
2. Begin immutable daily Nasdaq reference, quote, shortable quantity, and fee
   snapshots immediately; prospective history is cheap and exact for this
   account.
3. Send the same delisted-security acceptance pack to FactSet and, if available,
   WRDS/Compustat; test Börsdata only as a lower-cost supplement.
4. Send a current-and-delisted Main Market ISIN pack to ORTEX. Buy
   only if its historical availability fields and venue coverage pass.
5. Escalate to S&P Global Securities Finance only if ORTEX is incomplete or the
   expected value of higher-fidelity short research supports the cost.

The remaining open procurement choices are the benchmark licence,
point-in-time fundamentals provider for v2, and final account NAV, which
determines useful liquidity and concentration thresholds.

Acquisition checkpoint, 2026-08-10: the workspace contains no Börsdata,
FactSet, LSEG, Bloomberg, or equivalent fundamental-data entitlement. FactSet
Fundamentals Point-in-Time is the preferred trial because its published
coverage explicitly includes Sweden, active and inactive securities, and
historical values as they appeared at each point date. Börsdata Pro+ is a
lower-cost exploratory supplement: its API exposes up to 20 years of reports
and report dates, but the public contract does not establish revision-as-of
history or effective-dated instrument relations. It therefore cannot by itself
clear the point-in-time/delisting gate.

## 16. Sources and research anchors

- [IBKR TWS API documentation](https://ibkrcampus.com/campus/ibkr-api-page/twsapi-doc/):
  historical bar types, adjustment behavior, pacing, earliest timestamps, and
  unavailable historical data.
- [IBKR market data subscriptions](https://ibkrcampus.com/campus/ibkr-api-page/market-data-subscriptions/):
  subscription and API data requirements.
- [IBKR short-sale cost](https://www.interactivebrokers.com/en/pricing/short-sale-cost.php):
  annual borrow-fee calculation and decimal-to-percentage display semantics.
- [Nasdaq Nordic Market Model 2026:03](https://www.nasdaq.com/docs/2026/06/17/Nasdaq_Nordic_Market_Model_2026_03_Clean.pdf):
  Nasdaq Nordic segments, instruments, and Large/Mid/Small Cap structure.
- [Nasdaq Swedish markets](https://www.nasdaq.com/solutions/listings/markets/nordic/us-listings-in-sweden):
  distinction between Stockholm Main Market and First North Growth Market.
- [Nasdaq Nordic Reference Data Files](https://www.nasdaq.com/solutions/data/nasdaq-nordic-reference-data-files):
  current Stockholm and First North listing identifiers and reference data.
- [Skatteverket Aktiehistorik](https://www.skatteverket.se/aktiehistorik):
  Swedish listing, delisting, transfer, and corporate-action audit trail,
  including Nasdaq Stockholm and First North Sweden.
- [FactSet Prices & Returns API](https://www.factset.com/marketplace/catalog/product/factset-prices-and-returns-api):
  commercial active/inactive security prices, returns, volume, splits, and
  dividends.
- [FactSet Fundamentals Point-in-Time](https://www.factset.com/marketplace/catalog/product/factset-fundamentals-point-in-time):
  historical as-known fundamentals for active and inactive securities,
  including published Swedish coverage.
- [Börsdata API](https://borsdata.se/info/api/api_info): Nordic prices and
  company data; inactive and point-in-time coverage must be proven separately.
- [WRDS historical Compustat identifiers](https://wrds-www.wharton.upenn.edu/pages/wrds-research/database-linking-matrix/using-compustat-historical-identifier-notebook/):
  historical exchange, ticker, and primary-share identifiers.
- [ORTEX API](https://docs.ortex.com/reference/ortex-apis): global short-interest,
  cost-to-borrow, and short-availability datasets; exact Swedish venue coverage
  remains an acceptance test.
- [S&P Global Securities Finance](https://www.spglobal.com/market-intelligence/en/solutions/products/securities-finance):
  institutional historical securities-lending supply, demand, fees,
  availability, and utilization.
- [FIS Securities Finance Market Data](https://www.fisglobal.com/products/fis-securities-finance-market-data)
  and [EquiLend DataLend](https://equilend.com/wp-content/uploads/2025/04/data_magazine_digital_issuu_1.pdf):
  alternative institutional securities-lending datasets.
- [Finansinspektionen net-short register](https://www.fi.se/en/our-registers/net-short-positions/Positionsinnehavare/?navigation=open):
  current and historical reportable Swedish net-short positions; a demand proxy,
  not borrow inventory.
- [Jegadeesh and Titman (1993), “Returns to Buying Winners and Selling Losers”](https://onlinelibrary.wiley.com/doi/10.1111/j.1540-6261.1993.tb04702.x):
  medium-horizon momentum research anchor.
- [Jegadeesh (1990), “Evidence of Predictable Behavior of Security Returns”](https://onlinelibrary.wiley.com/doi/10.1111/j.1540-6261.1990.tb05110.x):
  short- and longer-lag return dependence research anchor.
- [Amihud (2002), “Illiquidity and stock returns”](https://www.sciencedirect.com/science/article/pii/S1386418101000246):
  daily absolute-return/traded-value illiquidity measure.
- [Säfvenblad (2000), “Trading volume and autocorrelation: Empirical evidence
  from the Stockholm Stock Exchange”](https://www.sciencedirect.com/science/article/abs/pii/S0378426699000710):
  Stockholm-specific return/volume research anchor.

These papers justify testing feature families; none is treated as evidence that
this implementation has a current, tradable edge.
