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


def finalize_fixture(path, *, contract="rust-crypto-targets-1", missing=0):
    lines = [json.loads(line) for line in path.read_text().splitlines()]
    lines[0].update(target_contract=contract, label_horizon_hours=72, label_lag_hours=1)
    for index, row in enumerate(lines[1:]):
        row.update(target_per_risk=None if index < missing else -0.123,
                   target_per_risk_abs=None if index < missing else 0.456,
                   raw_ret=999, vol=999)
    path.write_text("\n".join(json.dumps(line) for line in lines) + "\n")


@pytest.mark.parametrize("reward, expected", [("per_risk", -0.123), ("per_risk_abs", 0.456)])
def test_loader_uses_final_targets_without_recomputing_raw_columns(tmp_path, reward, expected):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path, rows=3001)
    finalize_fixture(path, missing=1)
    manifest, _, rows, x, y = load_matrix(path, date(2025, 1, 1), reward)
    assert len(rows) == 3000
    assert rows[0]["asset"] == "A1"
    assert x.shape == (3000, 2)
    assert (y == expected).all()
    assert manifest["label_horizon_hours"] == 72


def test_per_risk_requires_finalized_contract(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    with pytest.raises(ValueError, match="rebuild training-matrix"):
        load_matrix(path, date(2025, 1, 1), "per_risk")
    finalize_fixture(path, contract="unknown")
    with pytest.raises(ValueError, match="unsupported Rust target contract"):
        load_matrix(path, date(2025, 1, 1), "per_risk")


def test_contract_requires_valid_horizon_metadata(tmp_path):
    path = tmp_path / "matrix.jsonl"
    write_matrix(path)
    finalize_fixture(path)
    lines = path.read_text().splitlines()
    manifest = json.loads(lines[0])
    manifest["label_horizon_hours"] = 0
    lines[0] = json.dumps(manifest)
    path.write_text("\n".join(lines) + "\n")
    with pytest.raises(ValueError, match="valid horizon and lag"):
        load_matrix(path, date(2025, 1, 1), "per_risk")
