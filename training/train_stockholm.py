"""Fit a Stockholm model artifact from the finalized Rust matrix.

No feature, label, alignment, missing-value, ranking, clipping, or universe
logic belongs here. This module filters rows by the declared training cutoff,
assembles already-final values in manifest order, fits a declared model family,
and exports it for identical Rust inference.
"""

from __future__ import annotations

import argparse
import json
from datetime import date, datetime, timezone
from pathlib import Path

import lightgbm as lgb
import numpy as np

FORMAT_VERSION = "stockholm-model-json-2"
MODEL_VERSION = "stockholm-ranker-1"
MODEL_FAMILIES = ("lightgbm", "ridge", "hybrid")
DEFAULT_RIDGE_LAMBDA = 25.0
PARAMS = {
    "learning_rate": 0.025,
    "num_leaves": 15,
    "max_depth": 5,
    "min_data_in_leaf": 250,
    "feature_fraction": 0.75,
    "bagging_fraction": 0.75,
    "bagging_freq": 1,
    "lambda_l2": 25.0,
    "num_threads": 0,
    "verbose": -1,
}
NUM_ROUNDS = 250
OBJECTIVES = {
    "l2": ("regression", "l2"),
    "l1": ("regression_l1", "l1"),
    "huber": ("huber", "huber"),
}
REWARDS = (
    "absolute_return",
    "return_per_risk",
    "relative_return",
    "relative_return_per_risk",
    "relative_rank",
)


def load_matrix(
    path: Path,
    through: date,
    reward: str = "absolute_return",
    clip_quantile: float = 0.005,
):
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        if manifest.get("kind") != "stockholm_training_manifest":
            raise ValueError("first row is not a Stockholm Rust matrix manifest")
        features = manifest["features"]
        rows = [
            row
            for line in source
            if line.strip()
            for row in [json.loads(line)]
            if date.fromisoformat(row["date"]) <= through
        ]
    if manifest.get("survivorship_status") != "SURVIVORSHIP_CONTAMINATED":
        raise ValueError("unexpected research data status")
    if len(rows) < 10_000:
        raise ValueError(f"only {len(rows)} rows through {through}; refusing a small fit")
    if reward not in REWARDS:
        raise ValueError(f"unsupported Stockholm reward {reward!r}")
    if not 0.0 <= clip_quantile < 0.5:
        raise ValueError("clip quantile must be in [0, 0.5)")
    if reward == "return_per_risk":
        rows = [row for row in rows if row.get("return_per_risk_target") is not None]
        if len(rows) < 10_000:
            raise ValueError(
                f"only {len(rows)} Rust risk-target rows through {through}; "
                "refusing a small fit"
            )
        y = np.asarray([row["return_per_risk_target"] for row in rows], dtype=np.float64)
    elif reward == "relative_return_per_risk":
        rows = [
            row
            for row in rows
            if row.get("relative_return_per_risk_target") is not None
        ]
        if len(rows) < 10_000:
            raise ValueError(
                f"only {len(rows)} Rust relative-risk rows through {through}; "
                "refusing a small fit"
            )
        y = np.asarray(
            [row["relative_return_per_risk_target"] for row in rows],
            dtype=np.float64,
        )
    elif reward == "relative_return":
        if any(row.get("relative_target") is None for row in rows):
            raise ValueError("Rust matrix lacks relative-return labels")
        y = np.asarray([row["relative_target"] for row in rows], dtype=np.float64)
    elif reward == "relative_rank":
        if clip_quantile:
            raise ValueError("relative-rank labels are Rust-bounded; clipping must be zero")
        if any(row.get("relative_rank_target") is None for row in rows):
            raise ValueError("Rust matrix lacks relative-rank labels")
        y = np.asarray([row["relative_rank_target"] for row in rows], dtype=np.float64)
    else:
        y = np.asarray([row["target"] for row in rows], dtype=np.float64)
    x = np.asarray(
        [[row["features"][feature] for feature in features] for row in rows],
        dtype=np.float64,
    )
    clip = None
    if clip_quantile:
        lower, upper = np.quantile(y, [clip_quantile, 1.0 - clip_quantile])
        y = np.clip(y, lower, upper)
        clip = [float(lower), float(upper)]
    weights = np.asarray([row["sample_weight"] for row in rows], dtype=np.float64)
    if not np.isfinite(x).all() or not np.isfinite(y).all() or not np.isfinite(weights).all():
        raise ValueError("Rust matrix contains non-finite values")
    reward_scale = None
    if reward == "relative_rank":
        if any(row.get("relative_target") is None for row in rows):
            raise ValueError("Rust matrix lacks relative-return calibration labels")
        relative = np.asarray([row["relative_target"] for row in rows], dtype=np.float64)
        denominator = float(np.sum(weights * y * y))
        reward_scale = float(np.sum(weights * y * relative) / denominator)
        if not np.isfinite(reward_scale) or reward_scale <= 0.0:
            raise ValueError("relative-rank calibration is not finite and positive")
    return manifest, features, rows, x, y, weights, clip, reward_scale


def scale_leaves(node, factor):
    if "leaf_value" in node and node["leaf_value"] is not None:
        node["leaf_value"] *= factor
    for side in ("left_child", "right_child"):
        if isinstance(node.get(side), dict):
            scale_leaves(node[side], factor)


def fit_ridge(x, y, weights, penalty=DEFAULT_RIDGE_LAMBDA):
    """Fit weighted ridge with an unpenalized intercept.

    Matrix inputs and labels are already finalized by Rust. This is model
    fitting, not feature engineering. Centered normal equations avoid adding a
    second full copy of this relatively large research matrix.
    """
    if not np.isfinite(penalty) or penalty <= 0.0:
        raise ValueError("ridge penalty must be finite and positive")
    total_weight = float(weights.sum())
    if total_weight <= 0.0:
        raise ValueError("ridge sample weights must sum to a positive value")
    x_mean = np.average(x, axis=0, weights=weights)
    y_mean = float(np.average(y, weights=weights))
    weighted_x = x * weights[:, None]
    gram = x.T @ weighted_x - total_weight * np.outer(x_mean, x_mean)
    gram.flat[:: gram.shape[0] + 1] += penalty
    rhs = x.T @ (weights * y) - total_weight * x_mean * y_mean
    coefficients = np.linalg.solve(gram, rhs)
    intercept = y_mean - float(x_mean @ coefficients)
    if not np.isfinite(coefficients).all() or not np.isfinite(intercept):
        raise ValueError("ridge fit produced non-finite parameters")
    return intercept, coefficients


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--reward", choices=REWARDS, default="absolute_return")
    parser.add_argument("--model-family", choices=MODEL_FAMILIES, default="lightgbm")
    parser.add_argument("--objective", choices=OBJECTIVES, default="l1")
    parser.add_argument("--seeds", type=int, default=1)
    parser.add_argument("--ridge-lambda", type=float, default=DEFAULT_RIDGE_LAMBDA)
    parser.add_argument(
        "--clip-quantile",
        type=float,
        default=0.005,
        help="two-sided training-only target winsorisation; zero disables it",
    )
    args = parser.parse_args()
    if args.seeds <= 0:
        parser.error("--seeds must be positive")
    if args.model_family == "ridge" and args.seeds != 1:
        parser.error("ridge is deterministic; --seeds must be 1")

    manifest, features, rows, x, y, weights, clip, reward_scale = load_matrix(
        args.matrix, args.through, args.reward, args.clip_quantile
    )
    trees = []
    linear_intercept = None
    linear_weights = None
    ridge_lambda = None
    blend_weight = None
    if args.model_family in ("lightgbm", "hybrid"):
        dataset = lgb.Dataset(x, label=y, weight=weights, feature_name=features)
        objective, metric = OBJECTIVES[args.objective]
        for seed_index in range(args.seeds):
            seed = 917 + seed_index
            params = dict(
                PARAMS,
                objective=objective,
                metric=metric,
                seed=seed,
                bagging_seed=seed + 1_000,
                feature_fraction_seed=seed + 2_000,
            )
            booster = lgb.train(params, dataset, num_boost_round=NUM_ROUNDS)
            dumped = booster.dump_model()["tree_info"]
            if args.seeds > 1:
                for tree in dumped:
                    scale_leaves(tree["tree_structure"], 1.0 / args.seeds)
            trees.extend(dumped)
    if args.model_family in ("ridge", "hybrid"):
        linear_intercept, coefficients = fit_ridge(
            x, y, weights, penalty=args.ridge_lambda
        )
        linear_weights = coefficients.tolist()
        ridge_lambda = args.ridge_lambda
    if args.model_family == "hybrid":
        blend_weight = 0.5
    document = {
        "format_version": FORMAT_VERSION,
        "model_version": MODEL_VERSION,
        "feature_set_version": manifest["feature_set_version"],
        "label_version": manifest["label_version"],
        "trained_through": args.through.isoformat(),
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "n_rows": len(rows),
        "n_dates": len({row["date"] for row in rows}),
        "features": features,
        "survivorship_status": manifest["survivorship_status"],
        "model_family": args.model_family,
        "reward": args.reward,
        "objective": args.objective,
        "ensemble_seeds": args.seeds,
        "target_clip": clip,
        "reward_scale": reward_scale,
        "ridge_lambda": ridge_lambda,
        "linear_intercept": linear_intercept,
        "linear_weights": linear_weights,
        "tree_blend_weight": blend_weight,
        "tree_info": trees,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
    print(
        f"wrote {args.out}: {len(rows):,} rows, {len(features)} Rust-owned inputs, "
        f"{args.model_family} {args.reward}/{args.objective}, {args.seeds} seed(s), "
        f"through {args.through}"
    )


if __name__ == "__main__":
    main()
