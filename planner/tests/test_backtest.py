"""The replay backtest (design spec §2.3).

A backtest is a machine for fooling yourself, so most of what follows checks
the ways it could lie rather than the numbers it produces:

- that it cannot see the future (the fill happens *after* the decision)
- that costs are charged, in the direction that hurts
- that days the live system would have refused to trade are absent, and said so
- that the sample size travels with every result

The one number-shaped assertion — an asset that only rises makes money — exists
to catch the replay being wired up backwards, not to validate a strategy.
"""

from __future__ import annotations

import math
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl
import pytest

from planner import backtest, state, store, universe
from planner.config import Config, CostModel, RiskLimits

DAY = 86_400
START = datetime(2026, 4, 1, tzinfo=timezone.utc)
END = datetime(2026, 8, 1, tzinfo=timezone.utc)
ASSETS = ("AAA", "BBB", "CCC")
HISTORY = 260

#: A window long enough for a return series to mean something mechanically,
#: short enough that a test suite stays a test suite. Every step re-reads the
#: store, because that is what the live planner does.
MID = START + timedelta(days=30)


def _bars(assets=ASSETS, drift: float = 0.0006) -> pl.DataFrame:
    """Contiguous daily bars ending at END, each opening where the last closed.

    The oscillation amplitude is chosen so realised volatility lands in the
    range a real crypto asset occupies. Calmer bars would be screened out as
    pegs by `min_volatility` — which is the screen working, but it would make
    this fixture test the screen rather than the replay.
    """
    rows = []
    for i, asset in enumerate(assets):
        close = 100.0 * (i + 1)
        for d in range(HISTORY):
            ts = END - timedelta(seconds=DAY * (HISTORY - 1 - d))
            nxt = close * math.exp(0.02 * math.sin(i + d * 0.55) + drift)
            rows.append(
                {
                    "asset": asset,
                    "ts_utc": ts,
                    "interval_s": DAY,
                    "open": close,
                    "high": max(close, nxt),
                    "low": min(close, nxt),
                    "close": nxt,
                    "volume": 100_000.0,
                    "quote_volume": 5e8,
                    "trades": 10_000,
                }
            )
            close = nxt
    return pl.DataFrame(rows).with_columns(
        pl.col("ts_utc").dt.replace_time_zone("UTC"), pl.col("interval_s").cast(pl.Int32)
    )


def _config(**over) -> Config:
    base = dict(
        quote_currency="USD",
        interval_s=DAY,
        universe=list(ASSETS),
        target_gross_exposure=Decimal("0.75"),
        constructor="equal_weight",
        min_dollar_volume=Decimal(1_000_000),
        min_history_bars=60,
        rebalance_cost_multiple=Decimal("3.0"),
        turnover_budget=Decimal("1.0"),
        limits=RiskLimits(
            max_gross_exposure=Decimal("1.0"),
            max_position=Decimal("0.30"),
            max_position_count=20,
            min_position_notional=Decimal(25),
        ),
        costs=CostModel(
            commission_bps=Decimal(5),
            spread_bps=Decimal(2),
            impact_coefficient=Decimal("0.10"),
            adv_lookback_days=20,
        ),
    )
    return Config(**(base | over))


@pytest.fixture
def env(tmp_path: Path) -> Path:
    store.write(_bars(), root=tmp_path, source="synthetic")
    # A universe snapshot per rebalance date - never backfilled, recorded as of
    # each day, exactly as the live system would have had them.
    day = START
    while day <= END:
        universe.record(
            universe.from_config(list(ASSETS)), as_of=day, source="test", root=tmp_path
        )
        day += timedelta(seconds=DAY)
    return tmp_path


def run(env: Path, *, start=START, end=END, **over) -> backtest.Result:
    return backtest.replay(
        config=_config(**over),
        start=start,
        end=end,
        data_root=env,
        initial_cash=Decimal(100_000),
    )


# --- the replay ------------------------------------------------------------


def test_the_replay_produces_one_step_per_rebalance(env):
    result = run(env, start=START, end=START + timedelta(days=9))
    assert len(result.steps) == 10
    assert result.metrics.n == 10
    assert [s.as_of for s in result.steps] == [
        START + timedelta(days=d) for d in range(10)
    ]


def test_the_book_carries_forward_between_steps(env):
    result = run(env, start=START, end=START + timedelta(days=4))
    # The first step buys in; later steps hold rather than re-buying the lot.
    assert result.steps[0].fills
    assert sum(len(s.fills) for s in result.steps[1:]) < len(result.steps[0].fills) * 4


def test_nav_starts_at_the_initial_cash(env):
    result = run(env, start=START, end=START + timedelta(days=2))
    # First step: cash spent on positions, so NAV is initial cash less costs.
    assert result.steps[0].nav < Decimal(100_000)
    assert result.steps[0].nav > Decimal(99_000), "costs are basis points, not percent"


def test_a_rising_book_makes_money(env):
    # Wiring check, not a strategy result: if the replay were marking positions
    # backwards or dropping fills, this is where it would show.
    result = run(env, start=START, end=MID)
    assert result.metrics.total_return > 0
    assert result.metrics.cagr > 0


def test_a_falling_book_loses_money(tmp_path):
    store.write(_bars(drift=-0.0015), root=tmp_path, source="synthetic")
    day = START
    while day <= END:
        universe.record(
            universe.from_config(list(ASSETS)), as_of=day, source="test", root=tmp_path
        )
        day += timedelta(seconds=DAY)

    result = run(tmp_path, start=START, end=MID)
    assert result.metrics.total_return < 0
    assert result.metrics.max_drawdown < 0


# --- causality -------------------------------------------------------------


def test_fills_happen_at_the_open_of_the_bar_the_planner_could_not_see(env):
    """The earliest honest price, and the reason the forming bar is excluded."""
    result = run(env, start=START, end=START)
    bars = store.read(root=env, interval_s=DAY)

    for fill in result.steps[0].fills:
        row = bars.filter((pl.col("asset") == fill.asset) & (pl.col("ts_utc") == START))
        open_price = Decimal(str(row["open"][0]))
        close_price = Decimal(str(row["close"][0]))
        assert fill.price != close_price, "filling at the close is a free look forward"
        # Open plus half-spread crossed in the direction that costs us.
        assert fill.price > open_price if fill.side == "buy" else fill.price < open_price
        assert abs(fill.price - open_price) / open_price < Decimal("0.001")


def test_the_replay_cannot_reach_bars_after_the_decision(env, monkeypatch):
    """`store.read(until=...)` is the mechanism; assert it is actually used."""
    seen: list[datetime | None] = []
    real = store.read

    def spy(**kwargs):
        seen.append(kwargs.get("until"))
        return real(**kwargs)

    monkeypatch.setattr(store, "read", spy)
    run(env, start=START, end=START + timedelta(days=2))

    horizons = [u for u in seen if u is not None]
    assert horizons, "the planner must bound its reads by a horizon"
    assert all(h <= END for h in horizons)


def test_a_later_end_date_does_not_change_earlier_steps(env):
    """Prefix invariance for the replay itself, not just for features."""
    short = run(env, start=START, end=START + timedelta(days=5))
    long = run(env, start=START, end=START + timedelta(days=20))

    for a, b in zip(short.steps, long.steps):
        assert a.as_of == b.as_of
        assert a.nav == b.nav
        assert a.plan_id == b.plan_id


# --- costs -----------------------------------------------------------------


def test_every_fill_is_charged_a_fee(env):
    result = run(env, start=START, end=START)
    assert result.steps[0].fills
    assert all(f.fee > 0 for f in result.steps[0].fills)
    assert result.metrics.cost_drag_bps > 0


def test_slippage_is_crossed_in_the_direction_that_hurts(env):
    result = run(env, start=START, end=MID)
    bars = store.read(root=env, interval_s=DAY)

    for step in result.steps:
        for fill in step.fills:
            row = bars.filter(
                (pl.col("asset") == fill.asset) & (pl.col("ts_utc") == step.as_of)
            )
            open_price = Decimal(str(row["open"][0]))
            if fill.side == "buy":
                assert fill.price > open_price
            else:
                assert fill.price < open_price


def test_doubling_slippage_can_only_cost_more(env):
    plain, doubled = backtest.sensitivity(
        config=_config(),
        start=START,
        end=MID,
        data_root=env,
        initial_cash=Decimal(100_000),
    )
    assert doubled.slippage_multiple == 2
    assert doubled.metrics.total_return < plain.metrics.total_return, (
        "an edge that survives 2x slippage is the Phase 1 gate; one that improves "
        "under it means the sign is wrong somewhere"
    )


def test_the_sensitivity_run_discloses_that_it_is_scaled(env):
    _, doubled = backtest.sensitivity(
        config=_config(),
        start=START,
        end=START + timedelta(days=5),
        data_root=env,
        initial_cash=Decimal(100_000),
    )
    assert any("2x" in d and "error bar" in d for d in doubled.disclosures)


# --- honesty ---------------------------------------------------------------


def test_an_inadequate_sample_says_so_before_any_number(env):
    result = run(env, start=START, end=START + timedelta(days=5))
    assert result.metrics.insufficient_sample
    assert any("insufficient sample" in d for d in result.disclosures)
    assert result.disclosures.index(
        next(d for d in result.disclosures if "insufficient sample" in d)
    ) < len(result.disclosures)


def test_the_uncalibrated_cost_model_is_disclosed_on_every_run(env):
    result = run(env, start=START, end=START + timedelta(days=2))
    assert any("uncalibrated" in d for d in result.disclosures)


def test_the_fill_models_own_crudeness_is_disclosed(env):
    result = run(env, start=START, end=START + timedelta(days=2))
    assert any("no partial fills" in d for d in result.disclosures)


def test_dates_the_live_system_would_have_refused_are_absent_and_counted(tmp_path):
    """A backtest that silently traded through a gate failure measures a system
    that does not exist."""
    store.write(_bars(), root=tmp_path, source="synthetic")
    # Universe recorded for only half the window. The rest fail the gate, as
    # they would live.
    day = START
    while day <= START + timedelta(days=4):
        universe.record(
            universe.from_config(list(ASSETS)), as_of=day, source="test", root=tmp_path
        )
        day += timedelta(seconds=DAY)

    result = run(tmp_path, start=START, end=START + timedelta(days=9))
    assert len(result.steps) == 5
    assert result.metrics.n == 5
    assert any("gate failed" in d for d in result.disclosures)
    assert any("stood still" in d for d in result.disclosures)


def test_a_rejected_plan_trades_nothing_and_is_counted(env):
    result = run(
        env,
        start=START,
        end=START + timedelta(days=4),
        limits=RiskLimits(
            max_gross_exposure=Decimal("0.01"),
            max_position=Decimal("0.30"),
            max_position_count=20,
            min_position_notional=Decimal(25),
        ),
    )
    assert result.metrics.rejected == len(result.steps)
    assert all(not s.fills for s in result.steps)
    assert result.metrics.total_return == 0, "a book that never traded cannot move"
    assert any("rejected by the risk gate" in d for d in result.disclosures)


def test_the_plans_own_disclosures_travel_with_each_step(env):
    result = run(env, start=START, end=START)
    assert any("claims no edge" in w for w in result.steps[0].warnings)


# --- metrics ---------------------------------------------------------------


def test_volatility_is_annualised_on_a_365_day_year(env):
    result = run(env, start=START, end=MID)
    navs = [s.nav for s in result.steps]
    rets = [float((navs[i] - navs[i - 1]) / navs[i - 1]) for i in range(1, len(navs))]
    expected = pl.Series(rets).std() * math.sqrt(365)
    assert result.metrics.volatility == pytest.approx(expected, rel=1e-6)


def test_max_drawdown_is_measured_from_the_peak_and_is_never_positive(env):
    result = run(env, start=START, end=MID)
    assert result.metrics.max_drawdown <= 0


def test_metrics_on_a_single_step_refuse_to_invent_a_return(env):
    result = run(env, start=START, end=START)
    assert result.metrics.n == 1
    assert result.metrics.total_return == 0
    assert result.metrics.sharpe == 0.0


def test_a_backwards_window_is_refused(env):
    with pytest.raises(ValueError, match="is after end"):
        run(env, start=END, end=START)


CONFIG_TOML = """
[meta]
ruleset_version = "test"

[portfolio]
quote_currency = "USD"
interval_s = 86400
target_gross_exposure = "0.75"
constructor = "equal_weight"
signal = "placeholder_equal_long"
universe = ["AAA", "BBB", "CCC"]
min_dollar_volume = "1000000"
min_history_bars = 60
rebalance_cost_multiple = "3.0"
turnover_budget = "1.0"

[limits]
max_gross_exposure = "1.00"
max_position = "0.30"
max_position_count = 20
min_position_notional = "25"
max_net_exposure = ""
max_cluster_exposure = ""
max_benchmark_beta = ""

[costs]
commission_bps = "5.0"
spread_bps = "2.0"
impact_coefficient = "0.10"
adv_lookback_days = 20
calibrated = false
"""


@pytest.fixture
def cfg(tmp_path: Path) -> Path:
    """A config matching the synthetic universe.

    The shipped `default.toml` names BTC as its benchmark and enforces a beta
    limit against it, which these bars have no BTC to satisfy - and the gate
    correctly refuses to run. Pointing the CLI at its own config keeps that
    refusal a real behaviour rather than a test nuisance.
    """
    path = tmp_path / "test.toml"
    path.write_text(CONFIG_TOML, encoding="utf-8")
    return path


# --- the CLI surface -------------------------------------------------------


def test_the_cli_reports_disclosures_before_any_number(env, cfg, capsys):
    from planner import cli

    rc = cli.main(
        [
            "--config", str(cfg),
            "--data-root", str(env),
            "backtest",
            "--start", START.date().isoformat(),
            "--end", (START + timedelta(days=5)).date().isoformat(),
            "--slippage", "1",
        ]
    )
    assert rc == 0
    out = capsys.readouterr().out
    assert out.index("DISCLOSURES") < out.index("Sharpe")
    assert "insufficient sample" in out
    assert "evidence of edge" in out


def test_the_cli_runs_the_slippage_error_bar(env, cfg, capsys):
    from planner import cli

    cli.main(
        [
            "--config", str(cfg),
            "--data-root", str(env),
            "backtest",
            "--start", START.date().isoformat(),
            "--end", (START + timedelta(days=5)).date().isoformat(),
        ]
    )
    out = capsys.readouterr().out
    assert "1x" in out and "2x" in out
    assert "error bar, not a parameter" in out


def test_the_cli_writes_a_nav_series(env, cfg, tmp_path, capsys):
    from planner import cli

    nav = tmp_path / "nav.csv"
    cli.main(
        [
            "--config", str(cfg),
            "--data-root", str(env),
            "backtest",
            "--start", START.date().isoformat(),
            "--end", (START + timedelta(days=3)).date().isoformat(),
            "--slippage", "1",
            "--nav", str(nav),
        ]
    )
    lines = nav.read_text().strip().splitlines()
    assert lines[0] == "ts_utc,nav"
    assert len(lines) == 5


def test_the_cli_fails_closed_when_no_date_produced_a_plan(tmp_path, cfg, capsys):
    from planner import cli

    store.write(_bars(), root=tmp_path, source="synthetic")  # no universe recorded
    rc = cli.main(
        [
            "--config", str(cfg),
            "--data-root", str(tmp_path),
            "backtest",
            "--start", START.date().isoformat(),
            "--end", (START + timedelta(days=3)).date().isoformat(),
            "--slippage", "1",
        ]
    )
    assert rc == 2
    assert "nothing to report" in capsys.readouterr().err


def test_annualisation_uses_the_calendar_not_the_step_count(env):
    """Steps are not contiguous when gates fail, and CAGR must not assume they are.

    Ten steps spread over five years is not ten consecutive weeks. Treating it
    as such reported a 38% total return as a CAGR of 58,996,978% — obviously
    wrong at that magnitude, and silently wrong at a smaller one.
    """
    steps = [
        backtest.Step(
            as_of=START + timedelta(days=i * 365),
            nav=Decimal(100_000) * (Decimal("1.10") ** i),
            cash=Decimal(0),
            gross_exposure=Decimal("0.75"),
            status="accepted",
            fills=[],
            plan_id=f"p{i}",
            warnings=[],
        )
        for i in range(5)
    ]
    m = backtest.metrics(steps, interval_s=DAY)
    assert m.n == 5
    # Four years apart, compounding at 10% a year.
    assert m.cagr == pytest.approx(0.10, abs=0.005)


def test_a_sparse_replay_does_not_inflate_volatility(env):
    widely_spaced = [
        backtest.Step(
            as_of=START + timedelta(days=i * 180),
            nav=Decimal(100_000 + i * 1_000),
            cash=Decimal(0),
            gross_exposure=Decimal("0.75"),
            status="accepted",
            fills=[],
            plan_id=f"p{i}",
            warnings=[],
        )
        for i in range(6)
    ]
    m = backtest.metrics(widely_spaced, interval_s=DAY)
    # Two observations a year cannot produce a triple-digit annualised vol.
    assert m.volatility < 0.5
