import json

import numpy as np
import pytest

import train_scalper


def synthetic_matrix(tmp_path, n=60_000, seed=0):
    rng = np.random.default_rng(seed)
    names = [f"f{i}" for i in range(26)]
    path = tmp_path / "m.jsonl"
    with open(path, "w") as fh:
        fh.write(json.dumps({"kind": "manifest", "feature_set_version": "fs-test",
                             "features": names, "horizons_min": [30], "stride_min": 5,
                             "assets": ["A", "B"]}) + "\n")
        for i in range(n):
            f = rng.normal(size=26)
            y = 3 * f[0] - 2 * f[1] + rng.normal(scale=0.5)
            fh.write(json.dumps({"ts": 1_700_000_000 + i * 300,
                                 "asset": "A" if i % 2 else "B",
                                 "features": dict(zip(names, f.round(6).tolist())),
                                 "fwd_bps": {"30": round(float(y), 6)}}) + "\n")
    return path, names


def test_fit_learns_a_planted_signal(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    manifest, rows = train_scalper.load_matrix(path)
    X, y = train_scalper.prepare(rows, names, 30, through_ts=10**12)
    booster = train_scalper.fit(X, y, names)
    imp = dict(zip(names, booster.feature_importance(importance_type="gain")))
    top2 = sorted(imp, key=imp.get, reverse=True)[:2]
    assert set(top2) == {"f0", "f1"}


def test_through_ts_excludes_the_future(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    _, rows = train_scalper.load_matrix(path)
    cutoff = 1_700_000_000 + 30_000 * 300
    X, y = train_scalper.prepare(rows, names, 30, through_ts=cutoff)
    assert len(X) == 30_001  # ts <= cutoff is inclusive: indices 0..=30000


def test_too_small_matrix_is_refused(tmp_path):
    path, names = synthetic_matrix(tmp_path, n=1_000)
    _, rows = train_scalper.load_matrix(path)
    with pytest.raises(ValueError):
        train_scalper.prepare(rows, names, 30, through_ts=10**12)


def test_artifact_envelope_roundtrips(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    out = tmp_path / "model.json"
    train_scalper.main(["--matrix", str(path), "--horizon", "30",
                        "--through", "2262-01-01", "--out", str(out)])
    art = json.loads(out.read_text())
    assert art["format_version"] == "crypto-lightgbm-json-1"
    assert art["features"] == names
    assert art["horizon_min"] == 30
    assert "tree_info" in art["model"]
