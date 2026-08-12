"""Compose frozen Stockholm LightGBM artefacts without feature engineering.

Every input model has already been trained. This orchestration step remaps
tree feature indexes to one declared superset contract and scales leaf values
so Rust inference evaluates the exact weighted prediction average.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path


def remap_and_scale_tree(node, source_features, target_indexes, weight):
    if node.get("leaf_value") is not None:
        node["leaf_value"] *= weight
    if node.get("split_feature") is not None:
        source_index = int(node["split_feature"])
        node["split_feature"] = target_indexes[source_features[source_index]]
    for side in ("left_child", "right_child"):
        if isinstance(node.get(side), dict):
            remap_and_scale_tree(
                node[side], source_features, target_indexes, weight
            )


def blend_documents(documents, digests, weights, target_index):
    if len(documents) < 2 or len(documents) != len(weights):
        raise ValueError("a blend requires matching lists of at least two models")
    if any(not weight > 0.0 for weight in weights) or abs(sum(weights) - 1.0) > 1e-12:
        raise ValueError("blend weights must be positive and sum to one")
    target = documents[target_index]
    required_equal = (
        "format_version",
        "model_version",
        "label_version",
        "trained_through",
        "survivorship_status",
        "model_family",
        "reward",
        "objective",
        "target_clip",
        "reward_scale",
        "calibration",
    )
    if target.get("model_family") != "lightgbm" or not target.get("tree_info"):
        raise ValueError("the target must be a non-empty LightGBM model")
    target_features = target["features"]
    target_indexes = {name: index for index, name in enumerate(target_features)}
    if len(target_indexes) != len(target_features):
        raise ValueError("target model feature names are not unique")
    trees = []
    components = []
    for document, digest, weight in zip(documents, digests, weights):
        for field in required_equal:
            if document.get(field) != target.get(field):
                raise ValueError(f"component models differ on {field}")
        if any(feature not in target_indexes for feature in document["features"]):
            raise ValueError("target model is not a feature superset")
        for tree in copy.deepcopy(document["tree_info"]):
            remap_and_scale_tree(
                tree["tree_structure"],
                document["features"],
                target_indexes,
                weight,
            )
            tree["tree_index"] = len(trees)
            trees.append(tree)
        components.append(
            {
                "sha256": digest,
                "feature_set_version": document["feature_set_version"],
                "weight": weight,
            }
        )
    output = copy.deepcopy(target)
    output["tree_info"] = trees
    output["blend_components"] = components
    return output


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", type=Path, action="append", required=True)
    parser.add_argument("--weight", type=float, action="append", required=True)
    parser.add_argument("--target-index", type=int, default=-1)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    raw = [path.read_bytes() for path in args.model]
    documents = [json.loads(value) for value in raw]
    digests = [hashlib.sha256(value).hexdigest() for value in raw]
    target_index = args.target_index % len(documents)
    output = blend_documents(documents, digests, args.weight, target_index)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(output, sort_keys=True, indent=2) + "\n")
    print(
        f"wrote {args.out}: {len(documents)} frozen components, "
        f"{len(output['tree_info'])} weighted trees, "
        f"target contract {output['feature_set_version']}"
    )


if __name__ == "__main__":
    main()
