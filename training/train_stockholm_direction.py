"""Fit the separate Stockholm market-direction model.

Rust owns every feature, missing flag, label, and date alignment. This module
only selects the declared training prefix, applies training-only target
winsorisation, fits one deliberately shallow LightGBM regressor, and exports
the generic tree dump for Rust inference.

Retirement notice: every trained direction variant tested against this
project's ~250 independent 20-session market outcomes lost to a fixed
five-vote trend control and to buy-and-hold OMXSGI (see
docs/stockholm-portfolio-status.md, Task 8). Rust refuses to reach this
model family from any promotable configuration; this trainer stays only for
research/diagnostics replays that explicitly ask for it.

`score_scale` is exported as the fitted booster's own in-sample prediction
spread (`std` of its train-set predictions), not the target's. Rust computes
`score = clip(prediction / score_scale, -1, 1)` against fixed 0.4/0.8 policy
thresholds; anchoring the scale to the (shrunk) predictions rather than the
wider target means a score now spans the threshold range by construction,
instead of a heavily L2-shrunk model parking near zero forever.
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
FEATURE_SET_VERSIONS = {
    "fs-rust-stockholm-direction-1",
    "fs-rust-stockholm-direction-2",
    "fs-rust-stockholm-direction-3",
}
PARAMS = {
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
REWARDS = ("absolute_return", "direction_sign")
OBJECTIVES = {
    "l2": ("regression", "l2"),
    "l1": ("regression_l1", "l1"),
    "huber": ("huber", "huber"),
}


def load_training_prefix(
    path: Path, through: date, clip_quantile: float, reward: str = "absolute_return"
):
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
    if manifest.get("feature_set_version") not in FEATURE_SET_VERSIONS:
        raise ValueError("unsupported direction feature-set version")
    if len(rows) < 500:
        raise ValueError(f"only {len(rows)} direction rows through {through}")
    if not 0.0 <= clip_quantile < 0.5:
        raise ValueError("clip quantile must be in [0, 0.5)")
    if reward not in REWARDS:
        raise ValueError(f"unsupported direction reward {reward!r}")
    features = manifest["features"]
    x = np.asarray(
        [[row["features"][feature] for feature in features] for row in rows],
        dtype=np.float64,
    )
    absolute_y = np.asarray([row["target"] for row in rows], dtype=np.float64)
    if reward == "direction_sign":
        if clip_quantile:
            raise ValueError("direction-sign labels are Rust-bounded; clipping must be zero")
        if any("sign_target" not in row for row in rows):
            raise ValueError("Rust direction matrix lacks sign labels")
        y = np.asarray([row["sign_target"] for row in rows], dtype=np.float64)
        if not np.isin(y, [-1.0, 0.0, 1.0]).all():
            raise ValueError("Rust direction matrix has invalid sign labels")
    else:
        y = absolute_y.copy()
    if not np.isfinite(x).all() or not np.isfinite(y).all():
        raise ValueError("Rust direction matrix contains non-finite values")
    clip = None
    if clip_quantile:
        lower, upper = np.quantile(y, [clip_quantile, 1.0 - clip_quantile])
        y = np.clip(y, lower, upper)
        clip = [float(lower), float(upper)]
    return manifest, features, rows, x, y, absolute_y, clip


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--clip-quantile", type=float, default=0.01)
    parser.add_argument("--objective", choices=OBJECTIVES, default="l2")
    parser.add_argument("--reward", choices=REWARDS, default="absolute_return")
    args = parser.parse_args()

    manifest, features, rows, x, y, absolute_y, clip = load_training_prefix(
        args.matrix, args.through, args.clip_quantile, args.reward
    )
    dataset = lgb.Dataset(x, label=y, feature_name=features)
    objective, metric = OBJECTIVES[args.objective]
    booster = lgb.train(
        dict(PARAMS, objective=objective, metric=metric),
        dataset,
        num_boost_round=NUM_ROUNDS,
    )
    # The scale must come from what the model actually outputs, not from the
    # target it was trying to hit: a heavily L2-shrunk booster's predictions
    # can sit at a small fraction of the target's spread, and normalizing by
    # the wider target left scores parked near zero, never reaching the
    # policy's 0.4/0.8 thresholds. Normalizing by the model's own train-set
    # prediction spread instead makes a score span the threshold range by
    # construction.
    train_predictions = booster.predict(x)
    score_scale = float(np.std(train_predictions))
    if not np.isfinite(score_scale) or score_scale <= 0.0:
        raise ValueError("direction model prediction spread is not finite and positive")
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
        "reward": args.reward,
        "objective": args.objective,
        "target_clip": clip,
        "score_scale": score_scale,
        "tree_info": booster.dump_model()["tree_info"],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
    print(
        f"wrote {args.out}: {len(rows):,} causal daily rows, "
        f"{len(features)} Rust-owned inputs, {args.reward}/{args.objective}, "
        f"through {args.through}"
    )


if __name__ == "__main__":
    main()
