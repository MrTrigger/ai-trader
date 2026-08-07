# AI Trader

An automated multi-bot trading system with one rule above the rest: research
and live decisions use the same implementation. The crypto portfolio and CME
futures decision paths are Rust. In the crypto system, Python is permitted only
to fit a model from a matrix whose features, preprocessing, eligibility, target
alignment, and targets were finalized by Rust. The futures directory still has
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
| `features-crypto` | Rust | sole implementation of crypto training and live features, including rank preprocessing |
| `features-cme`, `noise-book`, `futures-bot` | Rust | futures features, book, replay, and live runtime |
| `plan`, `executor`, `runner`, venue crates | Rust | shared Plan contract and execution path |
| `api` + `frontend` | Rust + React/TypeScript | operational read/control surface |
| `training/train.py` | Python | LightGBM fitting and JSON tree export only |
| Postgres + Parquet | data | operational records and bar/funding history |

The old Python crypto planner is not on the scheduled path. `bin/cycle.sh`
invokes the Rust planner for data refresh, universe construction, and Plan
production, then hands the Plan to the Rust executor.

## Invariants

- Feature math has one owner: `service/crates/features-crypto`.
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
