"""Fit the round-2 market-timing model (docs/crypto-directional-design.md V4a).

Input: the Rust-built market matrix (crypto-portfolio directional-matrix).
Python fits LightGBM and dumps JSON; features and inference live in Rust.
Parameters are fixed by the pre-registration - small trees, heavy
regularisation, NaN-native - not tuned per fold.
"""
import argparse
import json
from datetime import date
from pathlib import Path

import lightgbm as lgb
import numpy as np

PARAMS = {
    "objective": "regression",
    "num_leaves": 8,
    "learning_rate": 0.03,
    "min_data_in_leaf": 30,
    "feature_fraction": 0.8,
    "bagging_fraction": 0.8,
    "bagging_freq": 1,
    "lambda_l2": 1.0,
    "verbosity": -1,
    "seed": 7,
    "deterministic": True,
    "force_row_wise": True,
    "use_missing": True,
    "zero_as_missing": False,
}
N_TREES = 300
MIN_ROWS = 400


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--matrix", type=Path, required=True)
    ap.add_argument("--label", choices=["fwd_z_7", "fwd_z_14", "fwd_z_30"], required=True)
    ap.add_argument("--through", type=date.fromisoformat, required=True)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()

    with args.matrix.open() as fh:
        manifest = json.loads(fh.readline())
        if manifest.get("kind") != "market-matrix":
            raise ValueError("not a market matrix")
        features = manifest["features"]
        rows = [json.loads(line) for line in fh if line.strip()]

    # The label at date D looks up to h days FORWARD; a row is usable for a
    # fit "through" C only if D + h <= C, or the label contains the test
    # window. Purge by shifting the cutoff back by the label horizon + 2-day
    # margin (the ranker's purge convention).
    horizon = int(args.label.rsplit("_", 1)[1])
    cutoff = args.through.toordinal() - horizon - 2
    keep = [
        r
        for r in rows
        if args.label in r["labels"] and date.fromisoformat(r["date"]).toordinal() <= cutoff
    ]
    if len(keep) < MIN_ROWS:
        raise ValueError(f"only {len(keep)} labeled rows through the purged cutoff; refusing a small fit")
    x = np.array(
        [[r["features"].get(f, float("nan")) for f in features] for r in keep], dtype=float
    )
    y = np.array([r["labels"][args.label] for r in keep], dtype=float)
    booster = lgb.train(PARAMS, lgb.Dataset(x, label=y, feature_name=features), num_boost_round=N_TREES)
    dump = booster.dump_model()
    doc = {
        "format_version": "crypto-lightgbm-json-1",
        "model_version": "market-timing-1",
        "features": features,
        "label": args.label,
        "label_std": float(np.std(y, ddof=1)),
        "trained_through": args.through.isoformat(),
        "n_rows": len(keep),
        "tree_info": dump["tree_info"],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(doc, sort_keys=True) + "\n")
    print(f"wrote {args.out}: {len(keep)} rows through {args.through} (purged {horizon}+2d), label_std {doc['label_std']:.3f}")


if __name__ == "__main__":
    main()
