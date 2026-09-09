# Corrected Lighter historical baseline

The earlier test setup did not match the Lighter strategy. The broad-universe
Sharpe of 1.54 and the later static-membership sensitivity of 0.516 were not
valid baselines for the deployed Lighter policy. This correction rebuilds
historical eligibility before ranking and uses native execution inputs.

The corrected fixed-model replay loses money over November 27, 2025 through
July 30, 2026. With native Lighter prices and hourly funding, its central
execution scenario has Sharpe **-1.4108**, return **-10.94%**, and maximum
drawdown **17.53%**. This is a 246-day development replay with modeled execution,
not measured live performance or an independent holdout.

## What changed in the test

The live strategy filters the available venue markets before ranking the top 30
by trailing external-market liquidity. The previous sensitivity filtered an
already ranked historical top-40 universe, preventing replacement candidates
from entering. It also applied today's active listings to dates before many of
those contracts existed.

The corrected dataset includes all 233 perpetual contracts in the native
metadata catalog, including 17 currently inactive contracts. Each decision
requires a contract creation timestamp before that decision and positive native
base and quote volume in the prior completed UTC day. Current status and future
candles never establish past eligibility. The existing liquidity ranker then
selects up to 30 names from that set, using only external bars before the decision.

This produces 246 daily snapshots, with 27–30 eligible assets per day and 45
different selected assets. The history gate is a conservative activity screen.
It does not reconstruct a complete listing, halt, or settlement event ledger.
Currently inactive contracts were considered, although none entered the selected
union during this particular period.

The strategy uses the deployed Lighter configuration, including its 0.10 net
exposure limit, 0.80 gross target, up to 24 selected positions, 0.50 turnover budget,
and 90-day external history requirement. Starting capital is $100,000. The model
is the frozen original fold-9 ranker, trained through November 25, 2025. The
August 2026 deployed model would leak information into this earlier period and
is therefore not used. No model was retrained or tuned during this correction.

Signal features retain the external price and funding history used by the live
strategy. Execution is separate: native Lighter daily opening prices value the
book and fill orders, native hourly funding payments charge held quantities, and
trailing native volume and volatility determine the execution liquidity proxy.

## Matched results

Both rows below use the same corrected historical universe, dates, frozen model,
portfolio rules, capital, and execution settings. Only the sources for execution
prices, funding, and liquidity differ. Neither is directly comparable with the
old 957-day pooled Sharpe without matching the observation window.

| Execution inputs | Sharpe | Return |
|---|---:|---:|
| External price, funding, and liquidity proxies | -0.2907 | -4.13% |
| Native Lighter prices, funding, and trailing liquidity | -1.4108 | -10.94% |

The central scenario assumes zero commission, 2.2 basis points half-spread,
impact coefficient 0.1, and participation capped at 1% of estimated hourly
volume. The hourly volume estimate is trailing native daily ADV divided by 24.
It is not measured historical order-book depth or the realized opening-hour
volume. Positive execution-day volume does not change that capacity estimate.
A synthetic daily candle with zero trades cannot support an executable open;
that is an ex-post execution-availability check, not a signal input.

Severe stress retains the same native data and increases half-spread to 22 basis
points and impact fourfold. Sharpe falls to -3.1190 and return to -22.53%.
Delayed native execution is not tested because the collected native execution
prices are daily. The implementation refuses to substitute external hourly
prices for such a request.

The native central replay's reconciled dollar attribution is:

| Component | P&L or cost |
|---|---:|
| Long price P&L | -$12,450.58 |
| Short price P&L | +$3,486.70 |
| Funding P&L | -$345.86 |
| Spread cost | $1,252.05 |
| Impact cost | $378.22 |
| Commission | $0 |

The native execution scenario leaves $9.48 million of requested notional
unfilled across the period, compared with $1.29 million using external liquidity
proxies. These are cumulative unfilled requests, not capital lost. The changed
fills alter the portfolio path. This comparison changes prices, funding, and
liquidity together, so it does not identify a separate causal effect for each.
Funding is a small part of the native loss; zero trading fees do not establish
that the requested portfolio can be implemented under the assumed capacity cap.

The 20-day circular block bootstrap uses 10,000 samples and seed 42. Its native
central 95% Sharpe interval is [-3.7117, +1.1264]. The sample is short, previously
exposed, and conditional on the model and execution assumptions. It does not
support a Sharpe-above-1.5 claim.

## Native data and checks

The public collection contains 274 daily-candle pages and 364 hourly-funding
pages. Funding covers 45 selected assets with 238,056 unique events and no
conflicting duplicate events. Every hour while the replay holds a position must
have an explicit funding event. Missing native valuation or funding fails the
replay instead of triggering another-venue or zero-payment substitution.

Lighter's `fundings` endpoint at `resolution=1d` subsamples hourly events. It does
not sum daily funding. The implementation reads `resolution=1h` and uses the
exact decimal `value` per native base unit. Cash changes by `-quantity * value`
when longs pay, and `+quantity * value` when shorts pay, over the interval after
the previous decision through the current decision. The previous portfolio
receives those charges before the new fills. The raw probes and Rust audit
record the timestamp, sign, and scale checks. See the official
[candle API](https://apidocs.lighter.xyz/reference/candles),
[funding API](https://apidocs.lighter.xyz/reference/fundings), and
[funding documentation](https://docs.lighter.xyz/trading/funding).

One AAVE daily candle, June 19, 2025, reports a close below its low. The loader
quarantines it with its raw source references; it changes no price. The date is
outside both this evaluation and its 30-day native liquidity warmup. A quarantine
inside either interval fails validation. BIO's 21 missing funding hours are in
download padding before its first eligible decision and do not enter held-hour
accounting.

All 117 Rust tests pass. They include pre-listing exclusion, historical inclusion
of inactive contracts, filter-before-rank replacement, future-data invariance,
quarantine enforcement, exact funding cash flow, and missing-hour refusal.
Changing positive current-day volume cannot change prior-day execution capacity.
The root executable independently regenerated all 246 universe memberships and
matched the original generation. All replay steps reconcile, and the regenerated
experiment register matches the saved result. Independent review found no
remaining blocking correctness issue.

This replay does not reconstruct historical depth, intra-hour fills, funding
rebates, or settlement events. Spread and impact remain explicit assumptions.
The next optimization must start from this venue-aware baseline and account for
which selected positions can actually fill. Neither the prior 1.54 result nor
this correction justifies promoting a new policy. The paper policy was not
changed during this work.

## Reproduction and artifacts

The public download scripts use no account credentials. Preserve existing raw
responses, configurations, and results; use new output paths for new datasets.
`bin/pull-lighter-daily.sh` accepts a saved `orderBooks` response, an output
directory, and an exclusive UTC end date. `bin/pull-lighter-funding.sh` accepts
the generated `selected-markets.json` and an output directory; its window is
fixed to this November 2025–July 2026 experiment. Run the collectors sequentially.
Both preserve raw attempts, resume verified pages, and withhold completion after
a failed or corrupted page. Offline resume and failure checks pass.

```bash
cargo build --release --manifest-path service/Cargo.toml \
  -p crypto-portfolio --bin crypto-portfolio --example lighter-history
service/target/release/examples/lighter-history \
  var/research/lighter-history-audit/daily-2026-09-08 data \
  var/research/lighter-native-universe deploy/lighter-config.toml \
  2025-11-27T00:00:00Z 2026-07-30T00:00:00Z
bin/replay-lighter-native.sh proxy
bin/replay-lighter-native.sh native
```

The universe command refuses to overwrite an existing generation. The completed
generation and its raw-input hashes are in
`var/research/lighter-native-universe/coverage.json`. Native public input evidence
is under `var/research/lighter-history-audit/`. Results and Monte Carlo outputs
are under `var/research/lighter-native-baseline-{proxy,native}/`.
The decision trail is `.audit/lighter-history.tsv`.
