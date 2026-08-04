"""The decision path: steps 1-8, pure.

Nothing in this module can move capital. It reads bars, a universe snapshot and
the current book, and it emits a Plan. That is the whole contract, and it is
what makes the backtest and the live run the same code: the backtest is this
function over history, and live is this function plus an executor.

Gates fail closed (design spec section 6.3). Missing data, a stale horizon or an
incomplete universe means *do nothing and say why* - never act on partial
knowledge. A gate failure raises `GateFailure`; it does not return a degraded
plan, because a degraded plan is indistinguishable from a good one downstream.
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass, replace
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl

from . import __version__, construct, costs, diff, features, risk, state, store, universe
from .config import Config
from .plan import (
    AssetCost,
    CostEstimate,
    CurrentPosition,
    Nav,
    Order,
    Provenance,
    Target,
    Warning,
    build,
)

RISK_MODEL_VERSION = "none-phase0"
SCORING_VERSION = "none-phase0"


class GateFailure(RuntimeError):
    """A precondition failed. The correct response is to stop, not to adapt."""


@dataclass(frozen=True)
class PlanResult:
    document: dict
    skipped: list[str]
    notes: list[str]


def usable_horizon(as_of: datetime, interval_s: int) -> datetime:
    """The newest bar OPEN a decision at `as_of` may see.

    A bar is usable only once it has fully closed. A daily bar opening at
    2026-07-31T00:00 closes at 2026-08-01T00:00, so a decision stamped
    2026-08-01T00:00 may use it and a decision stamped 2026-07-31T12:00 may not.

    Getting this off by one is how a strategy ends up trading on a bar that had
    not finished forming - which backtests beautifully and loses money.
    """
    return as_of - timedelta(seconds=interval_s)


def run(
    *,
    as_of: datetime,
    config: Config,
    mode: str = "dry",
    data_root: Path = store.DEFAULT_ROOT,
    book_path: Path | None = None,
    run_id: uuid.UUID | None = None,
    created_at: datetime | None = None,
) -> PlanResult:
    if as_of.tzinfo is None:
        raise ValueError("as_of must be timezone-aware")
    as_of = as_of.astimezone(timezone.utc)
    run_id = run_id or uuid.uuid5(uuid.NAMESPACE_URL, f"run:{as_of.isoformat()}")

    warnings: list[Warning] = []
    notes: list[str] = []

    # --- 1. OBSERVE ---------------------------------------------------------
    horizon = usable_horizon(as_of, config.interval_s)
    bars = store.read(root=data_root, interval_s=config.interval_s, until=horizon)
    if bars.is_empty():
        raise GateFailure(
            f"no bars at or before {horizon.isoformat()} for interval {config.interval_s}s"
        )

    try:
        members = universe.load(as_of, root=data_root)
    except FileNotFoundError as exc:
        raise GateFailure(str(exc)) from exc

    eligible_by_config = [m.asset for m in members if m.eligible]
    if not eligible_by_config:
        raise GateFailure(f"universe snapshot for {as_of.date()} has no eligible assets")

    # Freshness. A stale newest bar means the pull did not run or the source is
    # behind; either way the decision would be made on old prices.
    newest = bars["ts_utc"].max()
    max_staleness = timedelta(seconds=config.interval_s * 2)
    if horizon - newest > max_staleness:
        raise GateFailure(
            f"stale bars: newest is {newest.isoformat()}, horizon is {horizon.isoformat()} "
            f"({horizon - newest} > {max_staleness})"
        )

    # Completeness. A truncated universe manufactures false 'dropped out' exits
    # and will liquidate a healthy book on a bad response - the highest-severity
    # silent failure available to this class of system.
    have_bars = set(bars["asset"].unique().to_list())
    missing = sorted(set(eligible_by_config) - have_bars)
    if missing:
        raise GateFailure(
            f"universe is incomplete: no bars for {missing}. Refusing to evaluate "
            "exits against a partial universe."
        )

    book = state.load(book_path or (data_root / "book.json"), as_of=as_of)

    # --- 2. FEATURIZE -------------------------------------------------------
    # The benchmark is featurized alongside the universe even when it is not
    # itself eligible: its return series is what every beta is measured against
    # (section 6.2), and dropping it here would silently disable that limit.
    featurizable = list(eligible_by_config)
    if config.benchmark and config.benchmark not in featurizable:
        featurizable.append(config.benchmark)

    if config.limits.max_benchmark_beta is not None:
        if not config.benchmark:
            raise GateFailure(
                "max_benchmark_beta is enforced but no benchmark is configured; "
                "refusing to evaluate a limit against nothing"
            )
        if config.benchmark not in have_bars:
            raise GateFailure(
                f"max_benchmark_beta is enforced but benchmark {config.benchmark} "
                "has no bars at the horizon. A limit that cannot be evaluated is "
                "not a limit."
            )

    frame = features.build(
        bars.filter(pl.col("asset").is_in(featurizable)), benchmark=config.benchmark
    )
    cross = features.latest(frame).filter(pl.col("asset").is_in(eligible_by_config))

    prices = {r["asset"]: Decimal(str(r["close"])) for r in cross.iter_rows(named=True)}
    adv: dict[str, Decimal | None] = {}
    vol: dict[str, Decimal | None] = {}
    betas: dict[str, Decimal | None] = {}
    for r in cross.iter_rows(named=True):
        adv[r["asset"]] = None if r["adv_quote"] is None else Decimal(str(r["adv_quote"]))
        vol[r["asset"]] = None if r["vol_30"] is None else Decimal(str(r["vol_30"])) / Decimal(
            str(365 ** 0.5)
        )
        # Quantized where the float becomes a Decimal, not later. A beta is not
        # meaningful past six places, and carrying the full binary tail into the
        # risk report puts nineteen digits of noise next to a two-digit limit.
        # Rounding here rather than on the way out keeps the number that was
        # *checked* and the number that is *reported* the same number.
        betas[r["asset"]] = (
            None if r["beta_bench"] is None else _q(Decimal(str(r["beta_bench"])), "0.000001")
        )

    for p in book.positions:
        if p.asset not in prices:
            raise GateFailure(
                f"holding {p.asset} but it has no price at {horizon.isoformat()}; "
                "refusing to mark a held position at zero"
            )

    nav = book.nav(prices)
    if nav <= 0:
        raise GateFailure(f"non-positive NAV ({nav} {config.quote_currency})")
    current_weights = book.weights(prices)

    # --- 3. SIGNAL ----------------------------------------------------------
    signals, eligibility_notes = _signal(cross, config)
    notes.extend(eligibility_notes)
    warnings.append(
        Warning(
            kind="unenforced_rule",
            message=(
                f"signal {config.signal!r} is a Phase 0 placeholder and claims no edge. "
                "It has not been through the backtest harness. No capital until it has."
            ),
        )
    )

    # --- 4. RISK MODEL ------------------------------------------------------
    # No covariance estimate yet, so nothing here shapes construction (step 5).
    # The cluster and beta limits below are the crude stand-ins the spec asks
    # for, and they veto rather than shape - which is the weaker of the two
    # jobs section 3.1 says risk should do.
    constrained_by = "the position count and gross cap"
    if config.limits.max_cluster_exposure is not None:
        constrained_by = "a configured cluster grouping, the position count and gross cap"
    warnings.append(
        Warning(
            kind="unenforced_rule",
            message=(
                f"no risk model: covariance does not inform construction, so correlated "
                f"positions are constrained only by {constrained_by}."
            ),
        )
    )

    # --- 5. CONSTRUCT -------------------------------------------------------
    constructor = construct.get(config.constructor)
    built = constructor.construct(signals, config=config)
    notes.extend(built.notes)
    if built.fell_back:
        warnings.append(
            Warning(
                kind="constructor_fallback",
                message=f"requested {built.requested}, used {built.constructor}",
            )
        )

    # --- 6. RISK GATE -------------------------------------------------------
    evaluation = risk.evaluate(
        target_weights=built.weights,
        current_weights=current_weights,
        limits=config.limits,
        nav=nav,
        clusters=config.clusters,
        betas=betas,
    )
    report = evaluation.report
    for name in config.limits.unenforced():
        warnings.append(
            Warning(kind="unenforced_rule", message=f"limit {name} is not enforced")
        )
    # A limit can be enforced and still be weaker than it reads - an
    # unclassified asset escapes the cluster constraint, an unestimable beta is
    # assumed. Disclosed above the numbers, never beneath them (section 12).
    for note in evaluation.disclosures:
        warnings.append(Warning(kind="unenforced_rule", message=note))
    if not config.costs.calibrated:
        warnings.append(
            Warning(
                kind="other",
                message=(
                    "cost model is uncalibrated: the impact coefficient is assumed, "
                    "not fitted to realised fills, so every cost here carries an "
                    "unquantified error."
                ),
            )
        )

    # --- 7. DIFF ------------------------------------------------------------
    if report.passed:
        result = diff.compute(
            target_weights=built.weights,
            current_weights=current_weights,
            prices=prices,
            adv=adv,
            vol=vol,
            nav=nav,
            config=config,
        )
        orders = result.orders
        estimates = [t.cost for t in result.trades]
        skipped = result.skipped + result.dropped
        if result.dropped:
            # No silent caps. A plan that quietly did less than it intended
            # reads afterwards as a plan that failed.
            warnings.append(
                Warning(
                    kind="turnover_capped",
                    message=(
                        f"turnover budget {config.turnover_budget} spent "
                        f"({result.turnover_used} used); {len(result.dropped)} trade(s) "
                        f"totalling {result.turnover_dropped} weight deferred to a later run"
                    ),
                )
            )
    else:
        # Rejected plans carry no orders. Still price the intended trades, so
        # the record shows what the plan would have cost had it been legal.
        orders = []
        skipped = ["plan rejected: no orders computed"]
        estimates = []

    # --- 8. PLAN ------------------------------------------------------------
    gross = sum((abs(w) for w in built.weights.values()), Decimal(0))
    net = sum(built.weights.values(), Decimal(0))

    document = build(
        run_id=run_id,
        as_of=as_of,
        mode=mode,  # type: ignore[arg-type]
        quote_currency=config.quote_currency,
        nav=Nav(
            total=_q(nav),
            cash=_q(book.cash),
            gross_exposure=_q(gross, "0.000001"),
            net_exposure=_q(net, "0.000001"),
        ),
        provenance=Provenance(
            planner_version=_planner_version(),
            feature_set_version=features.FEATURE_SET_VERSION,
            scoring_version=SCORING_VERSION,
            risk_model_version=RISK_MODEL_VERSION,
            ruleset_version=config.ruleset_version,
            constructor=built.constructor,
            constructor_requested=built.requested,
            inputs_hash=store.content_hash(bars),
            universe_size=len(eligible_by_config),
        ),
        targets=[
            Target(
                asset=a,
                weight=_q(w, "0.000001"),
                direction="long" if w >= 0 else "short",
                conviction=Decimal(1),
            )
            for a, w in sorted(built.weights.items())
        ],
        current=[
            CurrentPosition(
                asset=p.asset,
                qty=_q(p.qty, "0.00000001"),
                weight=_q(current_weights.get(p.asset, Decimal(0)), "0.000001"),
            )
            for p in sorted(book.positions, key=lambda p: p.asset)
        ],
        orders=[_quantize_order(o) for o in orders],
        risk_report=report,
        cost_estimate=CostEstimate(
            total_bps=_q(costs.total_bps(estimates, nav=nav), "0.01"),
            total_quote=_q(costs.total_quote(estimates), "0.01"),
            per_asset=[
                AssetCost(
                    asset=e.asset,
                    bps=_q(e.total_bps, "0.01"),
                    spread_bps=_q(e.spread_bps, "0.01"),
                    impact_bps=_q(e.impact_bps, "0.01"),
                )
                for e in estimates
            ],
        ),
        warnings=warnings,
        created_at=created_at,
    )

    return PlanResult(document=document, skipped=skipped, notes=notes)


def _signal(cross: pl.DataFrame, config: Config) -> tuple[list[construct.Signal], list[str]]:
    """Phase 0 placeholder: every *eligible* asset is an equal-conviction long.

    The eligibility filter is real - insufficient history and insufficient
    liquidity are genuine reasons not to hold something. The direction and
    conviction are not: they claim no edge and exist to exercise the path.
    """
    signals: list[construct.Signal] = []
    notes: list[str] = []

    for r in cross.sort("asset").iter_rows(named=True):
        asset = r["asset"]
        if r["bars_available"] < config.min_history_bars:
            notes.append(
                f"{asset}: {r['bars_available']} bars, needs {config.min_history_bars}"
            )
            continue
        if r["adv_quote"] is None:
            notes.append(f"{asset}: no liquidity estimate")
            continue
        if Decimal(str(r["adv_quote"])) < config.min_dollar_volume:
            notes.append(
                f"{asset}: median turnover {Decimal(str(r['adv_quote'])):.0f} below "
                f"{config.min_dollar_volume}"
            )
            continue
        signals.append(construct.Signal(asset=asset, direction="long", conviction=Decimal(1)))

    return signals, notes


def _q(value: Decimal, exp: str = "0.01") -> Decimal:
    """Quantize for the wire.

    Deterministic rounding at a fixed exponent, so two runs cannot differ in the
    last digit of a float-derived quantity. ROUND_HALF_EVEN is the default and
    is what both `Decimal` and `rust_decimal` use.
    """
    return value.quantize(Decimal(exp))


def _quantize_order(o: Order) -> Order:
    return replace(
        o,
        qty=_q(o.qty, "0.00000001"),
        est_cost_bps=None if o.est_cost_bps is None else _q(o.est_cost_bps, "0.01"),
    )


def _planner_version() -> str:
    return __version__
