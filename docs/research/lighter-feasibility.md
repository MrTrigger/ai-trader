# Lighter feasibility, 2026-09-08

Lighter Standard is a plausible fee-saving venue for ordinary limit and market
orders. It is not yet a validated replacement for the ranker's present market
universe or execution assumptions.

## Fees, latency and API limits

The current official API account table lists Standard maker/taker fees of zero,
300 ms taker latency, 300 ms cancel/modify latency and zero maker latency. The
consumer fee page still lists 200 ms maker latency, so the two official pages
are inconsistent. Use the API-specific contract provisionally and verify order
timing before integration. Neither figure measures network delay or queue
execution. [API account types](https://apidocs.lighter.xyz/docs/account-types),
[consumer fee page](https://docs.lighter.xyz/trading/trading-fees).

The official API announcement adds a **1 bp fee for Standard RFQ, Chase Limit
and TWAP orders**. Ordinary limit and market orders remain free. A zero-fee
counterfactual therefore cannot assume exchange-native TWAP is free.
[Official API updates](https://t.me/s/lighter_api_updates).

Standard is limited to 60 REST requests per rolling minute, including
`sendTx` and `sendTxBatch`; the documented default transaction-type limit is
40 requests per minute. Limits apply to IP and L1 address. Standard permits
250 active orders per account and 30 per market, with 50 pending orders per
account and 10 per market. WebSocket limits include 500 subscriptions per
connection and 200 client messages per minute. These limits look compatible
with a paced daily rebalance, but that is an inference, not a tested adapter.
[Rate limits](https://apidocs.lighter.xyz/docs/rate-limits).

## Current market coverage

Public unauthenticated snapshots returned 244 order books, including 216 active
perpetual markets. `orderBooks` supplies symbols, market IDs, size/price precision,
minimum order amounts, market status and creation timestamps.
[Order books API](https://apidocs.lighter.xyz/reference/orderbooks).

A corrected Rust audit accumulated signed fills and recorded settlement closes
from the original policy's impact replay through **2026-07-30**, yielding 25
nonzero holdings. This is a saved research portfolio, not a live account query.
Nineteen match current active perpetual symbols exactly. Six do not:

- ALLO, DEXE, OPN and SYN have no exact active perpetual match.
- PEPE has a candidate `1000PEPE` contract. Its quantity and price conversion
  requires an explicit instrument mapping before counting it as supported.
- XAUT has an active `XAUT/USDC` spot market. The `XAU` perpetual is a different
  instrument and is not a substitute established by this audit.

Across recent folds 6–9, the replay traded 146 distinct asset symbols; 89 lack
an exact match in today's active perpetual set. This is a current symbol screen,
not historical listing coverage. Delisted names and ticker aliases need separate
identity work. Today's supported markets cannot be inserted into earlier
backtests as though they existed then.

## Depth, funding and history

The public `orderBookOrders?market_id=0&limit=20` request succeeded for ETH.
The saved snapshot contains 20 ask orders and 20 bid orders, with best ask
2482.64 and best bid 2482.63. These are individual orders, not necessarily 20
price levels. A single snapshot does not establish tradable size or historical
impact. [Depth API](https://apidocs.lighter.xyz/reference/orderbookorders).

`funding-rates` returned 216 Lighter market rows alongside other exchanges.
The historical ETH `fundings` request also succeeded without authentication,
returning 23 hourly records for the requested September 7 window. Pagination,
endpoint boundary semantics and rate units still need validation. The endpoint
supports hourly/daily resolution and at most 750 records per call.
[Funding history API](https://apidocs.lighter.xyz/reference/fundings).
Lighter settles funding hourly between counterparties, so zero trading fees do
not eliminate funding costs. [Funding mechanism](https://docs.lighter.xyz/trading/funding).

The documented global trade-Parquet archive extends back to mainnet genesis on
January 17, 2025. Access requires a one-time **100 LIT in-app L2 transfer**, not
an ordinary on-chain transfer. `/export/historicalTrades` accepts authentication
and returns a one-hour presigned URL; it updates daily at 20:00 UTC. This is a
paid, authenticated history route, not an unrestricted public download. No
transfer or authenticated request was made. It cannot supply Lighter execution
history for the ranker's pre-2025 folds.
[Historical data](https://apidocs.lighter.xyz/docs/historical-data),
[export endpoint](https://apidocs.lighter.xyz/reference/export_historicaltrades).

Raw responses, API reference excerpts and the reproducible Rust coverage audit
are saved under `var/research/sharpe-target-probes/lighter/`. The zero-commission
replay remains a fee counterfactual on the existing data. Actual Lighter
validation needs mapped instruments, listing dates, venue-specific funding,
spread/depth samples and fills under Standard-account restrictions.

## Current $3,000 depth check

A second read-only capture queried the original policy's 19 exact-matched perpetual markets from
11:03:47 through 11:04:14 UTC on September 8. Requests were paced at least one
second apart, with an eight-second timeout. All succeeded. Each response contains
up to 100 individual resting orders per side.

The Rust probe requests base quantity equal to $3,000 divided by each snapshot's
midpoint. It sorts asks ascending and bids descending, walks remaining order
quantities, and measures quoted VWAP shortfall from that midpoint. All 38
buy/sell quotes had enough displayed depth; the probe would report incomplete
depth instead of extrapolating a full-order price.

| Quoted cost, basis points | Median | Maximum |
| --- | ---: | ---: |
| Half-spread, both directions | 2.165 | 6.207 |
| Additional book impact, both directions | 0.453 | 7.232 |
| Total shortfall, buys | 2.807 | 9.549 |
| Total shortfall, sells | 2.829 | 7.801 |
| Total shortfall, all 38 quotes | 2.818 | 9.549 |

WLD's buy quote had the largest total shortfall, 9.549 bp; XPL's sell quote
reached 7.801 bp. These are separate marginal summaries, so their medians need
not add. The median quoted cost is smaller than the existing 4.5 bp commission
alone, supporting further venue testing. The tail is material, and this sample
also shows that a 0.5 bp half-spread is not a measured Lighter-wide assumption.

These sequential snapshots are not atomic across markets. Resting liquidity
can disappear before an order reaches the matcher; the calculation excludes
latency, fees, funding, order-size rounding and minimum-order constraints.
The sample is conditional on current support for a saved July portfolio. It is
neither a historical fill calibration nor an executable order proposal.

Evidence is local under `var/research/sharpe-target-probes/lighter/`:
`depth-2026-09-08/` holds raw responses and request timestamps;
`depth-summary-2026-09-08.json` holds every quote; `depth_probe.rs` is the
rerunnable Decimal-based calculation. Its three checks passed for price sorting,
buy/sell arithmetic and incomplete depth. Run `run-depth-probe.sh` from the
repository root to rebuild against the existing release dependencies and
recalculate the saved snapshots. The API source is
[orderBookOrders](https://apidocs.lighter.xyz/reference/orderbookorders).

## Replay with the observed spread scale

A further diagnostic set commission to zero and the constant half-spread to
**2.2 bp**, rounding the current depth sample's 2.165 bp median. The frozen
ranker models, historical universe, Binance funding proxy and existing impact
and participation assumptions remain in use. This is a cost scenario motivated
by current quotes, not a Lighter historical backtest. Changing config costs also
changes the strategy's cost floor and deadband, so the replay recomputes trades.

Across the same 957 exposed days, December 17, 2023 through July 30, 2026:

| Scenario | Pooled daily Sharpe |
| --- | ---: |
| Existing 4.5 bp commission, 0.5 bp half-spread | 1.132 |
| Zero commission, 0.5 bp half-spread | 1.647 |
| Zero commission, 2.2 bp half-spread | **1.459** |

The quoted-spread scenario returns 134.49% across reset folds, with 18.35%
maximum daily drawdown. Its latest fold returns 13.98%. The 20-day block
bootstrap gives a 95% Sharpe interval of 0.579–2.435. The paired Sharpe change
versus the existing-cost control is +0.327, interval +0.251 to +0.412; versus
the optimistic zero-fee scenario it is -0.188, interval -0.238 to -0.143.

The fee advantage survives this spread assumption, but the point estimate falls
below the 1.5 target. These intervals describe reused historical returns;
they do not account for all prior model and policy searches or venue mismatch.

All reports use 10,000 simulations and seed 42. Block sensitivity remains visible:

| Block length | Historical Sharpe, 95% interval | Simulated one-year loss | Simulated drawdown, 95th percentile |
| --- | ---: | ---: | ---: |
| 5 days | 0.395–2.659 | 6.77% | 26.85% |
| 20 days | 0.579–2.435 | 2.79% | 21.53% |
| 60 days | 0.743–2.189 | 1.05% | 18.35% |

The declared suite is `docs/research/lighter-quoted-spread-suite.json`.
`var/research/lighter-quoted-cost-replay/` contains the linked input inventory,
experiment register, four reconciled replay folds and standalone/paired Monte
Carlo reports. The register includes hashes of the suite, its `inputs.json`
and the quoted-depth summary.

Run `bin/replay-lighter-quoted-cost.sh --reports-only` to verify/reproduce these
reports with the existing binary. Without that flag, the script regenerates
the four configs and declaration, then runs or verifies the replay before
reporting. It does not rebuild the binary. Existing config/report files must
match exactly and an existing suite declaration must match semantically;
changed evidence causes an error rather than replacement. For a new build or
new inputs, use a fresh prefix, for example:

```sh
LIGHTER_QUOTED_PREFIX=var/research/lighter-quoted-cost-next bin/replay-lighter-quoted-cost.sh
```

Verification passed shell syntax, byte comparisons of all four
generated configs against the completed run, and the real reports-only path.
The repeated register and all five Monte Carlo reports matched the saved files
byte for byte. ShellCheck was unavailable. No live venue orders or
historical-data purchases were made.

## Selected candidate coverage and a correction

The round-4 `score-low-fee-impact` candidate's saved July 30 portfolio also has
25 holdings, of which 19 match the September 8 active-perpetual snapshot exactly.
Matched holdings represent **64.55% of gross notional**, $54,818.83 of $84,925.34.
The six unmatched symbols are ALLO, DEXE, OPN, PEPE, SYN and XAUT. PEPE's possible
`1000PEPE` conversion is excluded pending instrument validation. BNB and TRX
enter the candidate's matched set in place of NEAR and XPL; the earlier depth
sample therefore covers 17 of its 19 matched symbols, not all 19.

The initial fill-only audit overstated both portfolio counts as 26. TON was
closed by the replay's last-price settlement on July 1, but that cash/position
adjustment is disclosed separately from ordinary fills. The corrected audit
subtracts the signed settlement quantities. The original-policy count is also
19 exact matches out of 25; the market snapshot and 19-market depth capture
are unchanged.

For the candidate, all 25 remaining holdings have reference prices in the final
decision's fills. Their reconstructed gross notional matches saved NAV times
gross exposure exactly. Those historical reference prices weight the current
symbol-coverage comparison; they are not current Lighter prices. The original
policy lacks final-decision reference marks for DEXE, SYN and XRP, so the audit
does not claim a notional-weighted coverage figure for it.

Separate artifacts preserve the correction:
`r4-candidate-coverage-2026-09-08.json`,
`original-policy-coverage-corrected-2026-09-08.json`,
`candidate_coverage.rs` and `R4-COVERAGE-SHA256SUMS`, under
`var/research/sharpe-target-probes/lighter/`. The earlier
`coverage-2026-09-08.json` remains as superseded fill-only evidence. No new
market requests were needed, and current coverage establishes no historical
tradability on Lighter.
