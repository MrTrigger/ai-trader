"""Fit LightGBM on a matrix finalized by the Rust feature engine.

This file deliberately contains no feature calculation or preprocessing. Its
input is `crypto-portfolio training-matrix` JSONL: ordered, rank-normalised
model inputs and the final relative-return target. Python's only job is fitting
trees and exporting them to the auditable Rust runtime format.
"""

from __future__ import annotations

import argparse
import json
from datetime import date, datetime, timezone
from pathlib import Path

import lightgbm as lgb
import numpy as np

FORMAT_VERSION = "crypto-lightgbm-json-1"
MODEL_VERSION = "ml-ranker-rust-features-1"
PARAMS = {
    "objective": "regression",
    "metric": "l2",
    "learning_rate": 0.03,
    "num_leaves": 15,
    "min_data_in_leaf": 200,
    "feature_fraction": 0.7,
    "bagging_fraction": 0.7,
    "bagging_freq": 1,
    "lambda_l2": 20.0,
    "verbose": -1,
}
NUM_ROUNDS = 300


def load_matrix(path: Path, through: date):
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        if manifest.get("kind") != "manifest":
            raise ValueError("first matrix row is not the Rust feature manifest")
        features = manifest["features"]
        rows = [json.loads(line) for line in source if line.strip()]
    rows = [row for row in rows if date.fromisoformat(row["date"]) <= through]
    if len(rows) < 3_000:
        raise ValueError(f"only {len(rows)} rows through {through}; refusing a small fit")
    # Ordered lookup only. Values have already been computed, imputed, and
    # normalised by Rust; there is intentionally no transform here.
    x = np.asarray([[row["features"][name] for name in features] for row in rows])
    y = np.asarray([row["target"] for row in rows])
    return manifest, features, rows, x, y


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    manifest, features, rows, x, y = load_matrix(args.matrix, args.through)
    booster = lgb.train(PARAMS, lgb.Dataset(x, label=y), num_boost_round=NUM_ROUNDS)
    dump = booster.dump_model()
    document = {
        "format_version": FORMAT_VERSION,
        "model_version": MODEL_VERSION,
        "feature_set_version": manifest["feature_set_version"],
        "trained_through": args.through.isoformat(),
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "n_rows": len(rows),
        "n_dates": len({row["date"] for row in rows}),
        "features": features,
        "tree_info": dump["tree_info"],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes((json.dumps(document, sort_keys=True, indent=2) + "\n").encode())
    print(f"wrote {args.out}: {len(rows):,} rows, {len(features)} Rust-owned features")


if __name__ == "__main__":
    main()
