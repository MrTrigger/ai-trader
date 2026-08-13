"""Emit the deterministic Rust/Python LightGBM parity fixture.

Builds a tiny (30-row) synthetic matrix with a fixed seed, fits a small
model via Task 3's `fit`, and dumps the exact triad Task 4b's Rust test
needs to prove `lightgbm-json` reproduces Python's own predictions bit for
bit: `model.json` (the house artifact envelope), `rows.jsonl` (raw feature
rows, same shape as a real training-matrix row), and `expected.json` (this
booster's own `predict()` on those rows, full float precision).

No feature computation, no simulation, no prediction consumed by anything
but this fixture - purely a generator script.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np

import train_scalper

SEED = 7
N_ROWS = 30
N_TREES = 20
FEATURE_NAMES = [f"f{i}" for i in range(26)]
FIXED_TS = 1_700_000_000
HORIZON_MIN = 30
FEATURE_SET_VERSION = "fs-parity-fixture-1"
TRAINED_THROUGH = "2026-01-01"
TRAINED_AT = "2026-01-01T00:00:00+00:00"

FIXTURE_DIR = (
    Path(__file__).resolve().parents[1]
    / "service"
    / "crates"
    / "scalper-data"
    / "tests"
    / "fixtures"
    / "parity"
)


def build_rows():
    """A deterministic 30-row matrix: seed 7, a mild planted signal so the
    fit isn't pure noise, house matrix-row shape (ts/asset/features/fwd_bps)."""
    rng = np.random.default_rng(SEED)
    x = rng.normal(size=(N_ROWS, len(FEATURE_NAMES)))
    y = 3 * x[:, 0] - 2 * x[:, 1] + rng.normal(scale=0.5, size=N_ROWS)
    rows = [
        {
            "ts": FIXED_TS + i * 300,
            "asset": "PARITY",
            "features": dict(zip(FEATURE_NAMES, x[i].tolist())),
            "fwd_bps": {str(HORIZON_MIN): float(y[i])},
        }
        for i in range(N_ROWS)
    ]
    return x, y, rows


def main() -> None:
    x, y, rows = build_rows()
    # PARAMS' production min_data_in_leaf (500) can't split a 30-row matrix
    # at all - it would collapse to a single trivial leaf and the Rust test
    # would never exercise real split traversal. Loosen just the leaf-size
    # constraints for this fixture fit; num_boost_round still caps the tree
    # count, and every other pinned hyperparameter (learning_rate, num_leaves,
    # seed, ...) stays exactly as production uses it.
    booster = train_scalper.fit(
        x, y, FEATURE_NAMES, num_boost_round=N_TREES,
        params_overrides={"min_data_in_leaf": 2, "min_data_in_bin": 1},
    )
    predictions = [float(p) for p in booster.predict(x)]

    document = {
        "format_version": train_scalper.FORMAT_VERSION,
        "model_version": train_scalper.MODEL_VERSION,
        "feature_set_version": FEATURE_SET_VERSION,
        "horizon_min": HORIZON_MIN,
        "trained_through": TRAINED_THROUGH,
        "trained_at": TRAINED_AT,
        "n_rows": N_ROWS,
        "features": FEATURE_NAMES,
        "label": "fwd_bps_winsorized",
        "model": booster.dump_model(),
    }

    FIXTURE_DIR.mkdir(parents=True, exist_ok=True)
    (FIXTURE_DIR / "model.json").write_text(
        json.dumps(document, sort_keys=True, indent=2) + "\n"
    )
    with (FIXTURE_DIR / "rows.jsonl").open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row, sort_keys=True) + "\n")
    (FIXTURE_DIR / "expected.json").write_text(json.dumps(predictions, indent=2) + "\n")
    print(f"wrote parity fixture ({N_ROWS} rows, {N_TREES}-tree cap) to {FIXTURE_DIR}")


if __name__ == "__main__":
    main()
