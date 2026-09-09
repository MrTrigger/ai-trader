# Selection and later-date data audit, 2026-09-08

The 1.5405 candidate has strong gross short P&L, weak long P&L, and a useful
negative correlation between the two sides. This supports testing selection
within balanced sleeves before assuming a larger short allocation will help.
The current Lighter membership removes substantial historical contributors.
A static membership sensitivity is necessary before interpreting the candidate
as a venue-specific strategy.

## Evidence and method

Source: `var/research/sharpe-target-r4-confirm-score-low-fee/score-low-fee-impact/fold-{1,2,3,4}.json`.
These are original walk-forward folds 6–9, 2023-12-17 through 2026-07-30,
with separate $100,000 initial capital per fold, zero commission, 2.2bp
half-spread and the existing volume/impact/funding proxies. The saved verdict
reports pooled daily Sharpe 1.540501997536627 and explicitly disclaims
independent validation.

**Calculation disclosure:** this audit used ad hoc Python arithmetic, not a
committed Rust attribution command. Temporary Rust executables read actual
Parquet bars and funding through the existing `crypto_portfolio::store` and
`crypto_portfolio::funding` loaders. Python reconstructed signed positions from
saved fills, marked prior positions at each daily open, applied the previous
day's realized funding to current open notional, and deducted saved commission,
spread and impact costs per asset. Missing daily prices settled the position at
the previous mark, matching the existing replay. The scripts were executed in
the tool transcript; no Python financial-math script was added to the repo.
Treat asset-level figures as exploratory diagnostics until a Rust-owned audit
reproduces them. The rerunnable existing Rust evidence is the saved fold report's
side/cost attribution, computed in `service/crates/crypto-portfolio/src/backtest.rs`.

Maximum per-day reconstruction difference versus saved Rust attribution was
less than $9e-12 for price P&L and $5e-13 for funding. Dollar sums across folds
sum separately reset books; they are not one compounded account return.
Component Sharpe/correlation below divides each day's P&L by that day's saved
`attribution.nav_before`, including initial execution costs in total-book metrics.
Side metrics omit side-specific transaction/funding costs and are not standalone
strategy results.

| Fold | Return | Sharpe | Long price P&L | Short price P&L | Funding P&L | Spread + impact |
|---|---:|---:|---:|---:|---:|---:|
| 1 | 11.96% | 0.882 | $810 | $16,698 | -$48 | $5,499 |
| 2 | 30.70% | 2.012 | $6,339 | $30,765 | -$695 | $5,706 |
| 3 | 48.60% | 2.685 | $21,853 | $37,455 | -$4,803 | $5,906 |
| 4 | 13.92% | 0.819 | -$16,493 | $53,502 | -$17,760 | $5,328 |

Daily gross long/short component correlation is -0.771. Their component
Sharpes are 0.234 and 1.259, respectively. The gross short component Sharpe
being below the combined net Sharpe is one reason not to infer that removing
longs improves the portfolio. Average realized gross exposure by fold is
0.793, 0.784, 0.789, 0.760; average net exposure is 0.001, 0.008, 0.004, 0.013.
Turnover-budget warnings occur on 23, 31, 18 and 35 days respectively.

Largest pooled net contributors after saved costs and reconstructed funding:
XRP +$15,221, DASH +$13,823, SYN +$13,068, OM +$11,560, AUCTION +$11,068.
Largest detractors: ZEC -$10,800, DEXE -$9,963, HBAR -$8,691, ARB -$6,286,
FET -$5,106. Final-fold funding is concentrated in DEXE -$7,818,
KITE -$3,010, ENSO -$2,975 and AXS -$2,795. These names explain outcomes;
they are not proposed retrospective exclusion lists.

Exact active native symbols from
`var/research/sharpe-target-probes/lighter/orderBookDetails-2026-09-08.json`
exclude assets accounting for $87,743 of $150,928 pooled gross price P&L
(58.1%). This does not predict a filtered replay: replacement selections,
weights, turnover and funding all change. A September membership snapshot
cannot establish historical listing eligibility. No aliases such as PEPE to
1000PEPE are economically interchangeable here.

## At most two selection hypotheses

1. Select up to 12 names independently per side by the existing per-risk
   score, retaining the existing 50/50 gross budgets, floors and caps. Global
   top-24 selection can crowd out one side while that side still receives
   half the capital. This tests sleeve concentration without a timing forecast.
2. Keep only the global top 16 scores, preserving all other construction and
   execution settings. This tests whether weaker selected ranks dilute the
   strongest relative forecasts. More concentration and cap-driven gross
   reduction are part of the mechanism and must be reported.

These match the parent's declared per-side and select16 arms; they are not
additional experiments. Wider 40-name selection already failed to improve
on score-selected 24. Adverse-funding conviction subtraction was already
screened and failed; the funding concentration does not turn it into a new
hypothesis. All four folds have been repeatedly reused, so neither selection
nor confirmation on these dates is independent evidence. Do not optimize
side weights or asset exclusions from this attribution.

## Local coverage and frozen-model boundary

- Actual daily Parquet rows extend through **2026-08-08** (476 assets on that
  date). Daily manifests end August 6 and understate actual coverage.
- Actual hourly BTC, ETH, SOL, AAVE and XRP end **2026-08-06 23:00 UTC**.
  All hourly manifests share that maximum; later daily bars do not supply
  the missing intraday features.
- Funding has 442 assets. 361 reach **2026-08-04**, including BTC, ETH, SOL,
  XRP and AAVE. Other assets end earlier. No August 5–7 funding exists in
  the local table. `funding_carry` silently skips absent rates, so extending
  replay to August 8 would omit those payments.
- Universe snapshots extend through August 9, with August 8 absent. The
  August 7 snapshot has 25 eligible names; all 25 match exact active native
  Lighter symbols in the September 8 snapshot and have daily bars August 1–8.
- Frozen `data/models/ranker-rank-1.json` has feature set
  `fs-rust-crypto-4`, reward `per_risk`, `trained_through: 2026-08-01`, and
  `trained_at: 2026-08-09T09:53:49.529614+00:00`. Its SHA256 is
  `6b11e46f02680e2325078ae448e159c3b50de75a3e7364cd67b7465422fed3ca`.
- Selecting training-matrix rows through August 1 exactly matches its 74,089
  rows and 2,365 dates. There are 33 August 1 target rows. The legacy one-hour
  entry lag plus 24-hour target uses August 2 01:00 prices. Therefore August 3
  is the earliest daily decision beyond those training labels, even though
  the legacy runtime date check alone permits August 2.
- August 3–5 gives only three daily decision steps, two held return intervals,
  before missing funding starts. This is an operational diagnostic, not a
  meaningful Sharpe test. No local bars occur after the artifact's August 9
  creation, so a true subsequent forward test requires refreshed data.

Existing timing tools are `crypto-portfolio directional-matrix` and
`training/train_directional.py`. The trainer uses fixed 300 trees/8 leaves,
labels `fwd_z_7`, `fwd_z_14` or `fwd_z_30`, minimum 400 rows, and a horizon+2-day
purge. `data/models/market-matrix.jsonl` contains 3,249 rows through August 8.
It does not create a later price holdout. Raw BTC/ETH open-interest metric
files and DVOL/stablecoin data extend through August 20, but cannot substitute
for missing portfolio price/funding history. The new low-capacity timing arm
is the parent's separate implementation, not the old trainer's default.

## Prepared membership sensitivity

`bin/prepare-lighter-replay.sh` creates
`var/research/selection-trend-lighter-data` with shared bars/funding symlinks
and all 3,356 original universe snapshots filtered to exact active native
September 8 symbols. It preserves snapshot metadata, eligibility flags and
ranks on retained members. It records membership source/hash and input/output
snapshot hashes. It makes no historical listing claim and changes no source
snapshot. Run twice to verify unchanged output; different inputs fail rather
than overwrite existing evidence.
