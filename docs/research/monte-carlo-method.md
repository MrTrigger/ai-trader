# Monte Carlo tests for saved crypto replays

`crypto-portfolio monte-carlo` resamples consecutive daily returns from saved
walk-forward folds. The report separates uncertainty about historical Sharpe
from the range of return and drawdown paths over a chosen horizon.

This method retains local loss clusters and volatility clusters that independent
daily shuffling destroys. Block resampling assumes dependence weakens with time.
The [CMU bootstrap lecture](https://www.stat.cmu.edu/~cshalizi/dst/20/lectures/16/lecture-16.html)
explains the dependence problem and block methods. The
[time-series lecture](https://stat.cmu.edu/~cshalizi/uADA/16/lectures/26.pdf)
discusses the assumptions behind resampling dependent observations.

## Command reference

```sh
cargo run --release --manifest-path service/Cargo.toml -p crypto-portfolio -- \
	monte-carlo --folds var/research/wf-rank-2020-replay-settle \
	--out /tmp/ranker-monte-carlo.json --initial-cash 100000 \
	--iterations 10000 --seed 42 --block 20 --horizon-days 365
```

`--folds` and `--out` are required. Defaults are 10,000 iterations, seed 42,
20-day blocks, a 365-day horizon, and 365 annual periods for Sharpe.
`--annual-periods` changes Sharpe annualization. It does not change the daily
calendar or horizon length.

`--last-folds 4` selects the latest four folds by their first timestamp.
The report lists the selected source files and dates. This subset is a diagnostic
when those dates have already informed strategy development.

`--start YYYY-MM-DD --end YYYY-MM-DD` selects an explicit common calendar
window after computing the source returns. It preserves the first selected
day's return from the preceding NAV, without resetting capital at the cut.
Both dates are required and must lie inside each input's complete history.

`--benchmark DIR` enables paired resampling and a fixed daily-rebalanced 50/50
blend. Both inputs must have identical dates and fold boundaries after selection.
The command fails instead of silently dropping unmatched observations.

The loader reads `fold-*.json`. Each fold supplies `steps` with `as_of` and `nav`,
and preferably top-level `initial_cash`. Decimal strings and JSON numbers are
accepted. For legacy files without initial cash, `--initial-cash` is required.
Metadata takes precedence over that fallback. The report records the actual
starting cash, its source, and a SHA-256 hash for each input file.

The loader requires consecutive UTC-midnight timestamps, positive finite NAVs,
and at least 60 observed days. It rejects gaps, duplicate dates, overlapping
folds, and non-daily cadence. Each fold must contain at least two full blocks.
Iterations range from 100 to 1,000,000. Horizon length ranges from 1 to 36,500 days.

## Return accounting and sampling

The first return in each fold is `first_nav / initial_cash - 1`. Subsequent
returns are `nav_today / nav_yesterday - 1`. First-day fills and fees therefore
remain in the sample. Each fold restarts at initial cash. Liquidation and
transition costs absent from saved NAVs remain absent from these tests.

A block starts on a uniformly sampled source day and continues in order for
`--block` observations. At the fold's end it wraps to that fold's beginning.
Blocks never cross into another fold. Uniform starting days give every observed
day the same marginal sampling weight, including endpoint days.

Circular wrapping creates an artificial end-to-start adjacency. Joining sampled
blocks also breaks dependence. Block length sensitivity matters because the
history contains several market regimes and the correct dependence length is
unknown. Runs with 5-, 20-, and 60-day blocks expose that sensitivity. The command
does not select whichever length gives the most favorable result.

The SplitMix64 random stream and rejection-based index selection are specified
in the implementation. Identical inputs, options, and paths produce identical
JSON bytes. Changing source paths changes provenance fields.

## Report fields

`original_pooled_sharpe` uses all selected daily returns. It annualizes mean
return divided by sample standard deviation and assumes a zero risk-free rate.
`original_mean_fold_sharpe` averages independently recomputed fold Sharpes.
Those quantities need not match each other or older reports that omitted the
first-day costs.

`whole_sample_sharpe_bootstrap` resamples as many observations as the selected
history contains. Its percentile interval describes empirical sampling
uncertainty about Sharpe. Quantiles are 2.5%, 5%, 50%, 95%, and 97.5%, with linear
interpolation. Zero-variance or otherwise undefined Sharpe draws are counted and
excluded from the quantiles. A missing interval means no defined draws exist.

`horizon_scenarios` separately samples `--horizon-days` observations per path.
Each path starts at wealth 1 and compounds simple returns. Drawdown measures
peak-to-trough loss, including a loss on the first day. The report gives return
and maximum-drawdown quantiles, probability of a terminal loss, and probability
of a maximum drawdown greater than 25%.

With a benchmark, both histories use exactly the same source indices. The report
includes the bootstrap interval for strategy Sharpe minus benchmark Sharpe.
It also reports observed daily-return correlation and the benchmark and blend's
observed cumulative return and maximum drawdown.
The 50/50 blend uses the arithmetic mean of the two daily returns, implying
daily rebalancing. It adds no allocation-level transaction costs and optimizes
no weights.

## Interpretation limits

Horizon distributions are empirical scenarios conditional on observed regimes
and recorded costs. They are not calibrated forecasts. Missing execution costs,
unseen regimes, and signal decay do not appear automatically.

Simulated paths add no independent observations. Positive intervals cannot
repair look-ahead, repeated strategy selection, or reused holdouts. This command
does not apply a multiple-testing adjustment or deflated Sharpe calculation.
A fresh frozen evaluation remains necessary to assess a selected strategy.
