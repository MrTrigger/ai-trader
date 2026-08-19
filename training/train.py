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
    if reward in ("per_risk", "per_risk_abs"):
        # per_risk: demean(ret/vol) within each date, from the raw columns
        # Rust emits. per_risk_abs: ret/vol NOT demeaned - the sign carries
        # market direction, which a demeaned or ranked label throws away by
        # construction (docs/research/absolute-label-unbalanced.md). Rows
        # without a vol estimate cannot join either reward and are dropped
        # from the FIT only - inference still scores every eligible name.
        from collections import defaultdict

        byday = defaultdict(list)
        for row in rows:
            if row.get("vol"):
                byday[(row["date"], row.get("slot", 0))].append(row)
        kept = []
        for day_rows in byday.values():
            mean = sum(r["raw_ret"] / r["vol"] for r in day_rows) / len(day_rows)
            if reward == "per_risk_abs":
                mean = 0.0
            for r in day_rows:
                r["_y"] = r["raw_ret"] / r["vol"] - mean
                kept.append(r)
        rows = kept
        if len(rows) < 3_000:
            raise ValueError(f"only {len(rows)} vol-bearing rows; refusing a small fit")
    else:
        for row in rows:
            row["_y"] = row["target"]
    # Ordered lookup only. Values have already been computed, imputed, and
    # normalised by Rust; there is intentionally no transform here.
    x = np.asarray([[row["features"][name] for name in features] for row in rows])
    y = np.asarray([row["_y"] for row in rows])
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

    # TRAIN_RANK=1: replace the reward with its within-date uniform rank in
    # [-1, 1]. Composes with --reward per_risk (rank of return-per-risk), and
    # the per_risk artefact tag keeps inference converting score*vol exactly
    # once, so the cost threshold still bites in return units.
    if os.environ.get("TRAIN_RANK") == "1":
        from collections import defaultdict
        byday = defaultdict(list)
        for i, row in enumerate(rows):
            byday[(row["date"], row.get("slot", 0))].append(i)
        for ixs in byday.values():
            order = sorted(ixs, key=lambda i: y[i])
            n = max(1, len(order) - 1)
            for r, i in enumerate(order):
                y[i] = 2.0 * r / n - 1.0
        print("reward replaced by within-date uniform rank")

    n_seeds = int(os.environ.get("TRAIN_SEEDS", "1"))
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
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes((json.dumps(document, sort_keys=True, indent=2) + "\n").encode())
    print(f"wrote {args.out}: {len(rows):,} rows, {len(features)} Rust-owned features")


if __name__ == "__main__":
    main()
