import json

import pytest

import train_scalper
import walk_forward_scalper
from test_train_scalper import synthetic_matrix


def test_folds_are_chronological_purged_and_dumped(tmp_path):
    path, names = synthetic_matrix(tmp_path, n=120_000)  # 300s stride → ~416 days
    out = tmp_path / "folds"
    walk_forward_scalper.main(["--matrix", str(path), "--horizon", "30",
                               "--out-dir", str(out)])
    spec = json.loads((out / "folds.json").read_text())
    folds = spec["folds"]
    assert len(folds) >= 2
    for f in folds:
        assert f["train_end_ts"] <= f["test_start_ts"]
        assert (out / f["model"]).exists()
        art = json.loads((out / f["model"]).read_text())
        assert art["features"] == names
    # steps advance by test_days
    assert folds[1]["test_start_ts"] - folds[0]["test_start_ts"] == 30 * 86400


def test_embargo_drops_training_rows_inside_the_horizon(tmp_path):
    # prepare() must never see a training row whose label window crosses train_end.
    path, names = synthetic_matrix(tmp_path, n=60_000)
    _, rows = train_scalper.load_matrix(path)
    kept = walk_forward_scalper.training_slice(rows, train_start_ts=rows[0]["ts"],
                                               train_end_ts=rows[-1]["ts"], horizon_min=30)
    assert max(r["ts"] for r in kept) <= rows[-1]["ts"] - 30 * 60


def test_all_folds_skipped_is_a_loud_failure(tmp_path):
    path, _ = synthetic_matrix(tmp_path, n=60_000)  # ~208 days, but train floor will pass — shrink instead
    (tmp_path / "s").mkdir()
    small, _ = synthetic_matrix(tmp_path / "s", n=52_000)
    with pytest.raises(SystemExit):
        walk_forward_scalper.main(["--matrix", str(small), "--horizon", "30",
                                   "--out-dir", str(tmp_path / "o"),
                                   "--train-days", "400"])
