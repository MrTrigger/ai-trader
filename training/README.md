# Model training

Rust owns every model input. Generate the final matrix first:

```bash
cargo run --manifest-path service/Cargo.toml -p crypto-portfolio -- \
  training-matrix --config config/default.toml --data-root data \
  --start 2019-01-01 --end 2026-08-01 --out data/models/training.jsonl
```

Python may then fit those values without transforming them:

```bash
python3 -m venv training/.venv
training/.venv/bin/pip install -e training
training/.venv/bin/python training/train.py \
  --matrix data/models/training.jsonl --through 2025-08-01 \
  --out data/models/ranker.rust.json
```

The Rust runtime rejects an artifact whose feature catalogue version or ordered
feature names do not match its own.

Stockholm follows the same boundary. `stockholm-portfolio training-matrix`
emits the only permitted inputs and labels; `train_stockholm.py` performs only
ordered assembly, declared model fitting (LightGBM, weighted ridge, or their
fixed research blend), and JSON export. `walk_forward_stockholm.py` derives exchange-session fold boundaries,
purges one complete label horizon, retrains each expanding fold, and invokes the
Rust replay. The Rust matrix excludes First North and stamps the Large/Mid/Small
Main Market policy. Its public-data history remains survivor-contaminated and
research-only; see `docs/stockholm-reward-loss-study.md`. Official OMXSGI
history is collected by `equity-data` and aligned/evaluated by Rust; Python only
orchestrates fits and stitches already-computed Rust metrics.
The runtime and fold summaries bind both model family and feature-set version;
folds with different contracts may not be combined.

The optional `residual-pdmr-microstructure` contract consumes only normalized
Rust inputs. `equity-data` archives Nasdaq provider responses, and
`features-stockholm` calculates completed-session spread/trade features before
the JSONL crosses into Python. Pass the archive root with
`--nasdaq-market-history-root`; Python still performs no joins, rolling
windows, imputation, ranking, or liquidity transformations.

`residual-pdmr-microstructure-borrow` additionally consumes validated
historical `FEE_RATE` records from the shared `ib` crate. Rust calculates all
fee windows and emits the decision-session annual rate used by the Rust replay
cost model. Pass the completed audit root with `--ib-fee-history-root`; a
partial archive without `audit.json` is rejected.

The optional `relative_rank` response is also Rust-owned: the matrix contains
the average within-date forward-return rank already mapped to `[-1, 1]`.
Python fits it without ranking or clipping and records one training-prefix
zero-intercept slope back to relative-return units. Rust applies that scale
before its ordinary cost and minimum-edge gates.

`return_per_risk` and `relative_return_per_risk` are likewise complete Rust
labels. Python never divides a return by volatility. At inference the Rust
runtime multiplies the fitted per-risk score by the row's decision-time
volatility exactly once. The relative form is also centered cross-sectionally
in Rust after division and still requires the explicit market direction layer.
