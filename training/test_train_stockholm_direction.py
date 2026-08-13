import json
import sys
from datetime import date, timedelta

import lightgbm as lgb
import numpy as np
import pytest

import train_stockholm_direction as direction_trainer


def write_direction_matrix(path, rows=600, seed=1701):
    rng = np.random.default_rng(seed)
    with path.open("w", encoding="utf-8") as target:
        target.write(
            json.dumps(
                {
                    "kind": "stockholm_direction_training_manifest",
                    "feature_set_version": "fs-rust-stockholm-direction-3",
                    "label_version": "omxsgi-forward-close-20-v4",
                    "features": ["m_ret_5", "m_vol_20"],
                }
            )
            + "\n"
        )
        start = date(2016, 1, 1)
        for index in range(rows):
            # Independent, noisy inputs and target: this fixture only needs a
            # real booster to fit and predict, not a real economic signal.
            target.write(
                json.dumps(
                    {
                        "date": (start + timedelta(days=index)).isoformat(),
                        "target": float(rng.normal(0.0, 0.05)),
                        "sign_target": float(rng.choice([-1.0, 0.0, 1.0])),
                        "features": {
                            "m_ret_5": float(rng.normal()),
                            "m_vol_20": float(rng.normal()),
                        },
                    }
                )
                + "\n"
            )


def test_score_scale_is_prediction_spread_not_target_spread(tmp_path, monkeypatch):
    matrix_path = tmp_path / "direction-matrix.jsonl"
    write_direction_matrix(matrix_path)
    through = date(2016, 1, 1) + timedelta(days=10_000)  # comfortably past every row
    out_path = tmp_path / "model.json"

    monkeypatch.setattr(
        sys,
        "argv",
        [
            "train_stockholm_direction.py",
            "--matrix",
            str(matrix_path),
            "--through",
            through.isoformat(),
            "--out",
            str(out_path),
            "--clip-quantile",
            "0",
        ],
    )
    direction_trainer.main()
    document = json.loads(out_path.read_text())

    # Independently refit the identical booster (same data, same fixed seeds
    # in PARAMS) to recover its train-set predictions and their std, which is
    # what the exported score_scale must equal.
    _, features, _, x, y, absolute_y, _ = direction_trainer.load_training_prefix(
        matrix_path, through, clip_quantile=0.0
    )
    dataset = lgb.Dataset(x, label=y, feature_name=features)
    objective, metric = direction_trainer.OBJECTIVES["l2"]
    booster = lgb.train(
        dict(direction_trainer.PARAMS, objective=objective, metric=metric),
        dataset,
        num_boost_round=direction_trainer.NUM_ROUNDS,
    )
    expected_scale = float(np.std(booster.predict(x)))
    target_scale = float(np.std(absolute_y))

    assert document["score_scale"] == pytest.approx(expected_scale, rel=1e-6)
    # The old, buggy convention exported std(target) instead; guard against
    # silently reverting to it.
    assert document["score_scale"] != pytest.approx(target_scale, rel=0.2)
