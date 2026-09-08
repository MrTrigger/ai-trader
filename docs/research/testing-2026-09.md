# Strategy testing results, 2026-09-08

Keep the crypto ranker as a research candidate. Its recent results weaken under
execution stress, and the available NQ overlap does not support adding a 50/50
allocation. Monte Carlo is now part of the repeatable test suite. No strategy
was promoted or deployed by this work.

## What changed

The Rust replay now models spread, size-dependent impact, partial fills and
execution delay. Every daily NAV change reconciles to long and short price P&L,
funding, commission, spread, impact and delay P&L. It includes first-day costs,
rejects missing decision dates and records unfilled notional. Historical runs
cannot use a current liquidity snapshot as point-in-time evidence.
The shared replay defaults to the impact scenario, including callers such as
validation and gates. Use an explicit `backtest --execution` settings file for
the flat control; older commands can therefore produce different results.

The experiment register contains 11 historical records and six execution
scenarios. It separates invalid, superseded, rejected and development evidence,
records overlapping evaluation dates and leaves unknown trial counts unknown.
New outputs link to hashed data, models, configurations and executable inputs;
completion checks reject truncated or altered cached reports. Legacy reports
retain their provenance limitations.

The new Monte Carlo command uses paired circular blocks of daily returns,
deterministic seeds and explicit source hashes. It reports historical Sharpe
uncertainty separately from one-year return and drawdown scenarios. See the
[method](monte-carlo-method.md) and [register contract](experiment-register.md).

## Frozen crypto replays

The [declared suite](execution-suite.json) replays the same nine saved ranker
models across 2,142 days, September 18, 2020 through July 30, 2026. The recent
subset contains the last four folds, 957 days starting December 17, 2023.
These evaluation dates were already exposed during research.

All scenarios retain the configured 4.5 bp commission. The flat control uses
0.5 bp half-spread and unlimited fills. The impact scenarios cap each order at
1% of estimated hourly dollar volume and add square-root impact. Hourly volume
is prior closed daily ADV divided by 24; hourly volatility is annualized
volatility divided by the square root of 8,760. The impact coefficient is 0.1.
Those are research assumptions, not measured venue execution parameters.

| Scenario | Starting cash per fold | Full pooled Sharpe | Recent pooled Sharpe | Latest fold return |
| --- | ---: | ---: | ---: | ---: |
| Flat control | $100,000 | 2.04 | 1.27 | +14.08% |
| Impact and participation cap | $100,000 | 2.00 | 1.13 | +8.09% |
| Stress: 5 bp half-spread, 4x impact | $100,000 | 1.19 | 0.42 | -3.42% |
| Impact plus one-hour delay | $100,000 | 1.45 | 0.91 | +4.84% |
| Capacity sensitivity | $300,000 | 2.22 | 1.31 | +19.63% |
| Small account sensitivity | $30,000 | 2.00 | 1.22 | +12.62% |

Partial fills change subsequent holdings and exposures. The better $300,000
result therefore does not establish greater capacity or justify increasing
capital. A future capacity study must explain which orders are omitted and
validate the volume proxy. Delayed execution uses the later hourly open and
books signed execution shortfall against the decision-time mark. It is not an
order-book simulation.

The impact scenario's mean fold Sharpe is 2.08; its pooled daily Sharpe is 2.00.
The table consistently uses the latter. Each fold resets capital and positions;
the combined history omits any unrecorded liquidation or model-transition cost.

## Monte Carlo results

Each report uses 10,000 paths and seed 42. This table uses the recent four folds
and 20-day blocks. The Sharpe interval resamples 957 observations. Loss and
drawdown statistics describe separate 365-day simulated paths.

| Scenario | Historical Sharpe, 95% percentile interval | One-year loss frequency | Drawdown above 25% frequency | 95th percentile drawdown |
| --- | ---: | ---: | ---: | ---: |
| Flat control | 0.35 to 2.22 | 5.8% | 2.7% | 22.8% |
| Impact | 0.24 to 2.09 | 8.0% | 3.3% | 23.7% |
| Stress | -0.52 to 1.33 | 34.4% | 15.2% | 30.3% |
| One-hour delay | -0.04 to 1.88 | 15.0% | 6.9% | 26.1% |
| $300,000 sensitivity | 0.35 to 2.30 | 6.7% | 1.5% | 21.3% |
| $30,000 sensitivity | 0.31 to 2.17 | 6.6% | 3.0% | 23.1% |

Block length matters. For the recent impact scenario, 5-, 20- and 60-day blocks
produce loss frequencies of 12.9%, 8.0% and 5.3%, and 95th percentile drawdowns
of 28.9%, 23.7% and 18.5%. The corresponding historical Sharpe intervals are
0.09 to 2.30, 0.24 to 2.09 and 0.38 to 1.86. All three settings are retained.

Using the full history with 20-day blocks produces a 2.0% loss frequency for
the impact scenario, much lower than the recent 8.0%. Earlier strong regimes
make the full-history distribution more favorable. These frequencies are
conditional on recorded returns and execution assumptions. Resampling neither
creates an untouched holdout nor accounts for unknown past model searches.

## What explains the recent weakness

Daily attribution reconciles in all 54 replays. The following impact-scenario
amounts are thousands of dollars, rounded, with $100,000 reset at each fold.

| Fold | Long price P&L | Short price P&L | Funding P&L | Commission | Spread and impact | Net P&L |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 6 | +0.04 | +11.95 | -0.05 | 8.89 | 1.97 | +1.08 |
| 7 | +5.78 | +30.33 | -0.77 | 9.40 | 2.14 | +23.81 |
| 8 | +22.82 | +34.37 | -4.61 | 9.44 | 2.29 | +40.85 |
| 9 | -16.74 | +52.15 | -16.96 | 8.19 | 2.17 | +8.09 |

Fold 6's price profits barely cover trading costs. In fold 9, long losses and
funding consume $33.71k despite profitable shorts. Commission remains the
largest recorded trading cost. Fold 9 also records $1.29m of cumulative unfilled
requested notional. This sum can count repeated requests; it is not a loss or
a unique blocked position.

Next, measure forecast quality separately for longs and shorts, identify the
assets and dates contributing funding losses, and compare expected benefit
with incremental turnover cost. The current attribution establishes where
money went. It does not prove whether forecast decay, market regime or a few
instruments caused it. Any resulting feature or policy change needs a declared
hypothesis and a separate evaluation window.

## Other strategies and diversification

The standalone directional V6 and R3 candidates remain rejected under their
existing benchmark gate. Pairing the impact ranker with legacy V6 yields daily
correlation 0.067 and a fixed 50/50 blend Sharpe of 2.16 versus 2.00 for the
ranker. V6 still uses its legacy execution assumptions. The 95% paired interval
for ranker-minus-V6 Sharpe is -0.02 to 2.06. This comparison warrants a matched
execution replay, not an allocation change or an optimized blend search.

The new read-only NQ export exactly reproduces the committed 156-trade parity
fixture, including $12,503 net P&L. The wider local overlap has 150 calendar
days, March 3 through July 30, 2026, and 311 completed trades. NQ session P&L is
stamped at the following UTC midnight to align return intervals with crypto.

| Observed overlap | Return | Maximum daily drawdown | Sharpe |
| --- | ---: | ---: | ---: |
| Impact crypto ranker | +8.29% | 18.27% | 0.76 |
| Four NQ sleeves | -22.89% | 48.13% | 0.22 |
| Daily rebalanced 50/50 blend | +0.48% | 27.40% | 0.37 |

Daily correlation is -0.035, but the blend underperforms the crypto ranker in
this sample. NQ uses one full NQ contract per active sleeve and an assumed
$100,000 capital denominator. Its positive arithmetic Sharpe coexists with a
compounded loss. This is not a risk-matched allocation comparison. The NQ export
also has two retroactive early-session closes; exchange calendars, intraday
margin and impact remain unverified. The longer 2021–2026 five-minute archive
needs a raw-bar adapter before it can reproduce the external research ledger.

Scalper runs 1–3 remain invalid; clean runs 4–6 remain rejected. Stockholm remains
blocked on historical listing/price validation, filing-dated quarterly coverage
and historical short availability. The licensed EODHD paths could not run
because no token is available. The [data inventory](data-gaps.md) records the
existing collections and the remaining work.

## Rerun and verification

From the repository root, with the saved data, models and sibling
`../trading-journal` checkout available:

```sh
bin/test-strategies.sh
```

The script builds the Rust tools, runs or verifies all 54 declared replays,
creates the experiment register, runs full and recent Monte Carlo with 5-, 20-
and 60-day blocks, runs paired comparisons, and checks an identical-seed report
byte for byte. The NQ section runs when its local bars and roll manifest exist;
otherwise it prints that the comparison is unavailable. `--reports-only` skips
the crypto build and replay, using an existing release binary and saved folds.

Artifacts are in `var/research/testing-2026-09`: `inputs.json`, `register.json`,
six scenario directories and `monte-carlo/`. NQ exports and raw-input hashes are
in `var/research/nq-testing-2026-09`. Raw artifacts stay local. If source data,
models, execution settings or the crypto executable change, use a new manifest
and output directory through `STRATEGY_TEST_SUITE`; the suite refuses to mix
inputs with an existing run. Registration checks saved suite linkage but does
not rehash raw data or the historical executable. A full suite invocation does.

Verification passed 90 crypto tests, 8 futures tests and 73 causal feature tests,
plus the workspace check, daily attribution checks, exact NQ fixture parity and
deterministic Monte Carlo comparison. The existing selection-pool changes were
preserved. No live order path or account was exercised.
