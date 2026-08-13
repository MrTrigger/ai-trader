"""Fit LightGBM on a scalper matrix finalized by the Rust feature engine.

This file deliberately contains no feature calculation, resampling, or
simulation. Its input is `features-scalper` training-matrix JSONL: ordered,
already-computed model inputs and forward-return labels in basis points.
Python's only job is fitting trees and dumping them to the auditable house
JSON format.
"""

from __future__ import annotations

import argparse
import calendar
import json
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import lightgbm as lgb
import numpy as np

FORMAT_VERSION = "crypto-lightgbm-json-1"
MODEL_VERSION = "ml-scalper-rust-features-1"
PARAMS = {
    "objective": "regression",
    "num_leaves": 63,
    "learning_rate": 0.05,
    "min_data_in_leaf": 500,
    "feature_fraction": 0.8,
    "bagging_fraction": 0.8,
    "bagging_freq": 1,
    "seed": 7,
    "verbose": -1,
}
N_ESTIMATORS = 400
EARLY_STOPPING_ROUNDS = 50
MIN_ROWS = 50_000


def load_matrix(path: Path):
    """Stream the matrix manifest and rows. No computation, no filtering."""
    path = Path(path)
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        if manifest.get("kind") != "manifest":
            raise ValueError("first matrix row is not the Rust feature manifest")
        rows = [json.loads(line) for line in source if line.strip()]
    return manifest, rows


def prepare(rows, feature_names, horizon, through_ts):
    """Assemble ordered X/y through a cutoff timestamp (inclusive).

    Label is fwd_bps[horizon] winsorized at the 0.5/99.5 percentiles of the
    kept (training) rows themselves - no lookahead into excluded rows.
    """
    if len(rows) < MIN_ROWS:
        raise ValueError(
            f"only {len(rows)} rows in matrix; scalping matrices are big, "
            f"refusing a fit under {MIN_ROWS:,} rows"
        )
    kept = [row for row in rows if row["ts"] <= through_ts]
    # The matrix producer writes rows one asset block at a time (ts-ascending
    # only within each block), so a global chronological order is Python's
    # obligation here - fit()'s early-stopping tail-split depends on it.
    kept.sort(key=lambda row: row["ts"])
    x = np.asarray([[row["features"][name] for name in feature_names] for row in kept])
    y = np.asarray([row["fwd_bps"][str(horizon)] for row in kept], dtype=float)
    lo, hi = np.percentile(y, [0.5, 99.5])
    y = np.clip(y, lo, hi)
    return x, y


def fit(x, y, feature_names):
    """Fit the pinned LightGBM regressor, early-stopping on the chronologically
    last 10% of the given (already time-ordered) rows."""
    n = len(x)
    split = max(1, int(n * 0.9))
    train_set = lgb.Dataset(x[:split], label=y[:split], feature_name=list(feature_names))
    valid_set = lgb.Dataset(
        x[split:], label=y[split:], feature_name=list(feature_names), reference=train_set
    )
    booster = lgb.train(
        PARAMS,
        train_set,
        num_boost_round=N_ESTIMATORS,
        valid_sets=[valid_set],
        callbacks=[lgb.early_stopping(EARLY_STOPPING_ROUNDS, verbose=False)],
    )
    return booster


def _through_ts(through: date) -> int:
    """Epoch seconds at the end (23:59:59 UTC) of the --through date, so
    `ts <= through_ts` includes every row dated on or before it."""
    next_day = through + timedelta(days=1)
    return calendar.timegm(next_day.timetuple()) - 1


def main(argv=None) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--horizon", type=int, required=True)
    parser.add_argument("--through", type=date.fromisoformat, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args(argv)

    manifest, rows = load_matrix(args.matrix)
    features = manifest["features"]
    through_ts = _through_ts(args.through)
    x, y = prepare(rows, features, args.horizon, through_ts)
    booster = fit(x, y, features)

    document = {
        "format_version": FORMAT_VERSION,
        "model_version": MODEL_VERSION,
        "feature_set_version": manifest["feature_set_version"],
        "horizon_min": args.horizon,
        "trained_through": args.through.isoformat(),
        "trained_at": datetime.now(timezone.utc).isoformat(),
        "n_rows": len(x),
        "features": features,
        "label": "fwd_bps_winsorized",
        "model": booster.dump_model(),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_bytes((json.dumps(document, sort_keys=True, indent=2) + "\n").encode())
    print(f"wrote {args.out}: {len(x):,} rows, {len(features)} Rust-owned features")


if __name__ == "__main__":
    main()
