"""Step 3: signals — per-asset direction and conviction.

A signal says *what it thinks*, never *what to do*. It emits a direction and a
conviction; the constructor turns those into weights, the risk gate evaluates
the result, and the diff produces orders. Nothing here knows about sizes,
notionals or orders, which is the same boundary §7.1 draws around an LLM and for
the same reason: the output space must not contain an executable instruction.

Signals are selected by name from config, so switching strategies is a config
change with a recorded `ruleset_version` rather than an edit to the decision
path. Two exist:

- `placeholder_equal_long` — Phase 0. Claims no edge and says so on every plan.
- `xs_momentum` — the Phase 1 candidate. See below.

## `xs_momentum`, and the claim it makes

Rank the eligible cross-section on the return from *t−30d to t−7d* and hold the
top names long.

**The skip period is the part that is not obvious.** Short-horizon reversal is
well documented in crypto. A plain 30-day momentum measure contains last week's
reversal, so the two effects partially cancel and the result measures neither
cleanly. Dropping the recent week is the standard equity 12−1 construction
adapted to a shorter horizon.

**The mechanism, stated** — because "we rank things" is not a claim (§7.6):
slow diffusion of information in a market with high retail participation and
fragmented attention, so recent relative strength persists over weeks. That is
the hypothesis. The Phase 1 gate is what decides whether it survives contact
with costs, and it is meant to be capable of saying no.

**How it most likely dies, and what would catch it:** long-only crypto momentum
is a leveraged BTC bet in a rising market, and an attribution that ignores beta
will call that alpha. `max_benchmark_beta` (§6.2) is the constraint that stops
the book expressing it, and the beta-neutral residual is what the gate should be
read on. If that residual is flat, this is beta and it should be deleted.

**Long-only is not a preference.** Shorting spot requires margin and §9.2 puts
leverage above 1× out of scope before Phase 3. Shorts arrive with a venue
decision, not before it.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from decimal import Decimal
from typing import Protocol

import polars as pl

from . import scores
from .config import Config
from .construct import Signal as AssetSignal
from .plan import Warning

RULESET_VERSION = "xs-momentum-1"

#: The momentum column, and the only feature `xs_momentum` ranks on.
MOMENTUM_COLUMN = "ret_30_skip_7"


@dataclass(frozen=True)
class SignalResult:
    signals: list[AssetSignal]
    notes: list[str] = field(default_factory=list)
    warnings: list[Warning] = field(default_factory=list)
    scoring_version: str = "none"


class SignalGenerator(Protocol):
    name: str

    def generate(self, cross: pl.DataFrame, *, config: Config) -> SignalResult: ...


def _eligible(cross: pl.DataFrame, config: Config) -> tuple[list[dict], list[str]]:
    """Assets that can honestly be held, and why the others cannot.

    Real reasons, unlike the placeholder's direction: too little history to
    compute a feature honestly, and too little turnover to trade at size, are
    both grounds not to hold something regardless of what any signal says.
    """
    keep: list[dict] = []
    notes: list[str] = []

    for row in cross.sort("asset").iter_rows(named=True):
        asset = row["asset"]
        if row["bars_available"] < config.min_history_bars:
            notes.append(f"{asset}: {row['bars_available']} bars, needs {config.min_history_bars}")
            continue
        if row["adv_quote"] is None:
            notes.append(f"{asset}: no liquidity estimate")
            continue
        if Decimal(str(row["adv_quote"])) < config.min_dollar_volume:
            notes.append(
                f"{asset}: median turnover {Decimal(str(row['adv_quote'])):.0f} below "
                f"{config.min_dollar_volume}"
            )
            continue
        # A peg is not a position. Stablecoins rank near the top of any
        # liquidity screen and have no momentum to measure, so leaving them in
        # both wastes cross-section slots and hands a risk-parity constructor an
        # asset it would size enormously on a near-zero denominator.
        vol = row.get("vol_30")
        if vol is not None and Decimal(str(vol)) < config.min_volatility:
            notes.append(
                f"{asset}: realised vol {Decimal(str(vol)):.4f} below "
                f"{config.min_volatility} - a peg, not a position"
            )
            continue
        keep.append(row)

    return keep, notes


class PlaceholderEqualLong:
    """Every eligible asset, equal conviction, long. Claims no edge.

    Kept after Phase 0 because it is the null hypothesis a real signal has to
    beat: if `xs_momentum` cannot out-perform holding everything eligible, the
    ranking is not doing anything and the extra turnover is pure cost.
    """

    name = "placeholder_equal_long"

    def generate(self, cross: pl.DataFrame, *, config: Config) -> SignalResult:
        rows, notes = _eligible(cross, config)
        return SignalResult(
            signals=[
                AssetSignal(asset=r["asset"], direction="long", conviction=Decimal(1))
                for r in rows
            ],
            notes=notes,
            warnings=[
                Warning(
                    kind="unenforced_rule",
                    message=(
                        f"signal {self.name!r} is a placeholder and claims no edge. It has "
                        "not been through the backtest harness. No capital until it has."
                    ),
                )
            ],
        )


class LiquidityTop:
    """Hold the most liquid names. The null hypothesis a ranking has to beat.

    This is the honest baseline for a cross-sectional strategy, and a better one
    than `placeholder_equal_long`: it holds the *same number* of names, so the
    comparison isolates **which** names the ranking picked rather than confusing
    it with how many. Holding everything eligible instead is not a comparable
    book, and against a `max_position_count` limit it is not even a legal one -
    every plan is rejected and the "baseline" silently becomes a flat book that
    any strategy beats.

    Ranking by liquidity is roughly "hold the biggest names", which is what a
    crypto index would do. If momentum cannot beat that, the ranking is costing
    turnover and buying nothing.
    """

    name = "liquidity_top"

    def generate(self, cross: pl.DataFrame, *, config: Config) -> SignalResult:
        rows, notes = _eligible(cross, config)
        if not rows:
            return SignalResult(signals=[], notes=notes + ["no eligible assets"])

        ordered = sorted(rows, key=lambda r: (-float(r["adv_quote"]), r["asset"]))
        held = ordered[: config.max_holdings]

        for row in ordered[config.max_holdings :]:
            notes.append(f"{row['asset']}: outside the top {config.max_holdings} by liquidity")

        return SignalResult(
            signals=[
                AssetSignal(
                    asset=r["asset"],
                    direction="long",
                    conviction=Decimal(1),
                    volatility=(None if r.get("vol_30") is None else Decimal(str(r["vol_30"]))),
                )
                for r in held
            ],
            notes=notes,
            warnings=[
                Warning(
                    kind="unenforced_rule",
                    message=(
                        f"signal {self.name!r} is a baseline, not a strategy: it claims no "
                        "edge and exists to be beaten."
                    ),
                )
            ],
        )


class CrossSectionalMomentum:
    """Rank on skip-period momentum; hold the top `max_position_count` long."""

    name = "xs_momentum"

    def generate(self, cross: pl.DataFrame, *, config: Config) -> SignalResult:
        rows, notes = _eligible(cross, config)
        warnings: list[Warning] = []

        if MOMENTUM_COLUMN not in cross.columns:
            raise ValueError(
                f"{self.name} needs the {MOMENTUM_COLUMN!r} feature; the feature set "
                f"({len(cross.columns)} columns) does not provide it"
            )

        if not rows:
            return SignalResult(signals=[], notes=notes + ["no eligible assets"])

        eligible = pl.DataFrame(rows)
        factor = scores.Factor(
            name="momentum",
            sub_factors=(scores.SubFactor(MOMENTUM_COLUMN),),
            weight=Decimal(1),
        )
        scored = scores.score(
            eligible,
            factors=(factor,),
            groups=None,  # rank across the whole eligible universe, not per cluster
            min_group_size=config.min_cross_section,
        )

        for note in scored.disclosures:
            warnings.append(Warning(kind="degenerate_feature", message=note))

        # A cross-section too small to rank is not a ranking. Holding the
        # "top N" of four assets is holding four assets and calling it a
        # signal, so the honest answer is no position at all.
        if any("scored neutral" in note for note in scored.disclosures):
            return SignalResult(
                signals=[],
                notes=notes + ["cross-section too small to rank; target is flat"],
                warnings=warnings,
                scoring_version=scored.scoring_version,
            )

        ordered = scored.frame.sort(
            ["composite", "asset"], descending=[True, False]
        ).head(config.max_holdings)

        signals = [
            AssetSignal(
                asset=row["asset"],
                direction="long",
                conviction=Decimal(str(row["composite"])).quantize(Decimal("0.0001")),
                volatility=(
                    None if row.get("vol_30") is None else Decimal(str(row["vol_30"]))
                ),
            )
            for row in ordered.iter_rows(named=True)
        ]

        held = {s.asset for s in signals}
        for row in scored.frame.sort("composite", descending=True).iter_rows(named=True):
            if row["asset"] not in held:
                notes.append(
                    f"{row['asset']}: momentum score {row['composite']:.1f}, outside the "
                    f"top {config.max_holdings}"
                )

        return SignalResult(
            signals=signals, notes=notes, warnings=warnings, scoring_version=scored.scoring_version
        )


_REGISTRY: dict[str, SignalGenerator] = {
    generator.name: generator
    for generator in (PlaceholderEqualLong(), LiquidityTop(), CrossSectionalMomentum())
}


def get(name: str) -> SignalGenerator:
    if name not in _REGISTRY:
        raise ValueError(f"unknown signal {name!r}; have {sorted(_REGISTRY)}")
    return _REGISTRY[name]
