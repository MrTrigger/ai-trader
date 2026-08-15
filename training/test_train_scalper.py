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
    manifest, matrix = train_scalper.load_matrix(path)
    X, y = train_scalper.prepare(matrix, names, 30, through_ts=10**12)
    booster = train_scalper.fit(X, y, names)
    imp = dict(zip(names, booster.feature_importance(importance_type="gain")))
    top2 = sorted(imp, key=imp.get, reverse=True)[:2]
    assert set(top2) == {"f0", "f1"}


def test_through_ts_excludes_the_future(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    _, matrix = train_scalper.load_matrix(path)
    cutoff = 1_700_000_000 + 30_000 * 300
    X, y = train_scalper.prepare(matrix, names, 30, through_ts=cutoff)
    assert len(X) == 30_001  # ts <= cutoff is inclusive: indices 0..=30000


def test_too_small_matrix_is_refused(tmp_path):
    path, names = synthetic_matrix(tmp_path, n=1_000)
    _, matrix = train_scalper.load_matrix(path)
    with pytest.raises(ValueError):
        train_scalper.prepare(matrix, names, 30, through_ts=10**12)


def test_prepare_sorts_asset_blocked_rows_into_chronological_order(tmp_path):
    # The real matrix producer writes one asset block at a time - all of an
    # asset's rows in ts order, but block B's timestamps can be interleaved
    # with (even predate) block A's. prepare() must restore a single global
    # chronological order, or fit()'s "last 10%" early-stopping split would
    # leak rows from the future into training and the past into validation.
    names = ["f0"]
    path = tmp_path / "blocked.jsonl"
    with open(path, "w") as fh:
        fh.write(json.dumps({"kind": "manifest", "feature_set_version": "fs-test",
                             "features": names, "horizons_min": [30], "stride_min": 5,
                             "assets": ["A", "B"]}) + "\n")
        base = 1_700_000_000
        half = 30_000  # 60,000 rows total, clearing the 50,000-row MIN_ROWS gate
        # Asset block A: even ts offsets 0, 2, 4, ...
        for i in range(half):
            ts = base + 2 * i * 300
            fh.write(json.dumps({"ts": ts, "asset": "A",
                                 "features": {"f0": 0.0},
                                 "fwd_bps": {"30": ts / 1_000_000}}) + "\n")
        # Asset block B: odd ts offsets 1, 3, 5, ... - fully interleaved with A
        for i in range(half):
            ts = base + (2 * i + 1) * 300
            fh.write(json.dumps({"ts": ts, "asset": "B",
                                 "features": {"f0": 0.0},
                                 "fwd_bps": {"30": ts / 1_000_000}}) + "\n")
    _, matrix = train_scalper.load_matrix(path)
    x, y = train_scalper.prepare(matrix, names, 30, through_ts=10**12)
    assert list(y) == sorted(y)


def test_matrix_is_columnar_and_prepare_matches_dict_reference(tmp_path):
    path, names = synthetic_matrix(tmp_path, n=60_000, seed=3)
    manifest, matrix = train_scalper.load_matrix(path)

    assert matrix.x.dtype == np.float64
    assert matrix.x.shape == (60_000, len(names))
    assert matrix.ts.shape == (60_000,)
    assert matrix.ts.dtype == np.int64

    # Reference: the original dict-of-rows computation, done independently
    # of load_matrix/prepare, on the same file.
    with open(path) as fh:
        next(fh)  # manifest line
        rows = [json.loads(line) for line in fh if line.strip()]
    through_ts = 1_700_000_000 + 40_000 * 300
    kept = [row for row in rows if row["ts"] <= through_ts]
    kept.sort(key=lambda row: row["ts"])
    ref_x = np.asarray([[row["features"][name] for name in names] for row in kept])
    ref_y = np.asarray([row["fwd_bps"]["30"] for row in kept], dtype=float)
    lo, hi = np.percentile(ref_y, [0.5, 99.5])
    ref_y = np.clip(ref_y, lo, hi)

    x, y = train_scalper.prepare(matrix, names, 30, through_ts=through_ts)
    assert x.shape == ref_x.shape
    np.testing.assert_array_equal(x, ref_x)
    np.testing.assert_array_equal(y, ref_y)


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
