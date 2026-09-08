# Experiment register

Run the command from the repository root. Manifest paths and configured artifact
paths resolve from the current directory, matching the backtest commands.

```sh
cargo run --manifest-path service/Cargo.toml -p crypto-portfolio -- experiment-register \
  --manifest docs/research/experiments.json --out var/research/experiment-register.json
```

The curated manifest records corrected, superseded and rejected crypto research,
the Stockholm rejection and the external NQ comparator. The NQ sources require
the sibling `../trading-journal` checkout. Source-only entries carry no computed
NAV metrics because their report formats differ from crypto replay folds.
It is not a complete history of every search. `trial_count: null` means unknown;
the number of rows is not a substitute. Directional research documents 13 trials
across three rounds, but the manifest includes only two selected candidates.

Each experiment declares an `id`, `family`, `status`, optional `trial_count`,
`notes`, `corrections`, `supersedes` references and documentary `sources`.
Replay-backed entries also require `fold_dir` and a positive `expected_folds`.
The directory must contain exactly `fold-1.json` through the expected count and
each matching `config-N.toml`. Initial cash comes from the replay's
`initial_cash`, `metadata.initial_cash` or the manifest's `initial_cash` string.
Conflicting replay and manifest values fail the command.

The output hashes the supplied manifest, documentary sources, reports, configs
and configured model, timing-model, market-matrix and liquidity-profile files.
These hashes identify the bytes inspected now. They do not recreate a missing
historical source commit, data snapshot or run configuration. New replay metadata
should carry that provenance when the experiment runs.

Suite-produced folds require `inputs.json` in the scenario directory's parent
and a matching `fold-N.sha256` completion marker. Registration verifies the
saved inventory fingerprint, the fingerprinted source manifest, copied configs,
referenced models, execution scenario, initial cash and every declared fold
window. It hashes `inputs.json` once per suite. Raw data and the historical
executable are not rehashed during registration; the output states this limit.
Legacy folds explicitly mark suite provenance as unavailable.

A replay-suite scenario can declare `config_dir` to replace the suite's
`source_dir` for its per-fold configurations. The suite hashes those configs and
their model artifacts, and registration verifies the scenario-specific source
path. Shared feature preparation requires the same daily interval and benchmark.

`pooled_daily_sharpe` uses all nonoverlapping daily returns. The command compounds
intraday returns by UTC date and includes the first step relative to initial
cash. It uses sample standard deviation and 365 days per year.
`mean_fold_daily_sharpe` averages individually recomputed fold Sharpes.
`mean_saved_fold_sharpe` preserves the old reports' value separately. Undefined
zero-variance Sharpe is null. A missing fold, missing configured artifact,
duplicate identifier, broken decision grid or overlapping folds fails the run.

`reused_evidence` lists shared evaluation dates between experiments, including
records from different strategy families. It never turns a declared development
record into a frozen one. Shared dates disclose reuse; they do not determine
exactly when a researcher inspected a result.

Every fold reports attribution availability. New reports aggregate long and
short price P&L, funding, commissions, spread, impact, delay P&L and unfilled
notional. The command reconciles those amounts with every NAV change and checks
the saved reconciliation error. Nonzero residuals remain visible. Old files
explicitly report attribution as unavailable; the command does not infer it
from fills or overwrite the source files.
