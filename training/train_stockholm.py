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
CALIBRATION_METHOD = "purged_oos_affine_shrinkage_v1"
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
    from_date: date | None = None,
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
            if (from_date is None or date.fromisoformat(row["date"]) >= from_date)
            and date.fromisoformat(row["date"]) <= through
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


def fit_lightgbm_ensemble(x, y, weights, features, objective_name, seeds):
    """Fit the declared tree ensemble and return boosters plus exported trees."""
    dataset = lgb.Dataset(x, label=y, weight=weights, feature_name=features)
    objective, metric = OBJECTIVES[objective_name]
    boosters = []
    trees = []
    for seed_index in range(seeds):
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
        boosters.append(booster)
        dumped = booster.dump_model()["tree_info"]
        if seeds > 1:
            for tree in dumped:
                scale_leaves(tree["tree_structure"], 1.0 / seeds)
        trees.extend(dumped)
    return boosters, trees


def return_targets(rows, reward):
    field = (
        "relative_target"
        if reward in ("relative_return", "relative_return_per_risk", "relative_rank")
        else "target"
    )
    values = np.asarray([row[field] for row in rows], dtype=np.float64)
    if not np.isfinite(values).all():
        raise ValueError("Rust matrix contains non-finite return calibration labels")
    return values


def predictions_in_return_units(raw, rows, reward, reward_scale):
    values = np.asarray(raw, dtype=np.float64)
    if reward in ("return_per_risk", "relative_return_per_risk"):
        values = values * np.asarray([row["vol60"] for row in rows], dtype=np.float64)
    elif reward == "relative_rank":
        values = values * reward_scale
    return values


def fit_purged_calibration(
    rows,
    x,
    weights,
    features,
    reward,
    objective,
    seeds,
    reward_scale,
    calibration_sessions,
    horizon_sessions,
    clip_quantile,
):
    """Calibrate on predictions strictly after a purged provisional fit.

    The final model is subsequently refit on the complete training prefix. The
    affine slope is constrained to [0, 1], so calibration can only preserve or
    shrink raw dispersion; it cannot amplify a noisy score.
    """
    dates = np.asarray([row["date"] for row in rows])
    unique_dates = sorted(set(dates.tolist()))
    if calibration_sessions <= 0:
        return None
    minimum = calibration_sessions + horizon_sessions + 252
    if len(unique_dates) < minimum:
        raise ValueError(
            f"only {len(unique_dates)} dates; need at least {minimum} for purged calibration"
        )
    calibration_start_index = len(unique_dates) - calibration_sessions
    provisional_end_index = calibration_start_index - horizon_sessions - 1
    provisional_end = unique_dates[provisional_end_index]
    calibration_start = unique_dates[calibration_start_index]
    fit_mask = dates <= provisional_end
    calibration_mask = dates >= calibration_start
    if int(fit_mask.sum()) < 10_000 or int(calibration_mask.sum()) < 1_000:
        raise ValueError("purged calibration split is too small")
    provisional_y = training_targets(rows, reward)
    if clip_quantile:
        provisional_lower, provisional_upper = np.quantile(
            provisional_y[fit_mask], [clip_quantile, 1.0 - clip_quantile]
        )
        provisional_y = np.clip(provisional_y, provisional_lower, provisional_upper)
    provisional_boosters, _ = fit_lightgbm_ensemble(
        x[fit_mask],
        provisional_y[fit_mask],
        weights[fit_mask],
        features,
        objective,
        seeds,
    )
    raw = sum(booster.predict(x[calibration_mask]) for booster in provisional_boosters) / seeds
    calibration_rows = [row for row, keep in zip(rows, calibration_mask) if keep]
    predicted = predictions_in_return_units(raw, calibration_rows, reward, reward_scale)
    realised = return_targets(calibration_rows, reward)
    calibration_weights = weights[calibration_mask]
    total_weight = float(calibration_weights.sum())
    predicted_mean = float(np.sum(calibration_weights * predicted) / total_weight)
    realised_mean = float(np.sum(calibration_weights * realised) / total_weight)
    centered = predicted - predicted_mean
    denominator = float(np.sum(calibration_weights * centered * centered))
    if denominator <= 0.0:
        slope = 0.0
    else:
        slope = float(
            np.sum(calibration_weights * centered * (realised - realised_mean))
            / denominator
        )
    slope = min(1.0, max(0.0, slope))
    intercept = realised_mean - slope * predicted_mean
    residual = realised - (intercept + slope * predicted)
    residual_standard_deviation = float(
        np.sqrt(np.sum(calibration_weights * residual * residual) / total_weight)
    )
    if not np.isfinite([intercept, slope, residual_standard_deviation]).all():
        raise ValueError("purged calibration produced non-finite parameters")
    return {
        "method": CALIBRATION_METHOD,
        "start": calibration_start,
        "end": unique_dates[-1],
        "observations": int(calibration_mask.sum()),
        "intercept": intercept,
        "slope": slope,
        "residual_standard_deviation": residual_standard_deviation,
    }


def training_targets(rows, reward):
    if reward == "return_per_risk":
        return np.asarray([row["return_per_risk_target"] for row in rows], dtype=np.float64)
    if reward == "relative_return_per_risk":
        return np.asarray(
            [row["relative_return_per_risk_target"] for row in rows], dtype=np.float64
        )
    if reward == "relative_return":
        return np.asarray([row["relative_target"] for row in rows], dtype=np.float64)
    if reward == "relative_rank":
        return np.asarray([row["relative_rank_target"] for row in rows], dtype=np.float64)
    return np.asarray([row["target"] for row in rows], dtype=np.float64)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--from", dest="from_date", type=date.fromisoformat)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--reward", choices=REWARDS, default="absolute_return")
    parser.add_argument("--model-family", choices=MODEL_FAMILIES, default="lightgbm")
    parser.add_argument("--objective", choices=OBJECTIVES, default="l1")
    parser.add_argument("--seeds", type=int, default=1)
    parser.add_argument("--ridge-lambda", type=float, default=DEFAULT_RIDGE_LAMBDA)
    parser.add_argument(
        "--calibration-sessions",
        type=int,
        default=0,
        help="trailing decision sessions reserved for purged OOS affine calibration",
    )
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
    if args.calibration_sessions < 0:
        parser.error("--calibration-sessions must be non-negative")

    manifest, features, rows, x, y, weights, clip, reward_scale = load_matrix(
        args.matrix,
        args.through,
        args.reward,
        args.clip_quantile,
        args.from_date,
    )
    trees = []
    linear_intercept = None
    linear_weights = None
    ridge_lambda = None
    blend_weight = None
    calibration = None
    if args.calibration_sessions:
        if args.model_family != "lightgbm":
            parser.error("purged calibration currently requires the LightGBM family")
        calibration = fit_purged_calibration(
            rows,
            x,
            weights,
            features,
            args.reward,
            args.objective,
            args.seeds,
            reward_scale,
            args.calibration_sessions,
            int(manifest["horizon_sessions"]),
            args.clip_quantile,
        )
    if args.model_family in ("lightgbm", "hybrid"):
        _, trees = fit_lightgbm_ensemble(
            x, y, weights, features, args.objective, args.seeds
        )
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
        "calibration": calibration,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(document, sort_keys=True, indent=2) + "\n")
    print(
        f"wrote {args.out}: {len(rows):,} rows, {len(features)} Rust-owned inputs, "
        f"{args.model_family} {args.reward}/{args.objective}, {args.seeds} seed(s), "
        f"{args.from_date or 'inception'} through {args.through}"
    )


if __name__ == "__main__":
    main()
