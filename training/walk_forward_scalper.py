"""Slice a scalper matrix into purged walk-forward folds and fit each one.

This file deliberately contains no feature calculation, no prediction, and
no simulation. It reuses Task 3's `train_scalper.load_matrix`, `prepare`,
and `fit` to turn a single training matrix into a sequence of fold model
artifacts. Everything else - backtesting, Sharpe, exposure - is Rust's job.
"""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path

import train_scalper

DEFAULT_TRAIN_DAYS = 90
DEFAULT_TEST_DAYS = 30
DEFAULT_STEP_DAYS = 30
SECONDS_PER_DAY = 86400


def training_slice(matrix, train_start_ts, train_end_ts, horizon_min):
    """Boolean mask (over `matrix`) of rows in [train_start_ts, train_end_ts],
    purged of the horizon window that would otherwise let a label peek across
    the train/test boundary."""
    cutoff = train_end_ts - horizon_min * 60
    return (matrix.ts >= train_start_ts) & (matrix.ts <= cutoff)


def _build_fold_document(booster, features, manifest, horizon, train_end_ts, n_rows):
    trained_through = datetime.fromtimestamp(train_end_ts, tz=timezone.utc).date().isoformat()
    return {
        "format_version": train_scalper.FORMAT_VERSION,
        "model_version": train_scalper.MODEL_VERSION,
        "feature_set_version": manifest["feature_set_version"],
        "horizon_min": horizon,
        "trained_through": trained_through,
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "n_rows": n_rows,
        "features": features,
        "label": "fwd_bps_winsorized",
        "model": booster.dump_model(),
    }


def main(argv=None) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--horizon", type=int, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument("--train-days", type=int, default=DEFAULT_TRAIN_DAYS)
    parser.add_argument("--test-days", type=int, default=DEFAULT_TEST_DAYS)
    parser.add_argument("--step-days", type=int, default=DEFAULT_STEP_DAYS)
    args = parser.parse_args(argv)

    manifest, matrix = train_scalper.load_matrix(args.matrix)
    if len(matrix) == 0:
        raise SystemExit(f"{args.matrix} has no rows; nothing to fold")
    features = manifest["features"]
    matrix_start_ts = int(matrix.ts.min())
    matrix_end_ts = int(matrix.ts.max())

    train_seconds = args.train_days * SECONDS_PER_DAY
    test_seconds = args.test_days * SECONDS_PER_DAY
    step_seconds = args.step_days * SECONDS_PER_DAY

    # Anchored expanding window: every fold trains from the start of the
    # matrix through an ever-later train_end, so the training slice grows
    # fold over fold instead of sliding a fixed-width window that would
    # never accumulate enough rows to clear the training floor.
    candidates = []
    i = 0
    while True:
        train_end_ts = matrix_start_ts + train_seconds + i * step_seconds
        test_start_ts = train_end_ts
        test_end_ts = test_start_ts + test_seconds
        if test_end_ts > matrix_end_ts:
            break
        candidates.append((train_end_ts, test_start_ts, test_end_ts))
        i += 1

    kept = []
    for train_end_ts, test_start_ts, test_end_ts in candidates:
        mask = training_slice(matrix, matrix_start_ts, train_end_ts, args.horizon)
        n_train = int(mask.sum())
        if n_train < train_scalper.MIN_ROWS:
            continue
        kept.append((train_end_ts, test_start_ts, test_end_ts))

    if not kept:
        raise SystemExit(
            f"every fold's purged training slice was below the "
            f"{train_scalper.MIN_ROWS:,}-row floor; nothing to train "
            f"(matrix span too short for --train-days {args.train_days})"
        )

    args.out_dir.mkdir(parents=True, exist_ok=True)
    fold_specs = []
    for idx, (train_end_ts, test_start_ts, test_end_ts) in enumerate(kept):
        # matrix_start_ts is the matrix's own global minimum ts, so the
        # training_slice lower bound never excludes anything here - this
        # cutoff is exactly the upper edge training_slice already embargoed
        # to, so preparing straight from the matrix reproduces the same
        # purged, sorted, winsorized set the old rows-list slice-then-prepare
        # pipeline produced.
        cutoff = train_end_ts - args.horizon * 60
        x, y = train_scalper.prepare(matrix, features, args.horizon, cutoff)
        booster = train_scalper.fit(x, y, features)
        document = _build_fold_document(
            booster, features, manifest, args.horizon, train_end_ts, len(x)
        )
        model_name = f"fold-{idx}.json"
        (args.out_dir / model_name).write_bytes(
            (json.dumps(document, sort_keys=True, indent=2) + "\n").encode()
        )
        fold_specs.append(
            {
                "i": idx,
                "train_start_ts": matrix_start_ts,
                "train_end_ts": train_end_ts,
                "test_start_ts": test_start_ts,
                "test_end_ts": test_end_ts,
                "model": model_name,
            }
        )
        print(f"wrote {model_name}: {len(x):,} rows through {document['trained_through']}")

    folds_document = {
        "matrix": str(args.matrix),
        "horizon_min": args.horizon,
        "feature_set_version": manifest["feature_set_version"],
        "folds": fold_specs,
    }
    (args.out_dir / "folds.json").write_text(
        json.dumps(folds_document, sort_keys=True, indent=2) + "\n"
    )
    print(f"wrote folds.json: {len(fold_specs)} fold(s)")


if __name__ == "__main__":
    main()
