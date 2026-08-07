# Crypto model training

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
