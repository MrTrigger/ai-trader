import json
import sys
from datetime import date, timedelta

import pytest

import train_directional as trainer


def matrix_rows(count=650):
    start = date(2020, 1, 1)
    return [
        {
            "date": (start + timedelta(days=i)).isoformat(),
            "features": {name: (i % (j + 3)) / (j + 3) for j, name in enumerate(trainer.SELECTION_FEATURES)},
            "labels": {"fwd_z_7": (i % 19 - 9) / 10},
        }
        for i in range(count)
    ]


def test_profile_is_explicit_and_original_defaults_stay_unchanged():
    features, params, rounds = trainer.profile_settings("original", "fwd_z_30", ["dvol"])
    assert features == ["dvol"]
    assert (params["num_leaves"], params["min_data_in_leaf"], rounds) == (8, 30, 300)
    features, params, rounds = trainer.profile_settings(trainer.SELECTION_PROFILE, "fwd_z_7", trainer.SELECTION_FEATURES + ["dvol"])
    assert features == trainer.SELECTION_FEATURES
    assert (params["num_leaves"], params["min_data_in_leaf"], rounds) == (2, 180, 50)
    with pytest.raises(ValueError, match="fwd_z_7"):
        trainer.profile_settings(trainer.SELECTION_PROFILE, "fwd_z_14", features)


def test_purge_and_calendar_continuity_prevent_future_labels():
    through = date(2021, 8, 1)
    rows = matrix_rows()
    prefix = trainer.training_prefix(rows, through, 7, "fwd_z_7")
    assert date.fromisoformat(prefix[-1]["date"]) == through - timedelta(days=9)
    assert date.fromisoformat(prefix[-1]["date"]) + timedelta(days=7) == through - timedelta(days=2)
    with pytest.raises(ValueError, match="consecutive"):
        trainer.training_prefix(rows[:100] + rows[101:], through, 7, "fwd_z_7")
    with pytest.raises(ValueError, match="strictly increasing"):
        trainer.training_prefix(rows[:100] + [rows[99]] + rows[100:], through, 7, "fwd_z_7")


def test_future_targets_and_features_do_not_change_fitted_model(tmp_path, monkeypatch):
    through = date(2021, 8, 1)
    rows = matrix_rows()
    matrix = tmp_path / "matrix.jsonl"
    output = tmp_path / "model.json"
    monkeypatch.setattr(sys, "argv", ["train_directional.py", "--matrix", str(matrix), "--label", "fwd_z_7",
                                     "--through", through.isoformat(), "--out", str(output),
                                     "--profile", trainer.SELECTION_PROFILE])
    def fit():
        manifest = {"kind": "market-matrix", "features": trainer.SELECTION_FEATURES}
        matrix.write_text("\n".join(json.dumps(row) for row in [manifest, *rows]) + "\n")
        trainer.main()
        return json.loads(output.read_text())
    before = fit()
    for row in rows:
        if date.fromisoformat(row["date"]) > through - timedelta(days=9):
            row["labels"]["fwd_z_7"] = 999999
            row["features"] = {name: -999999 for name in trainer.SELECTION_FEATURES}
    after = fit()
    assert before["tree_info"] == after["tree_info"]
    assert before["label_std"] == after["label_std"]
    assert before["matrix_sha256"] != after["matrix_sha256"]
    assert after["training_label_end"] == (through - timedelta(days=2)).isoformat()
