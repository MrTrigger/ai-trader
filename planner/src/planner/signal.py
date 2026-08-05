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
            # Counted from the last discontinuity, so a ticker that changed
            # meaning reads as a young asset - which is what it is.
            broke = " since a price discontinuity" if row.get("had_discontinuity") else ""
            notes.append(
                f"{asset}: {row['bars_available']} bars{broke}, "
                f"needs {config.min_history_bars}"
            )
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


class GaussianChannelBreakout:
    """Hold what is trading above its Gaussian Channel upper band.

    A different *family* from `xs_momentum`, which is why it is worth testing
    after that one failed. Momentum is **relative** — rank assets against each
    other. This is **absolute** — each asset is judged against its own channel,
    and in a market where nothing is breaking out the book is simply flat. §10.2
    names channel-breakout trend-following as a legitimate documented family and
    "a reasonable baseline to beat, not a destination".

    The rules are taken from the TR-GC prompt family, which runs this live:

    - **Enter** when the close is above the upper band.
    - **Exit** when it is not. Here that falls out of target-state convergence —
      an asset no longer breaking out gets no target, and the diff sells it — so
      there is no separate exit rule to keep in step with the entry rule.
    - **Size by breakout recency**: those prompts use 8% of NAV for a breakout
      within 25 days and 2% otherwise. Expressed here as conviction 4:1 and left
      to the constructor, because the thing that differs between this system and
      that one is *not* the signal.

    That difference is worth stating plainly. Those prompts size per asset with
    no portfolio-level cap at all: twenty simultaneous breakouts at 8% "wants"
    160% of NAV, and channel breakouts are correlated by construction, so that
    is the normal case in a rally rather than a tail. Here the same signal is
    bounded by `target_gross_exposure`, `max_position` and `max_position_count`,
    and the risk gate rejects a plan that breaches them. Whether the *signal*
    has content is the question the gate is about to answer; whether it can be
    run without a gross cap is not in question.
    """

    name = "gc_breakout"

    #: Breakouts at least this fresh get the larger conviction. The TR-GC value.
    RECENT_DAYS = 25
    RECENT_CONVICTION = Decimal(4)
    STALE_CONVICTION = Decimal(1)

    def generate(self, cross: pl.DataFrame, *, config: Config) -> SignalResult:
        rows, notes = _eligible(cross, config)
        warnings: list[Warning] = []

        if "gc_breakout_age" not in cross.columns:
            raise ValueError(
                f"{self.name} needs the 'gc_breakout_age' feature; the feature set "
                "does not provide it"
            )

        unwarmed = [r["asset"] for r in rows if r.get("gc_upper") is None]
        if unwarmed:
            notes.append(
                f"{len(unwarmed)} asset(s) have too little history for the channel to "
                "have converged and were not evaluated"
            )

        breaking = [
            r for r in rows if r.get("gc_breakout_age") is not None and r.get("gc_upper")
        ]
        if not breaking:
            # Not a failure. An absolute signal is allowed to say "nothing
            # qualifies", and a flat book is the correct expression of that.
            return SignalResult(
                signals=[],
                notes=notes + ["no asset is above its upper channel; target is flat"],
                warnings=warnings,
            )

        # Freshest breakouts first, matching the sizing preference.
        breaking.sort(key=lambda r: (int(r["gc_breakout_age"]), r["asset"]))
        held = breaking[: config.max_holdings]

        for row in breaking[config.max_holdings :]:
            notes.append(
                f"{row['asset']}: breaking out {int(row['gc_breakout_age'])} bars ago, "
                f"outside the top {config.max_holdings} by recency"
            )

        return SignalResult(
            signals=[
                AssetSignal(
                    asset=r["asset"],
                    direction="long",
                    conviction=(
                        self.RECENT_CONVICTION
                        if int(r["gc_breakout_age"]) <= self.RECENT_DAYS
                        else self.STALE_CONVICTION
                    ),
                    volatility=(None if r.get("vol_30") is None else Decimal(str(r["vol_30"]))),
                )
                for r in held
            ],
            notes=notes,
            warnings=warnings,
        )


def _below_band(row: dict) -> tuple[float, str]:
    """How far under the lower channel band, as a fraction of it.

    Ranks the short leg. Assets with no lower band sort last rather than being
    treated as deeply broken down, which a naive `or 0.0` would do.
    """
    close, lower = row.get("close"), row.get("gc_lower")
    if close is None or lower is None or lower == 0:
        return (float("inf"), row["asset"])
    return ((float(close) - float(lower)) / abs(float(lower)), row["asset"])


class GaussianChannelLongShort:
    """`gc_breakout`'s selection, expressed market-neutral and tilted by regime.

    Long-only, the channel breakout lost 77% against a benchmark that gained 43%
    (§ Phase 1 findings). The selection was not worthless; the *packaging* was.
    Every name it picks is picked for being in an uptrend, so in a falling market
    it is long the least-bad assets in a book with no offset, and the beta buries
    whatever cross-sectional content exists. Holding the same longs against a
    short leg cancels the beta and leaves the spread, which is the only thing the
    signal ever had an opinion about.

    ## The two legs are not symmetric, and that is deliberate

    - **Long** = above the upper channel. A selection.
    - **Short** = eligible, listed, and *not* above it. A residual.

    Making the short leg symmetric (only names below the *lower* band) was tested
    and is worse: min-Sharpe 1.49 against 2.05, and it stands the book down for
    most of 2019-21 for want of three qualifying shorts. The residual form is
    also the more honest description of what is being claimed - the signal has an
    opinion about which assets are strong, not about which are weak, and the
    short leg is a hedge rather than a second forecast.

    **Both legs are perpetuals.** The venue is perps-first, so an asset without
    a listed contract is untradeable in either direction, and funding is PAID on
    the long leg as well as received on the short. That costs roughly a third of
    the funding income a spot long leg would have kept, and more than pays for
    itself in fees: a perp long is charged the perp taker rate rather than the
    materially higher spot one.

    The leg *sizes* need no such rule: `L` thins to three or four names by itself
    when almost nothing is trending, and the short leg fattens in the same move,
    so the book rotates net-short in a downtrend without being told to.

    ## The tilt, and why it lives here

    Gross stays pinned at 1.0 in every state - this is not leverage and §9.2 is
    untouched. Only the split moves, read off the *benchmark's* own channel:

        BTC above its upper band  -> long 0.5 + t, short 0.5 - t
        BTC below its filter      -> long 0.5 - t, short 0.5 + t
        between                   -> 0.5 / 0.5, no opinion

    where `t` scales with the filter's own slope and is capped at `TILT_CAP`.
    A view about market state is a *view*, so it belongs to the signal; the
    constructor's job is to turn views into weights and it should not acquire
    opinions of its own. Expressing the split as conviction is what lets it: with
    `conviction_tilt`, per-name conviction of `leg_weight / names_in_leg`
    normalises to exactly the intended leg weights, so no new interface is
    needed and `max_position` still binds where it should.

    **Weighting the split by breadth instead was tested and refuted** - monotonically
    worse (min-Sharpe 2.06 -> 1.45), because the share of names trending up does
    not predict the forward week (rho +0.09, t 1.45, widest quartile nearly the
    worst). The intuition that a wide long leg should mean a bigger long is a
    good one and the data does not support it.

    **How this most likely dies:** the tilt is one regime detector on one asset,
    and its 48-day read was chosen on the same two windows everything else was.
    If the edge is really the tilt rather than the selection, this is a BTC
    market-timing strategy wearing a long/short costume. The label-shuffle null
    puts that at p=0.040 - evidence, not proof, and paper is what settles it.
    """

    name = "gc_long_short"

    #: Cap on how far the split may lean from neutral. 0.5 is fully one-sided.
    TILT_CAP = Decimal("0.5")
    #: Bars over which the benchmark filter's slope is measured.
    LEAN_BARS = 20
    #: Slope-to-tilt gain. Plateau centre, per §7.4.
    LEAN_SCALE = Decimal(8)
    #: A leg thinner than this is not a portfolio, so the book stands down.
    MIN_LEG = 3

    def _tilt(self, cross: pl.DataFrame, config: Config) -> tuple[Decimal, str]:
        """Read the benchmark's channel state. Returns (tilt, description)."""
        if not config.benchmark:
            return Decimal(0), "no benchmark configured; split held neutral"
        row = cross.filter(pl.col("asset") == config.benchmark)
        if row.is_empty():
            return Decimal(0), f"{config.benchmark} not in the cross-section; split neutral"
        r = row.row(0, named=True)
        if r.get("gc_regime_upper") is None or r.get("gc_regime_filter") is None:
            return Decimal(0), f"{config.benchmark} channel has not converged; split neutral"
        if r["close"] > r["gc_regime_upper"]:
            sign, state = Decimal(1), "above its upper band"
        elif r["close"] < r["gc_regime_filter"]:
            sign, state = Decimal(-1), "below its filter"
        else:
            return Decimal(0), f"{config.benchmark} is inside its channel; split neutral"

        slope = r.get("gc_regime_slope")
        if slope is None:
            return Decimal(0), f"{config.benchmark} slope unavailable; split neutral"
        raw = sign * abs(Decimal(str(slope))) * self.LEAN_SCALE
        tilt = max(-self.TILT_CAP, min(self.TILT_CAP, raw))
        return tilt, (
            f"{config.benchmark} is {state}, filter slope "
            f"{Decimal(str(slope)):.4f} over {self.LEAN_BARS} bars -> tilt {tilt:+.3f}"
        )

    def generate(self, cross: pl.DataFrame, *, config: Config) -> SignalResult:
        rows, notes = _eligible(cross, config)
        warnings: list[Warning] = []

        for needed in ("gc_breakout_age", "perp_listed"):
            if needed not in cross.columns:
                raise ValueError(
                    f"{self.name} needs the {needed!r} column; the feature set does "
                    "not provide it"
                )

        # Both legs are perpetuals, so both need a listed contract. This used to
        # gate the short leg only, on the assumption that the long leg was spot.
        # It is not: the venue is perps-first, which makes an unlisted asset
        # untradeable in either direction rather than merely unshortable.
        warmed = [
            r for r in rows if r.get("gc_upper") is not None and r.get("perp_listed")
        ]
        longs = [r for r in warmed if r.get("gc_breakout_age") is not None]
        shorts = [r for r in warmed if r.get("gc_breakout_age") is None]

        unlisted = sum(
            1 for r in rows if r.get("gc_upper") is not None and not r.get("perp_listed")
        )
        if unlisted:
            notes.append(
                f"{unlisted} eligible asset(s) have no listed perpetual and were not "
                "traded on either side"
            )

        if len(longs) < self.MIN_LEG or len(shorts) < self.MIN_LEG:
            # Not a failure. A book that cannot form two legs is not a
            # market-neutral book, and holding one leg alone would be a
            # directional bet this signal never claimed to have.
            return SignalResult(
                signals=[],
                notes=notes
                + [
                    f"only {len(longs)} long and {len(shorts)} short candidates against a "
                    f"{self.MIN_LEG}-a-side minimum; target is flat"
                ],
                warnings=warnings,
            )

        tilt, why = self._tilt(cross, config)
        notes.append(why)
        long_w = Decimal("0.5") + tilt
        short_w = Decimal("0.5") - tilt

        # Fit inside `max_position_count`. The book naturally wants ~34 names and
        # the limit allows 12, so without this the risk gate rejects every plan -
        # which is the correct behaviour of a gate and the wrong behaviour of a
        # strategy. Truncating here rather than raising the limit keeps the limit
        # a constraint instead of quietly making it a parameter; the strategy was
        # measured under it and holds up (min-Sharpe 1.97 against 2.08 untruncated).
        #
        # The budget splits by leg weight, so a tilted book keeps more names on
        # the side it is leaning into, and neither leg drops below MIN_LEG.
        budget = config.limits.max_position_count
        if len(longs) + len(shorts) > budget:
            n_long = max(self.MIN_LEG, min(len(longs), round(budget * long_w)))
            n_short = max(self.MIN_LEG, min(len(shorts), budget - n_long))
            # Longs by breakout recency: freshest first, as `gc_breakout` sizes.
            longs = sorted(longs, key=lambda r: (int(r["gc_breakout_age"]), r["asset"]))[
                :n_long
            ]
            # The short leg is a residual and carries no score of its own, so it
            # is ranked by the same detector read the other way - furthest below
            # the lower band first. Least arbitrary available, not principled.
            shorts = sorted(shorts, key=_below_band)[:n_short]
            notes.append(
                f"truncated to {len(longs)} long and {len(shorts)} short to fit "
                f"max_position_count={budget}"
            )

        notes.append(
            f"{len(longs)} long at {long_w:.3f} of gross, {len(shorts)} short at "
            f"{short_w:.3f}; gross unchanged at the configured target"
        )

        def sized(rs: list[dict], leg: Decimal, direction: str) -> list[AssetSignal]:
            per = leg / Decimal(len(rs))
            return [
                AssetSignal(
                    asset=r["asset"],
                    direction=direction,
                    conviction=per,
                    volatility=(None if r.get("vol_30") is None else Decimal(str(r["vol_30"]))),
                )
                for r in sorted(rs, key=lambda x: x["asset"])
            ]

        return SignalResult(
            signals=sized(longs, long_w, "long") + sized(shorts, short_w, "short"),
            notes=notes,
            warnings=warnings,
        )


_REGISTRY: dict[str, SignalGenerator] = {
    generator.name: generator
    for generator in (
        PlaceholderEqualLong(),
        LiquidityTop(),
        CrossSectionalMomentum(),
        GaussianChannelBreakout(),
        GaussianChannelLongShort(),
    )
}


def get(name: str) -> SignalGenerator:
    if name not in _REGISTRY:
        raise ValueError(f"unknown signal {name!r}; have {sorted(_REGISTRY)}")
    return _REGISTRY[name]
