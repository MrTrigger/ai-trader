import json
from datetime import date

import pytest

from train import load_matrix


def write_matrix(path, rows=3_000):
    with path.open("w", encoding="utf-8") as target:
        target.write(
            json.dumps(
                {
                    "kind": "manifest",
                    "feature_set_version": "fixture",
                    "features": ["x_ret_7", "x_rv_24h"],
                }
            )
            + "\n"
        )
        for index in range(rows):
            target.write(
                json.dumps(
                    {
                        "date": "2025-01-01",
                        "asset": f"A{index}",
                        "target": 0.01,
                        "features": {"x_ret_7": -0.5, "x_rv_24h": 0.25},
                    }
                )
                + "\n"
            )


def test_loader_only_assembles_rust_finalized_values(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    manifest, features, rows, x, y = load_matrix(path, date(2025, 1, 1))
    assert manifest["feature_set_version"] == "fixture"
    assert features == ["x_ret_7", "x_rv_24h"]
    assert x.shape == (3_000, 2)
    assert list(x[0]) == [-0.5, 0.25]
    assert y[0] == 0.01
    assert len(rows) == 3_000


def test_loader_refuses_a_small_fit(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path, rows=2)
    with pytest.raises(ValueError, match="refusing a small fit"):
        load_matrix(path, date(2025, 1, 1))
