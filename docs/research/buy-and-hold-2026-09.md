# Buy-and-hold comparison, 2026-09-08

The selected strategy beats BTC, ETH and a fixed 50/50 BTC–ETH portfolio in
return, Sharpe and maximum daily drawdown on the saved December 17, 2023 through
July 30, 2026 sample. These are development results on previously exposed data.

| Portfolio | Total return | Annualized Sharpe | Maximum drawdown |
| --- | ---: | ---: | ---: |
| Selected strategy, zero commission and 2.2 bp half-spread | +147.7% | 1.54 | 19.1% |
| Same selection rule, original 4.5 bp commission and 0.5 bp half-spread | +102.8% | 1.23 | 19.0% |
| BTC buy and hold | +51.3% | 0.57 | 53.0% |
| ETH buy and hold | -14.3% | 0.25 | 67.6% |
| Initially 50/50 BTC–ETH, held without rebalancing | +18.5% | 0.38 | 58.6% |

Both strategy rows include their simulated funding, spread, impact and
participation limits. Changed costs also change the planner's trades, so they
are separate policy replays. The zero-commission row is a cost counterfactual;
Lighter coverage and execution remain unvalidated as explained in the
[strategy report](sharpe-improvement-2026-09.md).

Benchmarks buy spot once at the first timestamp and hold fixed quantities.
They exclude one-off entry/exit fees and staking income and incur no perpetual
funding. The 50/50 portfolio's weights drift after purchase. All marks use the
same local daily UTC opens as the strategy's central replay. The first benchmark
return is zero; subsequent returns include every calendar boundary. The stored
BTC opens are $42,278.02 initially and $63,984.29 finally; ETH opens are $2,228.96
and $1,910.72.

The strategy series joins four walk-forward folds, each initialized with
$100,000. Its total return compounds those reported daily returns, rather than
claiming an uninterrupted deployed account. Benchmark fold files only rescale
NAV for the comparison engine: holdings are continuous and there are no
fold-boundary liquidations, cash periods or rebalances. Export checks verify
that the compounded segmented benchmark return equals its endpoint price ratio.

All 957 daily observations and fold boundaries match. The existing Monte Carlo
engine calculates every benchmark's Sharpe and drawdown, using 365 annual
periods, arithmetic daily returns and sample standard deviation. Sharpe uses
zero as the risk-free return. Positive arithmetic-return Sharpe can coexist
with negative compounded return, as it does for ETH in this volatile sample.

With 10,000 paired paths, seed 42 and 20-day blocks, the selected candidate's
Sharpe advantage over BTC has a 95% percentile interval of -0.55 to +2.49.
The observed advantage is substantial, but this interval does not establish
reliable future outperformance. No adjustment for prior strategy selection is
applied.

Run the comparison against a fresh output directory:

```sh
bin/compare-buy-hold.sh \
  var/research/sharpe-target-r4-confirm-score-low-fee/score-low-fee-impact \
  var/research/buy-hold-rerun
```

Final artifacts are under `var/research/buy-hold-2026-09-final`: daily benchmark
NAVs, input bar hashes, source replay hashes, source/executable SHA256 values,
endpoint parity and three paired Monte Carlo reports. The original-fee strategy
row comes from `var/research/sharpe-target-r2-confirm-score-selected/monte-carlo/score-selected-impact-block20.json`.
