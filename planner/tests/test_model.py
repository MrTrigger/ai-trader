"""The learned ranker: its look-ahead guard, and the sizing it feeds.

Two properties carry the weight here, and both fail silently if unguarded.

**A model must not score a date it was trained on.** The output looks identical
either way - same shape, same plausible numbers - so nothing downstream can
detect it. A backtest built on such a model is spectacular and worthless, and the
error is usually found after capital has been committed.

**Sizing must not concentrate risk while looking diversified.** Phase 1 measured
a construction that held the same number of names at almost the same effective N
as its alternative and drew down 48.7% against 17.3%, because predictions scale
with volatility and weighting on raw conviction quietly buys the most volatile
names. Name count cannot detect that; only dividing by volatility fixes it.
"""

from __future__ import annotations

from datetime import date, datetime, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl
import pytest

from planner import construct, model
from planner.config import Config, CostModel, RiskLimits


def config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=86_400,
        universe=[],
        benchmark="BTC",
        target_gross_exposure=Decimal("0.80"),
        constructor="risk_adjusted",
        min_dollar_volume=Decimal(1_000_000),
        min_history_bars=60,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("1.0"),
        max_holdings=10,
        min_cross_section=5,
        limits=RiskLimits(
            max_gross_exposure=Decimal("1.0"),
            max_position=Decimal("0.25"),
            max_position_count=40,
            min_position_notional=Decimal(25),
        ),
        costs=CostModel(
            commission_bps=Decimal("4.5"),
            spread_bps=Decimal("0.5"),
            impact_coefficient=Decimal("0.10"),
            adv_lookback_days=20,
        ),
    )
    return Config(**(base | over))


FEATURES = ["f_a", "f_b", "f_c"]


def training_frame(n_dates: int = 300, n_assets: int = 20) -> pl.DataFrame:
    """A frame with a real relationship, so a trained model is not pure noise."""
    import random

    rng = random.Random(0)
    rows = []
    for d in range(n_dates):
        day = date(2020, 1, 1).toordinal() + d
        for a in range(n_assets):
            fa = rng.gauss(0, 1)
            rows.append({
                "date": date.fromordinal(day).isoformat(),
                "asset": f"A{a}",
                "f_a": fa,
                "f_b": rng.gauss(0, 1),
                "f_c": rng.gauss(0, 1),
                # The target genuinely depends on f_a, plus noise.
                "y": 0.01 * fa + rng.gauss(0, 0.02),
            })
    frame = pl.DataFrame(rows)
    return model.rank_normalise(frame, FEATURES)


@pytest.fixture(scope="module")
def artefact() -> model.Artefact:
    frame = training_frame()
    return model.train(
        frame,
        features=FEATURES,
        target="y",
        trained_through=date(2020, 8, 1),
        feature_set_version="test-fs",
    )


# --- the look-ahead guard ---------------------------------------------------


def test_it_refuses_to_score_a_date_it_was_trained_through(artefact):
    frame = training_frame(n_dates=1)
    with pytest.raises(model.ModelError, match="contains the answer"):
        artefact.predict(frame, as_of=artefact.trained_through)


def test_it_refuses_to_score_any_earlier_date(artefact):
    frame = training_frame(n_dates=1)
    with pytest.raises(model.ModelError, match="contains the answer"):
        artefact.predict(frame, as_of=date(2020, 3, 1))


def test_it_scores_a_date_after_the_cutoff(artefact):
    frame = training_frame(n_dates=1)
    scores = artefact.predict(frame, as_of=date(2020, 9, 1))
    assert len(scores) == frame.height
    assert all(isinstance(v, float) for v in scores.values())


def test_training_drops_rows_after_the_cutoff(artefact):
    # 300 dates were generated; the cutoff is well inside them.
    assert artefact.n_dates < 300
    assert artefact.trained_through == date(2020, 8, 1)


def test_it_refuses_a_frame_missing_features(artefact):
    frame = training_frame(n_dates=1).drop("x_f_a")
    with pytest.raises(model.ModelError, match="missing"):
        artefact.predict(frame, as_of=date(2020, 9, 1))


def test_it_refuses_to_fit_on_too_little_data():
    with pytest.raises(model.ModelError, match="refusing to fit"):
        model.train(
            training_frame(n_dates=5),
            features=FEATURES,
            target="y",
            trained_through=date(2020, 8, 1),
            feature_set_version="test-fs",
        )


# --- the artefact round-trips with its provenance ---------------------------


def test_save_and_load_preserves_the_cutoff(artefact, tmp_path: Path):
    p = model.save(artefact, tmp_path / "m.pkl")
    back = model.load(p)
    assert back.trained_through == artefact.trained_through
    assert back.features == artefact.features
    assert back.model_version == artefact.model_version


def test_the_sidecar_is_readable_without_unpickling(artefact, tmp_path: Path):
    """Provenance has to be legible when something has gone wrong."""
    import json

    model.save(artefact, tmp_path / "m.pkl")
    meta = json.loads((tmp_path / "m.json").read_text())
    assert meta["trained_through"] == "2020-08-01"
    assert meta["n_features"] == len(FEATURES)
    assert meta["num_boost_round"] == model.NUM_ROUNDS


def test_a_missing_artefact_is_an_error_not_a_default(tmp_path: Path):
    with pytest.raises(model.ModelError, match="will not run without it"):
        model.load(tmp_path / "absent.pkl")


def test_a_version_mismatch_refuses_rather_than_guesses(artefact, tmp_path: Path):
    import pickle

    p = tmp_path / "m.pkl"
    model.save(artefact, p)
    with p.open("rb") as fh:
        d = pickle.load(fh)
    d["model_version"] = "something-older"
    with p.open("wb") as fh:
        pickle.dump(d, fh)
    with pytest.raises(model.ModelError, match="Retrain"):
        model.load(p)


# --- rank normalisation -----------------------------------------------------


def test_features_are_ranked_within_their_own_date():
    frame = pl.DataFrame({
        "date": ["d1"] * 3 + ["d2"] * 3,
        "asset": list("abcabc"),
        # d2's values are ten times d1's; the RANKS are identical.
        "f": [1.0, 2.0, 3.0, 10.0, 20.0, 30.0],
    })
    out = model.rank_normalise(frame, ["f"])
    d1 = out.filter(pl.col("date") == "d1")["x_f"].to_list()
    d2 = out.filter(pl.col("date") == "d2")["x_f"].to_list()
    assert d1 == d2 == [-1.0, 0.0, 1.0]


def test_a_null_feature_lands_in_the_middle_rather_than_being_dropped():
    frame = pl.DataFrame({
        "date": ["d1"] * 3,
        "asset": list("abc"),
        "f": [1.0, None, 3.0],
    })
    out = model.rank_normalise(frame, ["f"])
    assert out.height == 3
    assert out["x_f"].to_list()[1] == 0.0


def test_the_target_is_demeaned_within_each_date():
    frame = pl.DataFrame({
        "date": ["d1"] * 3 + ["d2"] * 3,
        "y": [1.0, 2.0, 3.0, 11.0, 12.0, 13.0],
    })
    out = model.demean_target(frame, "y")
    assert out["_target"].to_list() == [-1.0, 0.0, 1.0, -1.0, 0.0, 1.0]


# --- the constructor it feeds -----------------------------------------------


def sig(asset, direction, conviction, vol):
    return construct.Signal(
        asset=asset, direction=direction,
        conviction=Decimal(str(conviction)), volatility=Decimal(str(vol)),
    )


def test_it_sizes_by_edge_over_volatility_not_by_edge():
    """The finding that separated a -17% drawdown from a -49% one.

    Two longs with the same expected edge and different volatility must not get
    the same weight - the volatile one is a larger risk position for identical
    expected return.
    """
    cfg = config()
    sigs = [
        sig("CALM", "long", 0.05, 0.20),
        sig("WILD", "long", 0.05, 0.80),
        sig("S1", "short", 0.05, 0.40),
        sig("S2", "short", 0.05, 0.40),
    ]
    w = construct.get("risk_adjusted").construct(sigs, config=cfg).weights
    assert w["CALM"] > w["WILD"]
    # Four times the volatility for the same edge earns a quarter of the weight.
    assert w["CALM"] / w["WILD"] == pytest.approx(Decimal(4), rel=Decimal("0.01"))


def test_names_below_the_cost_threshold_are_not_traded():
    cfg = config()
    round_trip = 2 * (cfg.costs.commission_bps + cfg.costs.spread_bps) / Decimal(10_000)
    sigs = [
        sig("GOOD1", "long", float(round_trip) * 5, 0.4),
        sig("GOOD2", "long", float(round_trip) * 4, 0.4),
        sig("TINY", "long", float(round_trip) / 10, 0.4),
        sig("S1", "short", float(round_trip) * 5, 0.4),
        sig("S2", "short", float(round_trip) * 4, 0.4),
    ]
    out = construct.get("risk_adjusted").construct(sigs, config=cfg)
    assert "TINY" not in out.weights
    assert any("round trip" in n for n in out.notes)


def test_a_one_sided_book_is_refused():
    """A book that cannot form two sides is a directional bet, not this strategy."""
    cfg = config()
    sigs = [sig(f"L{i}", "long", 0.05, 0.4) for i in range(5)]
    out = construct.get("risk_adjusted").construct(sigs, config=cfg)
    assert out.weights == {}
    assert any("directional bet" in n for n in out.notes)


def test_it_is_dollar_neutral():
    cfg = config()
    sigs = ([sig(f"L{i}", "long", 0.05 + i / 100, 0.3 + i / 10) for i in range(4)]
            + [sig(f"S{i}", "short", 0.05 + i / 100, 0.3 + i / 10) for i in range(4)])
    w = construct.get("risk_adjusted").construct(sigs, config=cfg).weights
    assert sum(w.values()) == pytest.approx(Decimal(0), abs=Decimal("1e-9"))


def test_gross_respects_the_target():
    cfg = config()
    sigs = ([sig(f"L{i}", "long", 0.05, 0.4) for i in range(4)]
            + [sig(f"S{i}", "short", 0.05, 0.4) for i in range(4)])
    w = construct.get("risk_adjusted").construct(sigs, config=cfg).weights
    gross = sum(abs(v) for v in w.values())
    assert gross <= cfg.target_gross_exposure + Decimal("1e-9")


def test_a_signal_without_volatility_is_dropped_and_disclosed():
    cfg = config()
    sigs = [
        construct.Signal(asset="NOVOL", direction="long", conviction=Decimal("0.05")),
        sig("L1", "long", 0.05, 0.4), sig("L2", "long", 0.05, 0.4),
        sig("S1", "short", 0.05, 0.4), sig("S2", "short", 0.05, 0.4),
    ]
    out = construct.get("risk_adjusted").construct(sigs, config=cfg)
    assert "NOVOL" not in out.weights
    assert any("assumed one" in n for n in out.notes)


def test_the_per_position_cap_scales_the_book_rather_than_redistributing():
    """Redistributing a capped position's leftover converts a diversification
    limit into a concentration one."""
    cfg = config()
    sigs = [
        sig("HUGE", "long", 1.0, 0.01),      # enormous edge, tiny vol
        sig("L2", "long", 0.05, 0.4),
        sig("S1", "short", 0.05, 0.4),
        sig("S2", "short", 0.05, 0.4),
    ]
    out = construct.get("risk_adjusted").construct(sigs, config=cfg)
    assert max(abs(v) for v in out.weights.values()) <= cfg.limits.max_position + Decimal("1e-9")
    assert any("scaled" in n for n in out.notes)
