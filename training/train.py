"""Fit LightGBM on a matrix finalized by the Rust feature engine.

This file deliberately contains no feature calculation or preprocessing. Its
input is `crypto-portfolio training-matrix` JSONL: ordered, rank-normalised
model inputs and the final relative-return target. Python's only job is fitting
trees and exporting them to the auditable Rust runtime format.
"""

from __future__ import annotations

import argparse
import os
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
    "num_threads": 8,
    "deterministic": True,
    "force_col_wise": True,
}
NUM_ROUNDS = 300


def load_matrix(path: Path, through: date, reward: str = "return"):
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        if manifest.get("kind") != "manifest":
            raise ValueError("first matrix row is not the Rust feature manifest")
        features = manifest["features"]
        rows = [json.loads(line) for line in source if line.strip()]
    rows = [row for row in rows if date.fromisoformat(row["date"]) <= through]
    if len(rows) < 3_000:
        raise ValueError(f"only {len(rows)} rows through {through}; refusing a small fit")
    contract = manifest.get("target_contract")
    if contract is not None and contract != "rust-crypto-targets-1":
        raise ValueError(f"unsupported Rust target contract: {contract}")
    if contract:
        horizon = manifest.get("label_horizon_hours")
        lag = manifest.get("label_lag_hours")
        if type(horizon) is not int or horizon <= 0 or type(lag) is not int or lag < 0:
            raise ValueError("Rust target contract requires valid horizon and lag metadata")
    target_field = {"return": "target", "per_risk": "target_per_risk",
                    "per_risk_abs": "target_per_risk_abs"}[reward]
    if reward != "return" and not contract:
        raise ValueError("per-risk fits require Rust-finalized targets; rebuild training-matrix")
    rows = [row for row in rows if row[target_field] is not None]
    if len(rows) < 3_000:
        raise ValueError(f"only {len(rows)} labeled rows; refusing a small fit")
    # Ordered lookup only. Values have already been computed, imputed, and
    # normalised by Rust; there is intentionally no transform here.
    x = np.asarray([[row["features"][name] for name in features] for row in rows])
    y = np.asarray([row[target_field] for row in rows])
    if not np.isfinite(x).all() or not np.isfinite(y).all():
        raise ValueError("matrix contains nonfinite inputs or targets")
    return manifest, features, rows, x, y


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--reward",
        choices=["return", "per_risk", "per_risk_abs"],
        default="return",
        help="what the trees predict: demean(ret), demean(ret/vol), or ret/vol "
        "not demeaned (per_risk_abs - the sign carries market direction). "
        "Recorded on the artefact so inference multiplies vol back exactly "
        "once for both per-risk forms.",
    )
    args = parser.parse_args()

    manifest, features, rows, x, y = load_matrix(args.matrix, args.through, args.reward)

    # TRAIN_DROP: comma-separated features to exclude from the fit. The model
    # artefact names its features, so inference follows automatically; the
    # matrix stays complete and the experiment lives entirely here.
    drop = {f.strip() for f in os.environ.get("TRAIN_DROP", "").split(",") if f.strip()}
    if drop:
        keep_ix = [i for i, f in enumerate(features) if f not in drop]
        features = [features[i] for i in keep_ix]
        x = x[:, keep_ix]
        print(f"dropped {len(drop)}, fitting on {len(features)} features")

    # TRAIN_SEEDS: fit N boosters differing only in their sampling seeds and
    # average them. A sum-of-trees model averages exactly by concatenating the
    # trees with every leaf scaled by 1/N, so the artefact format is unchanged
    # and Rust inference needs no concept of an ensemble.
    # TRAIN_OBJECTIVE: l2 (default) / l1 / huber. The target is skewed +2.6
    # with kurtosis 45; an L2 loss spends most of its gradient on the tail.
    objective = os.environ.get("TRAIN_OBJECTIVE", "regression")
    alias = {"l2": "regression", "l1": "regression_l1", "huber": "huber"}
    objective = alias.get(objective, objective)

    if os.environ.get("TRAIN_RANK", "") not in ("", "0"):
        raise ValueError("Python target transformations are disabled; emit the final target in Rust")

    n_seeds = int(os.environ.get("TRAIN_SEEDS", "1"))
    if n_seeds < 1:
        raise ValueError("TRAIN_SEEDS must be positive")
    def scale_leaves(node, k):
        if "leaf_value" in node and node["leaf_value"] is not None:
            node["leaf_value"] *= k
        for side in ("left_child", "right_child"):
            if isinstance(node.get(side), dict):
                scale_leaves(node[side], k)
    trees = []
    for seed in range(n_seeds):
        params = dict(PARAMS, objective=objective, seed=seed,
                      bagging_seed=seed + 100, feature_fraction_seed=seed + 200)
        booster = lgb.train(params, lgb.Dataset(x, label=y), num_boost_round=NUM_ROUNDS)
        dumped = booster.dump_model()["tree_info"]
        if n_seeds > 1:
            for t in dumped:
                scale_leaves(t["tree_structure"], 1.0 / n_seeds)
        trees.extend(dumped)
    dump = {"tree_info": trees}
    document = {
        "format_version": FORMAT_VERSION,
        "model_version": MODEL_VERSION,
        "feature_set_version": manifest["feature_set_version"],
        "trained_through": args.through.isoformat(),
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "n_rows": len(rows),
        "n_dates": len({row["date"] for row in rows}),
        "features": features,
        "reward": args.reward,
        "tree_info": dump["tree_info"],
    }
    if manifest.get("target_contract"):
        for key in ("target_contract", "label_horizon_hours", "label_lag_hours"):
            document[key] = manifest[key]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes((json.dumps(document, sort_keys=True, indent=2) + "\n").encode())
    print(f"wrote {args.out}: {len(rows):,} rows, {len(features)} Rust-owned features")


if __name__ == "__main__":
    main()
