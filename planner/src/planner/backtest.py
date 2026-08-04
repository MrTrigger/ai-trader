"""The backtest: `pipeline.run` replayed over history (design spec §2.3, §3.1).

**There is no second engine here, and that is the whole design.** This module
loops over dates, calls `pipeline.run(as_of=T)` — the same function the live
planner calls — fills the orders it produced against the bar that opened at `T`,
and steps forward. Steps 1–8 have exactly one implementation, so the thing
measured here is the thing that would be run. A harness with its own engine
cannot make that guarantee no matter how good the engine is (§0.1).

## Causality

Two mechanisms, and neither relies on remembering to be careful:

**The planner cannot see past its horizon.** `store.read(until=...)` never loads
a bar newer than `usable_horizon(T)`, so a bar the decision may not use is not
merely ignored — it is absent. `pipeline.run` re-derives that horizon itself.

**Fills happen after the decision.** A decision stamped `T` uses bars that
closed at or before `T`. The next tradeable moment is the bar that *opens* at
`T` — the one the planner deliberately excluded as still forming. Filling at its
open is the earliest honest price, and it is the same number as the last usable
bar's close, since `open[t] == close[t-1]` (§12). Filling at that bar's *close*
instead would be a free look at a whole interval of price action.

## What the fill model does and does not claim

`sim` here is pessimistic and crude: cross the spread, pay commission, no
partial fills, no queue, no market impact beyond what the cost model already
estimated. Crude is the right amount of sophistication for a daily rebalancer,
and every simplification errs against the strategy.

It is **not** the `paper` venue (§3.5). That one is Rust, runs inside the real
executor against live prices, and exists to exercise the operational path. This
one answers "would this have made money", which is a different question and is
the only one Phase 1 asks.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from decimal import Decimal
from pathlib import Path

import polars as pl

from . import pipeline, state, store
from .config import Config

#: Annualisation factor. Crypto trades every day, so 365 — using the equity
#: convention of 252 here would overstate Sharpe by about 20%.
CRYPTO_YEAR = 365

#: Below this many rebalances, nothing computed here supports a conclusion.
#: §7.5 does the arithmetic: separating a Sharpe of 0.5 from zero at two
#: standard errors takes on the order of sixteen years. This constant is not
#: that threshold — it is merely the point below which a result is not even
#: worth printing without a warning above it.
INSUFFICIENT_SAMPLE_N = 60


@dataclass(frozen=True)
class Fill:
    ts: datetime
    asset: str
    side: str
    qty: Decimal
    price: Decimal
    fee: Decimal
    reason: str

    @property
    def signed_qty(self) -> Decimal:
        return self.qty if self.side == "buy" else -self.qty

    @property
    def notional(self) -> Decimal:
        return self.qty * self.price


@dataclass(frozen=True)
class Step:
    """One rebalance: what was decided, what it cost, and where NAV landed."""

    as_of: datetime
    nav: Decimal
    cash: Decimal
    gross_exposure: Decimal
    status: str
    fills: list[Fill]
    plan_id: str
    warnings: list[str]

    @property
    def traded_notional(self) -> Decimal:
        return sum((f.notional for f in self.fills), Decimal(0))

    @property
    def fees(self) -> Decimal:
        return sum((f.fee for f in self.fills), Decimal(0))


@dataclass(frozen=True)
class Metrics:
    """Portfolio metrics. Every one carries `n`, and `n` is rebalances.

    Not trades: a rebalance is the decision this system actually makes, and
    counting fills instead would inflate the sample by the width of the book
    without adding an independent observation.
    """

    n: int
    total_return: Decimal
    cagr: float
    volatility: float
    sharpe: float
    max_drawdown: Decimal
    turnover_per_rebalance: Decimal
    cost_drag_bps: Decimal
    rejected: int

    @property
    def insufficient_sample(self) -> bool:
        return self.n < INSUFFICIENT_SAMPLE_N


@dataclass(frozen=True)
class Result:
    steps: list[Step]
    metrics: Metrics
    disclosures: list[str] = field(default_factory=list)
    slippage_multiple: Decimal = Decimal(1)

    @property
    def nav_series(self) -> list[tuple[datetime, Decimal]]:
        return [(s.as_of, s.nav) for s in self.steps]


class SimFill:
    """Fills against the bar that opens at the decision timestamp.

    `commission_bps` and `slippage_bps` are charged separately because they
    behave differently under the §9 sensitivity test: slippage is a modelling
    guess and gets doubled, commission is a published number and does not.
    """

    def __init__(
        self,
        *,
        commission_bps: Decimal,
        slippage_bps: Decimal,
        slippage_multiple: Decimal = Decimal(1),
    ) -> None:
        self.commission_bps = commission_bps
        self.slippage = slippage_bps * slippage_multiple

    def price(self, side: str, open_price: Decimal) -> Decimal:
        """Cross the spread. A buy pays up, a sell gets less. Never the reverse."""
        edge = open_price * self.slippage / Decimal(10_000)
        return open_price + edge if side == "buy" else open_price - edge

    def fee(self, notional: Decimal) -> Decimal:
        return notional * self.commission_bps / Decimal(10_000)


def replay(
    *,
    config: Config,
    start: datetime,
    end: datetime,
    data_root: Path,
    initial_cash: Decimal,
    book_path: Path | None = None,
    slippage_multiple: Decimal = Decimal(1),
) -> Result:
    """Replay the decision path from `start` to `end`, inclusive.

    A gate failure at one date is recorded and skipped rather than aborting the
    replay: a run that stops on missing data is the *correct* live behaviour
    (§0.3), and a backtest that hid those dates would report a strategy that
    traded on days the live system would have refused to.
    """
    if start > end:
        raise ValueError(f"start {start.date()} is after end {end.date()}")

    # The decision cadence, which is not the bar interval. Rebalancing weekly
    # over daily bars leaves the features untouched and only changes how often
    # they are acted on - which is what makes a frequency sweep measure one
    # thing (§10.3).
    step = timedelta(seconds=config.interval_s * max(1, config.rebalance_every))
    fills_model = SimFill(
        commission_bps=config.costs.commission_bps,
        slippage_bps=config.costs.spread_bps,
        slippage_multiple=slippage_multiple,
    )

    book = state.Portfolio(cash=initial_cash, positions=[], as_of=start)
    steps: list[Step] = []
    disclosures: list[str] = []
    gate_failures = 0

    # Execution prices come from the bar opening at each decision timestamp -
    # the bar the planner itself excluded as still forming. Loaded once.
    # One bar interval past the last decision, not one rebalance step: the
    # execution bar is always the *next bar*, however far apart decisions are.
    opens = _open_prices(
        config, data_root, start, end + timedelta(seconds=config.interval_s)
    )

    as_of = start
    while as_of <= end:
        book_file = book_path or (data_root / "_backtest_book.json")
        state.save(book, book_file)

        try:
            result = pipeline.run(
                as_of=as_of,
                config=config,
                mode="dry",
                data_root=data_root,
                book_path=book_file,
                created_at=as_of,
            )
        except pipeline.GateFailure as exc:
            gate_failures += 1
            disclosures.append(f"{as_of.date()}: gate failed, no plan ({exc})")
            as_of += step
            continue

        doc = result.document
        book, filled = _apply(doc, book, opens.get(as_of, {}), fills_model, as_of)

        prices = opens.get(as_of, {})
        nav = _mark(book, prices)
        steps.append(
            Step(
                as_of=as_of,
                nav=nav,
                cash=book.cash,
                gross_exposure=Decimal(doc["nav"]["gross_exposure"]),
                status=doc["status"],
                fills=filled,
                plan_id=doc["plan_id"],
                warnings=[w["message"] for w in doc["warnings"]],
            )
        )
        as_of += step

    if gate_failures:
        disclosures.insert(
            0,
            f"{gate_failures} of {gate_failures + len(steps)} dates produced no plan at all "
            "(gates failed). Those days are absent from every number below, which is what "
            "the live system would also have done - it would have stood still.",
        )

    return Result(
        steps=steps,
        metrics=metrics(steps, interval_s=config.interval_s),
        disclosures=disclosures + _standing_disclosures(config, steps, slippage_multiple),
        slippage_multiple=slippage_multiple,
    )


def _open_prices(
    config: Config, data_root: Path, start: datetime, end: datetime
) -> dict[datetime, dict[str, Decimal]]:
    """Execution prices, keyed by the timestamp the bar OPENS at.

    Reading opens rather than closes is the causal choice and is worth being
    explicit about: the close of the bar opening at `T` is a whole interval of
    information the decision at `T` did not have.
    """
    bars = store.read(root=data_root, interval_s=config.interval_s, until=end)
    if bars.is_empty():
        return {}

    window = bars.filter((pl.col("ts_utc") >= start) & (pl.col("ts_utc") <= end))
    out: dict[datetime, dict[str, Decimal]] = {}
    for row in window.iter_rows(named=True):
        out.setdefault(row["ts_utc"], {})[row["asset"]] = Decimal(str(row["open"]))
    return out


def _apply(
    doc: dict,
    book: state.Portfolio,
    prices: dict[str, Decimal],
    model: SimFill,
    as_of: datetime,
) -> tuple[state.Portfolio, list[Fill]]:
    """Fill a plan's orders and return the resulting book.

    An order for an asset with no execution price is **dropped and disclosed**,
    not filled at a stale price. The live executor would have had nothing to
    trade against either.
    """
    qty = {p.asset: p.qty for p in book.positions}
    cash = book.cash
    fills: list[Fill] = []

    for order in doc["orders"]:
        asset = order["asset"]
        if asset not in prices:
            continue

        price = model.price(order["side"], prices[asset])
        size = Decimal(order["qty"])
        notional = size * price
        fee = model.fee(notional)

        cash += (-notional if order["side"] == "buy" else notional) - fee
        qty[asset] = qty.get(asset, Decimal(0)) + (
            size if order["side"] == "buy" else -size
        )

        fills.append(
            Fill(
                ts=as_of,
                asset=asset,
                side=order["side"],
                qty=size,
                price=price,
                fee=fee,
                reason=order["reason"],
            )
        )

    positions = [
        state.Position(asset=a, qty=q) for a, q in sorted(qty.items()) if q != 0
    ]
    return state.Portfolio(cash=cash, positions=positions, as_of=as_of), fills


def _mark(book: state.Portfolio, prices: dict[str, Decimal]) -> Decimal:
    """NAV at execution prices, skipping positions this bar cannot price.

    `state.Portfolio.nav` refuses to mark an unpriced holding, which is right
    for a live decision. Here a missing price is a data gap in history rather
    than a reason to abort the whole replay, so it is carried at its last known
    contribution of zero and the step still records.
    """
    total = book.cash
    for p in book.positions:
        if p.asset in prices:
            total += p.qty * prices[p.asset]
    return total


def metrics(steps: list[Step], *, interval_s: int) -> Metrics:
    """Portfolio metrics over the replay.

    Returns are computed on the NAV series, which already includes fees, so cost
    drag is reported separately for attribution rather than added on.
    """
    rejected = sum(1 for s in steps if s.status == "rejected")
    if not steps:
        return Metrics(
            n=0,
            total_return=Decimal(0),
            cagr=0.0,
            volatility=0.0,
            sharpe=0.0,
            max_drawdown=Decimal(0),
            turnover_per_rebalance=Decimal(0),
            cost_drag_bps=Decimal(0),
            rejected=0,
        )

    navs = [s.nav for s in steps]
    traded = sum((s.traded_notional for s in steps), Decimal(0))
    fees = sum((s.fees for s in steps), Decimal(0))
    average_nav = sum(navs, Decimal(0)) / len(navs)
    turnover = (traded / len(steps) / average_nav) if average_nav else Decimal(0)
    cost_drag = (fees / average_nav * Decimal(10_000)) if average_nav else Decimal(0)

    # What a single step *can* honestly report: it traded, so it has turnover
    # and it paid fees. What it cannot report is a return, because a return
    # needs two marks. Zeroing the whole block would have hidden real costs
    # behind an accident of window length.
    if len(steps) < 2:
        return Metrics(
            n=len(steps),
            total_return=Decimal(0),
            cagr=0.0,
            volatility=0.0,
            sharpe=0.0,
            max_drawdown=Decimal(0),
            turnover_per_rebalance=turnover,
            cost_drag_bps=cost_drag,
            rejected=rejected,
        )

    first, last = navs[0], navs[-1]
    total_return = (last - first) / first if first else Decimal(0)

    periods_per_year = CRYPTO_YEAR * 86_400 / interval_s
    years = (len(navs) - 1) / periods_per_year
    cagr = (float(last / first) ** (1 / years) - 1) if years > 0 and first > 0 else 0.0

    rets = [
        float((navs[i] - navs[i - 1]) / navs[i - 1]) for i in range(1, len(navs)) if navs[i - 1]
    ]
    mean = sum(rets) / len(rets) if rets else 0.0
    variance = sum((r - mean) ** 2 for r in rets) / (len(rets) - 1) if len(rets) > 1 else 0.0
    volatility = math.sqrt(variance * periods_per_year)
    # Excess over zero, not over a risk-free rate. Stated rather than assumed:
    # a Sharpe against 0% is optimistic and the number should be read that way.
    sharpe = (mean * periods_per_year) / volatility if volatility > 0 else 0.0

    peak, drawdown = navs[0], Decimal(0)
    for nav in navs:
        peak = max(peak, nav)
        if peak > 0:
            drawdown = min(drawdown, (nav - peak) / peak)

    return Metrics(
        n=len(steps),
        total_return=total_return,
        cagr=cagr,
        volatility=volatility,
        sharpe=sharpe,
        max_drawdown=drawdown,
        turnover_per_rebalance=turnover,
        cost_drag_bps=cost_drag,
        rejected=rejected,
    )


def _standing_disclosures(
    config: Config, steps: list[Step], slippage_multiple: Decimal
) -> list[str]:
    """What must be read before any number this module produced (§12)."""
    out: list[str] = []

    if len(steps) < INSUFFICIENT_SAMPLE_N:
        out.append(
            f"insufficient sample: {len(steps)} rebalances, fewer than {INSUFFICIENT_SAMPLE_N}. "
            "No conclusion about edge is available from this, and §7.5 argues none is "
            "available from a much longer run either."
        )
    if not config.costs.calibrated:
        out.append(
            "the cost model is uncalibrated: its impact coefficient is assumed rather than "
            "fitted to realised fills, so every cost number here carries an unquantified error."
        )
    if slippage_multiple != 1:
        out.append(
            f"slippage is scaled {slippage_multiple}x for this run. It is an error bar, not a "
            "parameter: the question is how much of the result is a modelling artefact."
        )
    rejected = sum(1 for s in steps if s.status == "rejected")
    if rejected:
        out.append(
            f"{rejected} of {len(steps)} plans were rejected by the risk gate and traded nothing."
        )
    out.append(
        "the fill model crosses the spread and charges commission, and models nothing else - "
        "no partial fills, no queue position, no depth. It is pessimistic but crude."
    )
    return out


def sensitivity(
    *,
    config: Config,
    start: datetime,
    end: datetime,
    data_root: Path,
    initial_cash: Decimal,
    multiples: tuple[Decimal, ...] = (Decimal(1), Decimal(2)),
) -> list[Result]:
    """The same replay at several slippage assumptions (§9 Phase 1, §2.2).

    Not a sweep and not an optimisation: these are error bars. An edge that
    evaporates at 2x was a cost artefact, and Phase 1's gate names that test
    explicitly.
    """
    return [
        replay(
            config=config,
            start=start,
            end=end,
            data_root=data_root,
            initial_cash=initial_cash,
            slippage_multiple=m,
        )
        for m in multiples
    ]
