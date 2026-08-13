import json
from datetime import date

import pytest

from blend_stockholm_models import blend_documents
from train_stockholm import fit_ridge, load_matrix


def write_matrix(path, rows=10_000, unlabelled=0):
    with path.open("w", encoding="utf-8") as target:
        target.write(
            json.dumps(
                {
                    "kind": "stockholm_training_manifest",
                    "feature_set_version": "fixture",
                    "label_version": "fixture-label",
                    "features": ["x_ret_5", "m_ret_5"],
                    "survivorship_status": "SURVIVORSHIP_CONTAMINATED",
                }
            )
            + "\n"
        )
        for index in range(rows + unlabelled):
            # Rust emits the whole decision-date cross-section, so the tail of
            # this fixture is members whose forward outcome was never observed.
            labelled = index < rows
            target.write(
                json.dumps(
                    {
                        "date": "2020-01-02",
                        "instrument_id": f"TX{index}",
                        "target": 0.01 if labelled else None,
                        "market_target": 0.006,
                        "relative_target": 0.004 if labelled else None,
                        "return_per_risk_target": 0.5 if labelled else None,
                        "relative_return_per_risk_target": 0.2 if labelled else None,
                        "relative_rank_target": 0.5 if labelled else None,
                        "vol60": 0.02,
                        "sample_weight": 0.001,
                        "features": {"x_ret_5": -0.5, "m_ret_5": 0.0},
                    }
                )
                + "\n"
            )


def test_loader_only_assembles_final_rust_values(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    _, features, rows, x, y, weights, clip, reward_scale, dropped = load_matrix(
        path, date(2020, 1, 2), clip_quantile=0.0
    )
    assert features == ["x_ret_5", "m_ret_5"]
    assert len(rows) == 10_000
    assert list(x[0]) == [-0.5, 0.0]
    assert y[0] == 0.01
    assert weights[0] == 0.001
    assert clip is None
    assert reward_scale is None
    assert dropped == 0


def test_return_per_risk_preserves_direction_and_units(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    _, _, _, _, y, _, _, _, _ = load_matrix(
        path,
        date(2020, 1, 2),
        reward="return_per_risk",
        clip_quantile=0.0,
    )
    assert y[0] == 0.5


def test_relative_return_is_precomputed_by_rust(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    _, _, _, _, y, _, _, _, _ = load_matrix(
        path,
        date(2020, 1, 2),
        reward="relative_return",
        clip_quantile=0.0,
    )
    assert y[0] == 0.004


def test_relative_return_per_risk_is_precomputed_by_rust(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    _, _, _, _, y, _, _, _, _ = load_matrix(
        path,
        date(2020, 1, 2),
        reward="relative_return_per_risk",
        clip_quantile=0.0,
    )
    assert y[0] == 0.2


def test_relative_rank_is_precomputed_and_calibrated_to_return_units(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    _, _, _, _, y, _, clip, reward_scale, _ = load_matrix(
        path,
        date(2020, 1, 2),
        reward="relative_rank",
        clip_quantile=0.0,
    )
    assert y[0] == 0.5
    assert clip is None
    assert reward_scale == pytest.approx(0.008)


@pytest.mark.parametrize(
    "reward",
    [
        "absolute_return",
        "return_per_risk",
        "relative_return",
        "relative_return_per_risk",
        "relative_rank",
    ],
)
def test_loader_drops_unlabelled_cross_section_members(tmp_path, reward):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path, unlabelled=7)
    _, _, rows, x, y, weights, _, _, dropped = load_matrix(
        path, date(2020, 1, 2), reward=reward, clip_quantile=0.0
    )
    assert dropped == 7
    assert len(rows) == 10_000
    assert len(y) == len(x) == len(weights) == 10_000


def test_loader_refuses_a_matrix_whose_labels_are_all_unobserved(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path, rows=0, unlabelled=10_001)
    with pytest.raises(ValueError, match="10001 unlabelled"):
        load_matrix(path, date(2020, 1, 2))


def test_loader_refuses_small_fit(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path, rows=2)
    with pytest.raises(ValueError, match="refusing a small fit"):
        load_matrix(path, date(2020, 1, 2))


def test_weighted_ridge_exports_intercept_and_coefficients():
    x = __import__("numpy").asarray(
        [[-2.0, 1.0], [-1.0, 1.0], [1.0, 1.0], [2.0, 1.0]]
    )
    y = 0.25 + 0.5 * x[:, 0]
    weights = __import__("numpy").ones(4)
    intercept, coefficients = fit_ridge(x, y, weights, penalty=1e-9)
    assert intercept == pytest.approx(0.25)
    assert coefficients[0] == pytest.approx(0.5)
    assert coefficients[1] == pytest.approx(0.0)


def test_model_blend_remaps_features_and_scales_leaf_values():
    def document(features, split_feature):
        return {
            "format_version": "stockholm-model-json-2",
            "model_version": "stockholm-ranker-1",
            "feature_set_version": f"fs-{len(features)}",
            "label_version": "forward-adjusted-open-20-v1",
            "trained_through": "2024-01-01",
            "survivorship_status": "SURVIVORSHIP_CONTAMINATED",
            "model_family": "lightgbm",
            "reward": "absolute_return",
            "objective": "l2",
            "target_clip": [-0.2, 0.2],
            "reward_scale": None,
            "calibration": None,
            "features": features,
            "tree_info": [
                {
                    "tree_index": 0,
                    "tree_structure": {
                        "split_feature": split_feature,
                        "left_child": {"leaf_value": 2.0},
                        "right_child": {"leaf_value": -2.0},
                    },
                }
            ],
        }

    lean = document(["b"], 0)
    rich = document(["a", "b"], 0)
    result = blend_documents(
        [lean, rich], ["a" * 64, "b" * 64], [0.5, 0.5], 1
    )
    assert result["features"] == ["a", "b"]
    assert result["tree_info"][0]["tree_structure"]["split_feature"] == 1
    assert result["tree_info"][1]["tree_structure"]["split_feature"] == 0
    assert result["tree_info"][0]["tree_structure"]["left_child"]["leaf_value"] == 1.0
    assert [tree["tree_index"] for tree in result["tree_info"]] == [0, 1]
