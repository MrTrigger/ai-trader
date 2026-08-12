"""Fit the separate Stockholm market-direction model.

Rust owns every feature, missing flag, label, and date alignment. This module
only selects the declared training prefix, applies training-only target
winsorisation, fits one deliberately shallow LightGBM regressor, and exports
the generic tree dump for Rust inference.
"""

from __future__ import annotations

import argparse
import json
from datetime import date, datetime, timezone
from pathlib import Path

import lightgbm as lgb
import numpy as np

FORMAT_VERSION = "stockholm-direction-model-json-1"
MODEL_VERSION = "stockholm-direction-model-1"
FEATURE_SET_VERSION = "fs-rust-stockholm-direction-1"
SCORE_SCALE = 0.04
PARAMS = {
    "objective": "regression",
    "metric": "l2",
    "learning_rate": 0.025,
    "num_leaves": 7,
    "max_depth": 3,
    "min_data_in_leaf": 100,
    "feature_fraction": 0.8,
    "bagging_fraction": 0.8,
    "bagging_freq": 1,
    "lambda_l2": 25.0,
    "seed": 1701,
    "bagging_seed": 2701,
    "feature_fraction_seed": 3701,
    "num_threads": 0,
    "verbose": -1,
}
NUM_ROUNDS = 200


def load_training_prefix(path: Path, through: date, clip_quantile: float):
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        rows = [
            row
            for line in source
            if line.strip()
            for row in [json.loads(line)]
            if date.fromisoformat(row["date"]) <= through
        ]
    if manifest.get("kind") != "stockholm_direction_training_manifest":
        raise ValueError("first row is not a Stockholm direction matrix manifest")
    if manifest.get("feature_set_version") != FEATURE_SET_VERSION:
        raise ValueError("direction feature-set version differs from the trainer")
    if len(rows) < 500:
        raise ValueError(f"only {len(rows)} direction rows through {through}")
    if not 0.0 <= clip_quantile < 0.5:
        raise ValueError("clip quantile must be in [0, 0.5)")
    features = manifest["features"]
    x = np.asarray(
        [[row["features"][feature] for feature in features] for row in rows],
        dtype=np.float64,
    )
    y = np.asarray([row["target"] for row in rows], dtype=np.float64)
    if not np.isfinite(x).all() or not np.isfinite(y).all():
        raise ValueError("Rust direction matrix contains non-finite values")
    clip = None
    if clip_quantile:
        lower, upper = np.quantile(y, [clip_quantile, 1.0 - clip_quantile])
        y = np.clip(y, lower, upper)
        clip = [float(lower), float(upper)]
    return manifest, features, rows, x, y, clip


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--clip-quantile", type=float, default=0.01)
    args = parser.parse_args()

    manifest, features, rows, x, y, clip = load_training_prefix(
        args.matrix, args.through, args.clip_quantile
    )
    dataset = lgb.Dataset(x, label=y, feature_name=features)
    booster = lgb.train(PARAMS, dataset, num_boost_round=NUM_ROUNDS)
    document = {
        "format_version": FORMAT_VERSION,
        "model_version": MODEL_VERSION,
        "feature_set_version": manifest["feature_set_version"],
        "label_version": manifest["label_version"],
        "trained_through": args.through.isoformat(),
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "n_rows": len(rows),
        "n_dates": len(rows),
        "features": features,
        "model_family": "lightgbm",
        "objective": "l2",
        "target_clip": clip,
        "score_scale": SCORE_SCALE,
        "tree_info": booster.dump_model()["tree_info"],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
    print(
        f"wrote {args.out}: {len(rows):,} causal daily rows, "
        f"{len(features)} Rust-owned inputs, through {args.through}"
    )


if __name__ == "__main__":
    main()
