"""The learned cross-sectional ranker: train it offline, load it to decide.

A model is the one part of this system that cannot be recomputed at decision
time. Training needs the whole history and takes minutes; a decision needs a
millisecond and must be reproducible. So the model is an **artefact**: trained by
an explicit command, written to disk with everything needed to audit it, and
loaded read-only by the signal.

## The look-ahead guard is the reason this module exists

A model trained through 2026 and asked to score 2024 has seen the answer. In a
backtest that produces a spectacular result and in production it produces
nothing, because the mistake is invisible from the output. Nothing about the
file's contents reveals it either - the weights look the same.

So the artefact records `trained_through`, and `predict` **refuses** to score a
date at or before it. Not a warning: a refusal. A signal that silently scored
those dates would make every backtest built on it worthless, and the failure
would be discovered only after capital had been committed.

## What is deliberately not here

No training inside the decision path, no automatic retraining, no fallback to an
untrained model. Each of those turns a reproducible decision into one that
depends on when it was run. Retraining is a scheduled operation that produces a
new artefact with a new cutoff, and the plan records which artefact it used.
"""

from __future__ import annotations

import json
import pickle
from dataclasses import dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import Any

import polars as pl

#: Bumped whenever the feature construction or training procedure changes in a
#: way that makes an older artefact incomparable. A plan records this alongside
#: the feature-set version, so a result can always be traced to what produced it.
MODEL_VERSION = "ml-ranker-1"

#: Hyperparameters. Recorded here rather than passed in, because a model whose
#: settings live at the call site cannot be reproduced from the artefact alone.
#: These are the Phase 1 values: deliberately small and heavily regularised, on
#: roughly 2,350 effectively-independent dates.
PARAMS: dict[str, Any] = {
    "objective": "regression",
    "metric": "l2",
    "learning_rate": 0.03,
    "num_leaves": 15,
    "min_data_in_leaf": 200,
    "feature_fraction": 0.7,
    "bagging_fraction": 0.7,
    "bagging_freq": 1,
    "lambda_l2": 20.0,
    "verbose": -1,
}
NUM_ROUNDS = 300


class ModelError(RuntimeError):
    """The model cannot be used as asked. Always fail rather than degrade."""


@dataclass(frozen=True)
class Artefact:
    """A trained model plus everything needed to audit a decision it informed."""

    booster: Any
    features: list[str]
    trained_through: date
    trained_at: datetime
    n_rows: int
    n_dates: int
    model_version: str
    feature_set_version: str

    def predict(self, frame: pl.DataFrame, *, as_of: date) -> dict[str, float]:
        """Score one date's cross-section. Refuses to look backwards in time.

        `frame` is one row per asset, carrying the feature columns. Returns the
        predicted *relative* return - the model is trained on returns demeaned
        within each date, so a score says "beats the cross-section by this
        much", never "goes up".
        """
        if as_of <= self.trained_through:
            raise ModelError(
                f"artefact was trained through {self.trained_through} and cannot "
                f"score {as_of}: the training set contains the answer. Train a "
                "model with an earlier cutoff, or score a later date."
            )
        missing = [f for f in self.features if f not in frame.columns]
        if missing:
            raise ModelError(
                f"frame is missing {len(missing)} feature(s) the artefact needs: "
                f"{missing[:5]}. The feature set has changed since training."
            )
        if frame.is_empty():
            return {}
        scores = self.booster.predict(frame.select(self.features).to_numpy())
        return dict(zip(frame["asset"].to_list(), (float(s) for s in scores)))


def rank_normalise(frame: pl.DataFrame, features: list[str], *, by: str = "date") -> pl.DataFrame:
    """Convert each feature to its rank WITHIN each date, scaled to [-1, 1].

    The model must learn an ordering among assets available on the same day, not
    a level that drifts with the calendar. Trained on raw values it would spend
    its capacity discovering that 2021 was more volatile than 2024 - a fact about
    history, not about which asset to hold.

    Nulls go to the middle rather than being dropped. Dropping loses the whole
    row and with it the asset's other features; the middle is the honest "no
    information" position for one missing input.
    """
    exprs = []
    for f in features:
        rank = pl.col(f).rank("average").over(by)
        n = pl.col(f).is_not_null().sum().over(by)
        exprs.append(
            pl.when(pl.col(f).is_null())
            .then(0.0)
            .otherwise(2.0 * (rank - 1) / pl.max_horizontal(n - 1, pl.lit(1)) - 1.0)
            .alias(f"x_{f}")
        )
    return frame.with_columns(exprs)


def normalise_cross_section(frame: pl.DataFrame, features: list[str]) -> pl.DataFrame:
    """Rank-normalise ONE date's cross-section, at decision time.

    Training normalises within each date across a whole history; a decision has
    only today's names. Same transform, no grouping - and it must be the same
    transform, because a model trained on ranks and fed levels is being asked a
    question in a language it does not speak, and will answer anyway.
    """
    exprs = []
    n = max(frame.height - 1, 1)
    for f in features:
        rank = pl.col(f).rank("average")
        exprs.append(
            pl.when(pl.col(f).is_null())
            .then(0.0)
            .otherwise(2.0 * (rank - 1) / n - 1.0)
            .alias(f"x_{f}")
        )
    return frame.with_columns(exprs)


def demean_target(frame: pl.DataFrame, column: str, *, by: str = "date") -> pl.DataFrame:
    """Subtract each date's cross-sectional mean from the target.

    What remains is the part a long/short book can capture. Predicting the raw
    return means predicting the market, which is a different and much harder
    claim - and one this book does not need, because it holds both sides.
    """
    return frame.with_columns(
        (pl.col(column) - pl.col(column).mean().over(by)).alias("_target")
    )


def train(
    frame: pl.DataFrame,
    *,
    features: list[str],
    target: str,
    trained_through: date,
    feature_set_version: str,
) -> Artefact:
    """Fit on everything at or before `trained_through`.

    The cutoff is an argument rather than inferred from the data, so a caller
    cannot accidentally train on rows they meant to hold out. Rows after it are
    dropped here and the count is reported, which makes an accident visible.
    """
    try:
        import lightgbm as lgb
    except ImportError as exc:  # pragma: no cover - dependency is declared
        raise ModelError(
            "lightgbm is required to train. It is a declared dependency; "
            "reinstall the package."
        ) from exc

    usable = frame.filter(pl.col("date") <= trained_through.isoformat())
    if usable.height < 3_000:
        raise ModelError(
            f"only {usable.height} rows at or before {trained_through}; "
            "refusing to fit a model on a sample this small"
        )
    dropped = frame.height - usable.height
    prepared = demean_target(usable, target)
    xcols = [f"x_{f}" for f in features]
    missing = [c for c in xcols if c not in prepared.columns]
    if missing:
        raise ModelError(f"frame has not been rank-normalised: missing {missing[:5]}")

    booster = lgb.train(
        PARAMS,
        lgb.Dataset(prepared.select(xcols).to_numpy(), label=prepared["_target"].to_numpy()),
        num_boost_round=NUM_ROUNDS,
    )
    return Artefact(
        booster=booster,
        features=xcols,
        trained_through=trained_through,
        trained_at=datetime.now(timezone.utc),
        n_rows=usable.height,
        n_dates=usable["date"].n_unique(),
        model_version=MODEL_VERSION,
        feature_set_version=feature_set_version,
    )


def save(artefact: Artefact, path: Path) -> Path:
    """Write the artefact and a human-readable sidecar.

    The sidecar exists so the provenance of a decision can be read without
    unpickling anything - which matters when the question is being asked after
    something went wrong.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as fh:
        pickle.dump(
            {
                "booster": artefact.booster,
                "features": artefact.features,
                "trained_through": artefact.trained_through.isoformat(),
                "trained_at": artefact.trained_at.isoformat(),
                "n_rows": artefact.n_rows,
                "n_dates": artefact.n_dates,
                "model_version": artefact.model_version,
                "feature_set_version": artefact.feature_set_version,
            },
            fh,
        )
    meta = path.with_suffix(".json")
    meta.write_bytes(
        (
            json.dumps(
                {
                    "model_version": artefact.model_version,
                    "feature_set_version": artefact.feature_set_version,
                    "trained_through": artefact.trained_through.isoformat(),
                    "trained_at": artefact.trained_at.isoformat(),
                    "n_rows": artefact.n_rows,
                    "n_dates": artefact.n_dates,
                    "n_features": len(artefact.features),
                    "params": PARAMS,
                    "num_boost_round": NUM_ROUNDS,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n"
        ).encode("utf-8")
    )
    return path


def load(path: Path) -> Artefact:
    if not path.exists():
        raise ModelError(
            f"no model artefact at {path}. Train one with `ai-trader model train`; "
            "the decision path will not run without it, by design."
        )
    with path.open("rb") as fh:
        d = pickle.load(fh)
    if d.get("model_version") != MODEL_VERSION:
        raise ModelError(
            f"artefact is {d.get('model_version')!r} but this code is "
            f"{MODEL_VERSION!r}. Retrain rather than trusting a model built by "
            "different code."
        )
    return Artefact(
        booster=d["booster"],
        features=d["features"],
        trained_through=date.fromisoformat(d["trained_through"]),
        trained_at=datetime.fromisoformat(d["trained_at"]),
        n_rows=d["n_rows"],
        n_dates=d["n_dates"],
        model_version=d["model_version"],
        feature_set_version=d["feature_set_version"],
    )


# --- building the training frame --------------------------------------------


def build_training_frame(
    *,
    config,
    root: Path,
    start: date,
    end: date,
    lag_hours: int = 1,
    hold_hours: int = 24,
) -> tuple[pl.DataFrame, list[str]]:
    """One row per (decision date, asset), with features and a tradeable target.

    Two details decide whether the model learns anything usable.

    **The target is what a trade would actually have earned.** It runs from the
    price `lag_hours` AFTER the decision to `hold_hours` later, not from the bar
    the features were computed on. Training on a same-instant return teaches the
    model to predict something no order can capture: Phase 1 measured that book
    at Sharpe 2.57 filling instantly and 0.29 filling a day late, and the gap is
    entirely structure the model learned and execution could not reach.

    **Eligibility is applied at the decision date**, from the point-in-time
    universe snapshot, so an asset that was untradeable then cannot appear.

    Returns the frame and the feature names, so the caller never has to
    reconstruct the list that was actually used.
    """
    from datetime import timedelta

    from . import borrow, features as feat, store, universe
    from .bars import mark_discontinuities

    daily = store.read(root=root, interval_s=config.interval_s)
    hourly = store.read(root=root, interval_s=3600)
    if hourly.is_empty():
        raise ModelError(
            "no hourly bars in the store. The feature set needs them; pull with "
            "the archive source before training."
        )
    perp = borrow.listings(root=root)
    dframe = feat.build(daily, benchmark=config.benchmark, perp_listed_from=perp)
    hframe = feat.build_hourly(hourly, benchmark=config.benchmark)

    prices = mark_discontinuities(hourly).select(["asset", "ts_utc", "open"])
    px = {(a, t): o for a, t, o in prices.iter_rows()}

    daily_cols = [
        "ret_7", "ret_30", "ret_90", "ret_30_skip_7", "vol_30", "adv_quote",
        "beta_bench", "gc_regime_slope",
    ]
    hourly_cols = list(feat.HOURLY_FEATURES)
    names = daily_cols + hourly_cols

    # Hourly rows at 00:00 carry everything that closed before the decision.
    dec = hframe.filter(pl.col("ts_utc").dt.hour() == 0)
    dsnap = dframe.select(["asset", "ts_utc"] + [c for c in daily_cols if c in dframe.columns])

    rows = []
    day = datetime(start.year, start.month, start.day, tzinfo=timezone.utc)
    last = datetime(end.year, end.month, end.day, tzinfo=timezone.utc)
    hourly_by_ts = {ts: g for ts, g in
                    ((k[0] if isinstance(k, tuple) else k, v)
                     for k, v in dec.partition_by("ts_utc", as_dict=True).items())}
    daily_by_ts = {ts: g for ts, g in
                   ((k[0] if isinstance(k, tuple) else k, v)
                    for k, v in dsnap.partition_by("ts_utc", as_dict=True).items())}

    while day <= last:
        g = hourly_by_ts.get(day.replace(tzinfo=None)) or hourly_by_ts.get(day)
        if g is None:
            day += timedelta(days=1)
            continue
        try:
            members = universe.load(day, root=root)
        except FileNotFoundError:
            day += timedelta(days=1)
            continue
        elig = {m.asset for m in members if m.eligible}
        # The daily features are stamped at the bar that closed at this instant.
        prev = day - timedelta(seconds=config.interval_s)
        dg = daily_by_ts.get(prev.replace(tzinfo=None)) or daily_by_ts.get(prev)
        dmap = ({r["asset"]: r for r in dg.iter_rows(named=True)} if dg is not None else {})

        entry = day + timedelta(hours=lag_hours)
        exit_ = entry + timedelta(hours=hold_hours)
        block = []
        for r in g.iter_rows(named=True):
            a = r["asset"]
            if a not in elig:
                continue
            first = perp.get(a)
            if first is None or first > day.date():
                continue
            p0, p1 = px.get((a, entry)), px.get((a, exit_))
            if not p0 or not p1:
                continue
            row = {"date": day.date().isoformat(), "asset": a, "y": p1 / p0 - 1}
            d = dmap.get(a, {})
            for c in daily_cols:
                row[c] = d.get(c)
            for c in hourly_cols:
                v = r.get(c)
                row[c] = v if v is None or (isinstance(v, float) and v == v and abs(v) != float("inf")) else None
            block.append(row)
        if len(block) >= 12:
            rows.extend(block)
        day += timedelta(days=1)

    if not rows:
        raise ModelError(f"no usable rows between {start} and {end}")
    frame = pl.DataFrame(rows)
    return rank_normalise(frame, names), names
