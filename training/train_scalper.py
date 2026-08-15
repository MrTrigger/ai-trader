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
import operator
from dataclasses import dataclass
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

# x stays float64 (NOT float32): verified empirically that LightGBM produces
# non-bit-identical split thresholds when fit on a float32-cast copy of the
# same data vs the original float64 (see training/README or git history for
# the check) - feature_infos min/max and tree thresholds diverge at the 7th
# significant digit. That would silently change every model's numbers, so
# we keep the extra memory and stay float64.
X_DTYPE = np.float64


@dataclass(slots=True)
class Matrix:
    """Columnar in-memory form of a `features-scalper` training matrix.

    Built by one streaming pass over the JSONL file - no per-row dict is
    retained once its fields have been copied into the arrays below, so
    peak memory is a small multiple of the arrays themselves rather than a
    materialized list of nested Python dicts.
    """

    feature_names: list[str]
    ts: np.ndarray  # int64, shape (n,)
    asset_codes: np.ndarray  # int32, shape (n,) - index into asset_vocab
    asset_vocab: list[str]
    x: np.ndarray  # float64, shape (n, len(feature_names)), feature_names order
    fwd: dict[str, np.ndarray]  # horizon (str) -> float64 array, shape (n,)

    def __len__(self) -> int:
        return len(self.ts)


def load_matrix(path: Path):
    """Stream the matrix manifest and rows into a columnar `Matrix`.

    No computation, no filtering. Two passes over the file: a cheap first
    pass just counts data rows (and peeks at the first row to learn which
    forward-return horizons are present) so the arrays can be preallocated
    exactly once; the second pass fills them line by line. No list of
    per-row dicts is ever held in full.
    """
    path = Path(path)
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        if manifest.get("kind") != "manifest":
            raise ValueError("first matrix row is not the Rust feature manifest")
        feature_names = list(manifest["features"])
        n = 0
        first_line = None
        for line in source:
            if not line.strip():
                continue
            n += 1
            if first_line is None:
                first_line = line

    n_features = len(feature_names)

    if "horizons_min" in manifest:
        horizons = [str(h) for h in manifest["horizons_min"]]
    elif first_line is not None:
        horizons = list(json.loads(first_line).get("fwd_bps", {}).keys())
    else:
        horizons = []

    ts = np.empty(n, dtype=np.int64)
    x = np.empty((n, n_features), dtype=X_DTYPE)
    fwd = {h: np.empty(n, dtype=np.float64) for h in horizons}
    asset_codes = np.empty(n, dtype=np.int32)
    asset_vocab: list[str] = []
    vocab_index: dict[str, int] = {}

    single_feature = feature_names[0] if n_features == 1 else None
    feature_getter = operator.itemgetter(*feature_names) if n_features > 1 else None

    if n:
        with path.open(encoding="utf-8") as source:
            next(source)  # manifest line, already parsed above
            i = 0
            for line in source:
                if not line.strip():
                    continue
                row = json.loads(line)
                ts[i] = row["ts"]

                asset = row.get("asset")
                code = vocab_index.get(asset)
                if code is None:
                    code = len(asset_vocab)
                    vocab_index[asset] = code
                    asset_vocab.append(asset)
                asset_codes[i] = code

                if n_features == 1:
                    x[i, 0] = row["features"][single_feature]
                elif n_features > 1:
                    x[i] = feature_getter(row["features"])

                fwd_row = row["fwd_bps"]
                for h in horizons:
                    fwd[h][i] = fwd_row[h]

                i += 1
        assert i == n, "matrix row count changed between the counting and parsing pass"

    matrix = Matrix(
        feature_names=feature_names,
        ts=ts,
        asset_codes=asset_codes,
        asset_vocab=asset_vocab,
        x=x,
        fwd=fwd,
    )
    return manifest, matrix


def prepare(matrix: Matrix, feature_names, horizon, through_ts):
    """Assemble ordered X/y through a cutoff timestamp (inclusive).

    Label is fwd_bps[horizon] winsorized at the 0.5/99.5 percentiles of the
    kept (training) rows themselves - no lookahead into excluded rows.
    """
    feature_names = list(feature_names)
    if feature_names != matrix.feature_names:
        raise ValueError(
            "feature_names must match the matrix's stored feature order "
            f"(matrix has {matrix.feature_names}, got {feature_names})"
        )
    n_total = len(matrix)
    if n_total < MIN_ROWS:
        raise ValueError(
            f"only {n_total} rows in matrix; scalping matrices are big, "
            f"refusing a fit under {MIN_ROWS:,} rows"
        )
    # The matrix producer writes rows one asset block at a time (ts-ascending
    # only within each block), so a global chronological order is Python's
    # obligation here - fit()'s early-stopping tail-split depends on it. A
    # stable sort of the kept rows' positions preserves the same tie order
    # that Python's stable list.sort() gave the old dict-of-rows code.
    kept_idx = np.flatnonzero(matrix.ts <= through_ts)
    order = np.argsort(matrix.ts[kept_idx], kind="stable")
    idx = kept_idx[order]

    x = matrix.x[idx]
    y = matrix.fwd[str(horizon)][idx]
    lo, hi = np.percentile(y, [0.5, 99.5])
    y = np.clip(y, lo, hi)
    return x, y


def fit(x, y, feature_names, num_boost_round=N_ESTIMATORS, params_overrides=None):
    """Fit the pinned LightGBM regressor, early-stopping on the chronologically
    last 10% of the given (already time-ordered) rows.

    `num_boost_round` defaults to the pinned production tree count; callers
    (e.g. the parity fixture generator) may cap it lower for a small,
    fast-to-produce model. `params_overrides`, if given, is layered on top of
    the pinned `PARAMS` (production call sites never pass it, so the frozen
    hyperparameters are untouched); it exists so a tiny fixture matrix - far
    below `min_data_in_leaf` - can still produce a model with real splits
    instead of a single trivial leaf.
    """
    params = PARAMS if not params_overrides else {**PARAMS, **params_overrides}
    n = len(x)
    split = max(1, int(n * 0.9))
    train_set = lgb.Dataset(x[:split], label=y[:split], feature_name=list(feature_names))
    valid_set = lgb.Dataset(
        x[split:], label=y[split:], feature_name=list(feature_names), reference=train_set
    )
    booster = lgb.train(
        params,
        train_set,
        num_boost_round=num_boost_round,
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

    manifest, matrix = load_matrix(args.matrix)
    features = manifest["features"]
    through_ts = _through_ts(args.through)
    x, y = prepare(matrix, features, args.horizon, through_ts)
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
