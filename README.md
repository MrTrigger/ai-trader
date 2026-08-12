# AI Trader

An automated multi-bot trading system with one rule above the rest: research
and live decisions use the same implementation. The crypto portfolio and CME
futures and Stockholm-equity decision paths are Rust. For learned portfolios,
Python is permitted only to fit a model from a matrix whose features,
preprocessing, eligibility, target alignment, and targets were finalized by
Rust. The futures directory still has
a lab-only Python parity script against the external journal reference; it is
not a production or feature path.

See [`docs/design-spec.md`](docs/design-spec.md) for the original design and
[`docs/architecture-review-multibot.md`](docs/architecture-review-multibot.md)
for the multi-bot deployment boundary. Some historical documents describe the
former Python planner; this README is the current implementation map.

## Current architecture

| Piece | Implementation | Role |
|---|---|---|
| `crypto-portfolio` | Rust | crypto data, features, universe, signals, construction, risk, diff, Plan, replay, validation, gate, IC |
| `features-common` | Rust | instrument-agnostic final matrix operations shared by feature crates |
| `features-crypto` | Rust | sole implementation of crypto training and live features |
| `features-stockholm` | Rust | sole implementation of Stockholm training/live features and final preprocessing |
| `equity-data` | Rust | reusable public equity reference/history provider adapters and validation |
| `lightgbm-json` | Rust | strategy-neutral LightGBM tree evaluation |
| `portfolio-construction` | Rust | strategy-neutral no-quota ranking and maximum-budget allocation |
| `stockholm-portfolio` | Rust | Stockholm matrix CLI, model contract, policy composition, and replay |
| `features-cme`, `noise-book`, `futures-bot` | Rust | futures features, book, replay, and live runtime |
| `ib` | Rust | shared IB Gateway account verification plus futures and stock data/broker support |
| `plan`, `executor`, `runner`, venue crates | Rust | shared Plan contract and execution path |
| `api` + `frontend` | Rust + React/TypeScript | operational read/control surface |
| `training/train*.py` | Python | LightGBM fitting/evaluation orchestration and JSON tree export only |
| Postgres + Parquet | data | operational records and bar/funding history |

The old Python crypto planner is not on the scheduled path. `bin/cycle.sh`
invokes the Rust planner for data refresh, universe construction, and Plan
production, then hands the Plan to the Rust executor.

## Invariants

- Feature math has one asset-class owner: `features-crypto`, `features-cme`, or
  `features-stockholm`; shared matrix mechanics live in `features-common`.
- Training and inference both call the same Rust rank-normalisation function.
- A daily decision uses the daily bar at `T-1d` and the last fully closed
  hourly bar at `T-1h`; the Rust training matrix uses those exact snapshots.
- Python receives only final `x_*` values and a final demeaned target. It does
  not calculate, impute, align, rank, or select production features.
- Model artifacts are JSON LightGBM trees. The Rust runtime rejects pickle,
  unknown model/feature versions, reordered inputs, and scoring on or before
  the training cutoff.
- Backtests replay the production Rust `decide()` function. They do not carry a
  second strategy engine.
- Risk failure produces a rejected Plan with no orders; missing/stale inputs
  fail closed before Plan production.
- Bot code never implements IB requests, contract resolution, quote/bar
  decoding, shortability, or order transport; those belong to `ib` and
  broker-neutral contracts belong to `venue`.

## Build and test

```bash
cd service
cargo test --workspace
cargo build --workspace
```

The scheduled crypto cycle expects `service/target/debug/crypto-portfolio` and
`service/target/debug/bot`, falling back to release binaries.

## Crypto commands

From the repository root:

```bash
# Refresh public Binance spot reference bars.
service/target/debug/crypto-portfolio data-pull \
  --config config/default.toml --data-root data --days 7

# Bulk historical rebuild, including delisted assets (survivorship-safe).
service/target/debug/crypto-portfolio data-archive \
  --config config/default.toml --data-root data \
  --start 2019-10-01 --end 2026-08-01 --all-listed --interval daily

# Record one or a range of point-in-time liquidity universes.
service/target/debug/crypto-portfolio universe-rank \
  --config config/default.toml --data-root data \
  --as-of 2026-08-01 --end 2026-08-07 --step-days 1 --top 30

# Build a dry Plan from venue-shaped book JSON.
service/target/debug/crypto-portfolio plan \
  --config config/default.toml --data-root data --as-of 2026-08-07 \
  --book data/book.json --out /tmp/plan.json

# Replay the exact decision function or run the Phase 1 gate.
service/target/debug/crypto-portfolio backtest \
  --config config/default.toml --data-root data \
  --start 2021-10-01 --end 2026-08-01 --initial-cash 100000 \
  --slippage-multiple 1 --out /tmp/backtest.json

service/target/debug/crypto-portfolio gate \
  --config config/default.toml --data-root data \
  --start 2021-10-01 --end 2026-08-01 --initial-cash 100000 \
  --out /tmp/gate.json

# Measure signal IC on the same feature/universe history.
service/target/debug/crypto-portfolio ic \
  --config config/default.toml --data-root data \
  --start 2021-10-01 --end 2026-08-01 \
  --score ret_30_skip_7 --horizons 7,14,30 --out /tmp/ic.json

# Inspect raw store coverage and test the timestamp convention.
service/target/debug/crypto-portfolio data-inspect --data-root data
service/target/debug/crypto-portfolio data-verify \
  --data-root data --interval daily --asset BTC --cross-interval

# Prove repeated decisions are content-identical despite different wall clocks.
service/target/debug/crypto-portfolio plan-verify \
  --config config/default.toml --data-root data --as-of 2026-08-07 \
  --book data/book.json --runs 3

# Sweep one axis, build one-window evidence, then render it without recomputing.
service/target/debug/crypto-portfolio sweep \
  --config config/default.toml --data-root data \
  --start 2021-10-01 --end 2026-08-01 --initial-cash 100000 \
  --axis holdings --values 5,10,15,20 --out /tmp/holdings-sweep.json

service/target/debug/crypto-portfolio research \
  --config config/default.toml --data-root data \
  --start 2021-10-01 --end 2026-08-01 --initial-cash 100000 \
  --out /tmp/research.json
service/target/debug/crypto-portfolio report \
  --record /tmp/research.json --out /tmp/research.html
```

`gate` exits non-zero unless all criteria pass. The default configuration still
uses `placeholder_equal_long`, which explicitly claims no edge.

## Model training

Generate the final matrix in Rust:

```bash
service/target/debug/crypto-portfolio training-matrix \
  --config config/default.toml --data-root data \
  --start 2019-10-01 --end 2026-08-01 \
  --out data/models/training.jsonl
```

Then fit without transforming the values:

```bash
python3 -m venv training/.venv
training/.venv/bin/pip install -e training
training/.venv/bin/python training/train.py \
  --matrix data/models/training.jsonl --through 2025-08-01 \
  --out data/models/ranker.rust.json
```

See [`training/README.md`](training/README.md) for the boundary in detail.

## Stockholm exploratory research

The first current-survivor study is reproducible with the dedicated Rust CLI:

```bash
service/target/release/stockholm-portfolio collect \
  --data-root var/stockholm/research-public \
  --start 2016-08-08 --end 2026-08-07

service/target/release/stockholm-portfolio collect-benchmark \
  --data-root var/stockholm/research-public --symbol OMXSGI \
  --start 2016-08-08 --end 2026-08-07

service/target/release/stockholm-portfolio training-matrix \
  --data-root var/stockholm/research-public \
  --start 2016-08-08 --end 2026-08-07 \
  --horizon-sessions 20 --min-adv-sek 1000000 \
  --feature-set baseline \
  --out var/stockholm/research-public/training-20-main-relative.jsonl

training/.venv/bin/python training/walk_forward_stockholm.py \
  --matrix var/stockholm/research-public/training-20-main-relative.jsonl \
  --benchmark var/stockholm/research-public/benchmarks/OMXSGI.json \
  --binary service/target/release/stockholm-portfolio \
  --out var/stockholm/reward-loss-main-dev/absolute_return-l2 \
  --start 2022-09-01 --end 2025-09-30 --folds 4 \
  --reward relative_return --model-family lightgbm \
  --objective l2 --seeds 1 --direction-overlay
```

Every resulting dataset, model, and report is stamped
`SURVIVORSHIP_CONTAMINATED`; this public-data path cannot authorize capital.
The matrix contains Nasdaq Stockholm Main Market Large, Mid, and Small Cap only;
First North is excluded in Rust. See the corrected
[reward/loss study](docs/stockholm-reward-loss-study.md) for the six-way
development comparison, decomposed direction/residual challengers,
candidate-specific pseudo-holdout, OMXSGI comparison, and failed Sharpe-2.0
verdict. `--feature-set residual` emits the versioned 62-input v3 negative
experiment; it is research functionality, not a promoted model.

The shared authority-data collectors also archive FI net-short disclosures,
FI PDMR transactions, and Skatteverket listing histories. The PDMR collector
uses publication-date slices, adapts around FI's export ceiling, and is
resumable from immutable raw files. Its v5 matrix is generated entirely in
Rust:

```bash
service/target/release/stockholm-portfolio collect-fi-pdmr \
  --data-root var/stockholm/authority-data \
  --start 2016-07-03 --end 2026-08-10

service/target/release/stockholm-portfolio training-matrix \
  --data-root var/stockholm/research-public \
  --start 2016-08-08 --end 2026-08-07 \
  --horizon-sessions 20 --min-adv-sek 1000000 \
  --feature-set residual-pdmr \
  --fi-pdmr var/stockholm/authority-data/fi-pdmr/latest.json \
  --out var/stockholm/research-public/training-20-residual-pdmr-v5.jsonl
```

PDMR v5 improves the closed development control from Sharpe 0.34 to 0.47 and
reduces drawdown, but remains rejected: the later diagnostic has negative
return/Sharpe and the required 2.0 Sharpe is not approached.

Official observed-liquidity history is collected separately through the same
shared data crate. Nasdaq's public endpoint currently covers active shares for
approximately ten years and includes closing bid/ask, turnover, and trade
count; it does not retain delisted instruments, so its manifest remains
`SURVIVORSHIP_CONTAMINATED`:

```bash
service/target/release/stockholm-portfolio collect-nasdaq-market-history \
  --data-root var/stockholm/authority-data \
  --start 2016-01-01 --end 2026-08-11 \
  --supplemental-universe var/stockholm/research-public/universe.json

service/target/release/stockholm-portfolio training-matrix \
  --data-root var/stockholm/research-public \
  --start 2016-08-08 --end 2026-08-07 \
  --horizon-sessions 20 --min-adv-sek 1000000 \
  --feature-set residual-pdmr-microstructure \
  --fi-pdmr var/stockholm/authority-data/fi-pdmr/latest.json \
  --nasdaq-market-history-root var/stockholm/authority-data \
  --out var/stockholm/research-public/training-20-residual-pdmr-microstructure-v9.jsonl
```

The one-shot v9 development replay improved v5 to +26.9%, Sharpe 0.76 (2x
costs: +20.5%, Sharpe 0.61), but retained a severe negative fold and remained
below OMXSGI's 0.92 Sharpe. It is rejected; no later diagnostic or tuned
microstructure variant was opened.

Historical stock-borrow cost is collected through the shared `ib` data source,
not through bot code. The full request is paced and resumable; `FEE_RATE` is a
decimal annual cost and does not prove that shares were available to borrow:

```bash
set -a; source .env; set +a
cargo run --release --manifest-path service/Cargo.toml -p ib \
  --example stock_coverage_audit -- \
  var/stockholm/research-public/universe.json \
  var/stockholm/ib-fee-full-v1 10 0 10500 fee-rate

# Prospective availability cannot be backfilled. Capture immutable current
# shortability and account-visible quantity snapshots through the same source.
cargo run --release --manifest-path service/Cargo.toml -p ib \
  --example stock_borrow_snapshot -- \
  var/stockholm/research-public/universe.json \
  var/stockholm/ib-borrow-snapshots 8

service/target/release/stockholm-portfolio training-matrix \
  --data-root var/stockholm/research-public \
  --start 2016-08-08 --end 2026-08-07 \
  --horizon-sessions 20 --min-adv-sek 1000000 \
  --feature-set residual-pdmr-microstructure-borrow \
  --fi-pdmr var/stockholm/authority-data/fi-pdmr/latest.json \
  --nasdaq-market-history-root var/stockholm/authority-data \
  --ib-fee-history-root var/stockholm/ib-fee-full-v1 \
  --out var/stockholm/research-public/training-20-residual-pdmr-microstructure-borrow-v10.jsonl
```

V10 also supports the crypto-style robust `relative_rank` response as a
separate predeclared arm. The rank label is finalized by Rust and the fitted
training-prefix scale converts scores back to return units before Rust applies
cost gates; run it with `--clip-quantile 0`.

The third predeclared arm, `relative_return_per_risk`, transfers the crypto
bot's validated risk-adjusted reward without mixing market direction back into
stock selection. Rust owns the label and converts inference back to return
units exactly once.

Run each arm on the same frozen development folds by changing only `--reward`
and the output directory:

```bash
training/.venv/bin/python training/walk_forward_stockholm.py \
  --matrix var/stockholm/research-public/training-20-residual-pdmr-microstructure-borrow-v10.jsonl \
  --benchmark var/stockholm/direction-data/benchmarks/OMXSGI.json \
  --binary service/target/release/stockholm-portfolio \
  --out var/stockholm/residual-pdmr-microstructure-borrow-v10-return-dev \
  --start 2022-09-01 --end 2025-09-30 --folds 4 \
  --reward relative_return --model-family lightgbm --objective l2 \
  --seeds 1 --direction-overlay
```

Use `relative_return_per_risk` for the second directory/arm. Use
`relative_rank --clip-quantile 0` for the third; the other two retain the
declared default clipping.

The first prospective IB capture returned current shortable quantity for 342
of 412 Main Market lines: 140 Large, 119 Mid, and 83 Small Cap. That evidence
supports a broad short candidate pool, but it is one timestamp and never
substitutes for the live pre-trade locate/quantity gate or historical data.

## Operations

`bin/cycle.sh [bot-config]` is one fail-closed cycle:

1. refresh daily and hourly bars in Rust;
2. record the current venue-screened universe in Rust;
3. read the venue book and produce a live-mode Plan in Rust;
4. execute it with the Rust bot.

The executor's venue mode (`paper`, `live-readonly`, or `live`) remains the
authority on whether orders can move real capital. A Plan cannot select that
mode for itself.

## Relationship to trading-journal

This is a separate system and security boundary. It inherits the journal's
research discipline—causal prefix tests, time-ordered validation, cost stress,
and disclosure-first reporting—not its discrete-trade backtest engine. A
continuously rebalanced portfolio has different accounting and execution
semantics.
