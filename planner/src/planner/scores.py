"""Cross-sectional scoring (design spec §5.2, §9.1).

**Scores are not features, and the separation is deliberate.** A feature is a
raw measurement of one asset — its 30-day return, its realised vol. A score is a
*cross-sectional transform*: rank this asset's measurement against the other
assets in its group, blend the ranks, weight the blend. The distinction matters
because a group-relative rank is not reproducible from one asset's row alone. It
depends on which assets were in the universe that day, which is why a score is
only replayable when stored next to the `universe_members` snapshot that
produced it.

The framework is shared across asset classes and the factors are not (§9.1).
Sub-factors, equal weight within a parent, group-relative percentile rank,
weighted composite, degenerate flags — all of that is here and none of it knows
what a crypto factor is. Momentum survives into equities; emissions and TVL do
not, and book-to-price does not come the other way.

## Honesty rules

Two things can go wrong, and neither may be silent:

1. **A measurement is missing.** Insufficient history, a null column. The asset
   gets the neutral score and a flag — never a flat 50 that reads downstream as
   a real measurement of average-ness (§5.2).
2. **The cross-section is too small to rank within.** A percentile rank among
   two assets carries almost no information, and among one it carries none at
   all: the "rank" is a foregone conclusion. Same treatment — neutral, flagged.

Both flow into the plan as `degenerate_feature` disclosures, reported above the
numbers rather than beneath them (§12).
"""

from __future__ import annotations

from dataclasses import dataclass, field
from decimal import Decimal

import polars as pl

SCORING_VERSION = "sc-phase1-1"

#: The score assigned when nothing can honestly be measured. Mid-scale, so a
#: degenerate factor neither helps nor hurts the composite it feeds.
NEUTRAL = 50.0

#: Below this many assets, a within-group percentile rank is not a measurement.
#: Five is a judgement, not a derivation: it is the point at which a quintile
#: means something. Configurable because the right answer depends on how the
#: universe is grouped, not on anything universal.
DEFAULT_MIN_GROUP_SIZE = 5

#: The group every asset falls into when no grouping is supplied. One group
#: means "rank against the whole universe", which for a 30-name crypto book is
#: usually what you want — crypto sectors are too small to rank within.
UNGROUPED = "all"


@dataclass(frozen=True)
class SubFactor:
    """One measurable input to a parent factor.

    `column` names a column in the feature frame. `higher_is_better` is the only
    place a factor's *direction* is declared, and it is declared once rather
    than by negating the underlying feature — a feature called `vol_30` should
    mean volatility everywhere, not "volatility, but backwards, in this one
    context".
    """

    column: str
    higher_is_better: bool = True


@dataclass(frozen=True)
class Factor:
    """A parent factor: its sub-factors, equal-weighted, and its composite weight.

    Equal weight *within* a parent is §9.1's rule and it is a discipline rather
    than a discovery — it keeps the researcher-degrees-of-freedom budget on the
    composite weights, where the sweep can see it, instead of scattering free
    parameters through every sub-factor.
    """

    name: str
    sub_factors: tuple[SubFactor, ...]
    weight: Decimal = Decimal(1)

    def __post_init__(self) -> None:
        if not self.sub_factors:
            raise ValueError(f"factor {self.name!r} has no sub-factors")
        if self.weight < 0:
            raise ValueError(f"factor {self.name!r} has a negative weight")


#: A starting cross-section to look at — **not a chosen strategy**.
#:
#: §10.2 leaves the strategy undecided on purpose and this does not decide it.
#: Three conventional, cheap, price-and-volume-derived factors — the one row of
#: §7.3's table where backtesting is both valid and cheap. It has not been near
#: the harness, it claims no edge, and nothing in the decision path consumes it.
#: Its job is to make the cross-section *visible*, so that a strategy can be
#: chosen against evidence rather than against argument.
BASELINE = (
    Factor(
        name="momentum",
        sub_factors=(SubFactor("ret_30"), SubFactor("ret_90")),
        weight=Decimal(2),
    ),
    Factor(
        name="low_vol",
        sub_factors=(SubFactor("vol_30", higher_is_better=False),),
        weight=Decimal(1),
    ),
    Factor(
        name="liquidity",
        sub_factors=(SubFactor("adv_quote"),),
        weight=Decimal(1),
    ),
)


@dataclass(frozen=True)
class ScoreResult:
    """The scored cross-section, and what it could not honestly measure.

    `frame` carries one row per asset: every sub-factor percentile, every parent
    factor score, the composite, the group key, and the flags. `disclosures` is
    the human-readable form of the flags, for the plan's warnings.
    """

    frame: pl.DataFrame
    disclosures: list[str] = field(default_factory=list)
    scoring_version: str = SCORING_VERSION

    def composite(self) -> dict[str, float]:
        return dict(zip(self.frame["asset"].to_list(), self.frame["composite"].to_list()))

    def flags_for(self, asset: str) -> list[str]:
        row = self.frame.filter(pl.col("asset") == asset)
        return [] if row.is_empty() else list(row["degenerate_flags"][0])


def percentile_column(name: str) -> str:
    return f"pct_{name}"


def factor_column(name: str) -> str:
    return f"factor_{name}"


def score(
    cross: pl.DataFrame,
    *,
    factors: tuple[Factor, ...],
    groups: dict[str, str] | None = None,
    min_group_size: int = DEFAULT_MIN_GROUP_SIZE,
) -> ScoreResult:
    """Score one point-in-time cross-section.

    `cross` is one row per asset — `features.latest(...)`, not the full history.
    Scoring a whole history at once would silently rank each asset against the
    universe of *its own* bar, which is right, but it would also make it easy to
    forget that the ranking is per-timestamp. One cross-section at a time makes
    the point-in-time nature structural rather than remembered.
    """
    if not factors:
        raise ValueError("scoring needs at least one factor")
    if cross.is_empty():
        return ScoreResult(frame=cross, disclosures=["no assets to score"])

    total_weight = sum((f.weight for f in factors), Decimal(0))
    if total_weight <= 0:
        raise ValueError("factor weights sum to zero; the composite would be undefined")

    groups = groups or {}
    df = cross.sort("asset").with_columns(
        pl.col("asset")
        .map_elements(lambda a: groups.get(a, UNGROUPED), return_dtype=pl.String)
        .alias("group_key")
    )

    sizes = dict(
        zip(
            *df.group_by("group_key")
            .len()
            .sort("group_key")
            .select(["group_key", "len"])
            .to_dict(as_series=False)
            .values()
        )
    )
    small = {g for g, n in sizes.items() if n < min_group_size}

    flags: dict[str, list[str]] = {a: [] for a in df["asset"].to_list()}
    disclosures: list[str] = []

    for group in sorted(small):
        members = sorted(df.filter(pl.col("group_key") == group)["asset"].to_list())
        disclosures.append(
            f"group {group!r} has {sizes[group]} asset(s), fewer than the {min_group_size} "
            f"a percentile rank needs to mean anything: {', '.join(members)} scored neutral"
        )

    df = _rank_sub_factors(df, factors, small, flags, disclosures)
    df = _blend(df, factors, total_weight)

    df = df.with_columns(
        pl.col("asset")
        .map_elements(lambda a: flags.get(a, []), return_dtype=pl.List(pl.String))
        .alias("degenerate_flags"),
        pl.lit(SCORING_VERSION).alias("scoring_version"),
    )

    return ScoreResult(frame=df, disclosures=disclosures)


def _rank_sub_factors(
    df: pl.DataFrame,
    factors: tuple[Factor, ...],
    small: set[str],
    flags: dict[str, list[str]],
    disclosures: list[str],
) -> pl.DataFrame:
    """Group-relative percentile rank, one column per sub-factor."""
    for factor in factors:
        for sub in factor.sub_factors:
            if sub.column not in df.columns:
                raise ValueError(
                    f"factor {factor.name!r} wants column {sub.column!r}, "
                    f"which the feature frame does not have"
                )

            out = percentile_column(sub.column)
            df = df.with_columns(_percentile(sub).alias(out))

            # A null measurement and an unrankable group are the same outcome -
            # neutral - reached two different ways, and both are recorded so the
            # plan can say which.
            missing = sorted(
                df.filter(pl.col(sub.column).is_null())["asset"].to_list()
            )
            for asset in missing:
                flags[asset].append(f"{factor.name}/{sub.column}:no_measurement")
            if missing:
                disclosures.append(
                    f"{sub.column} is missing for {', '.join(missing)}: scored neutral "
                    f"in factor {factor.name!r} rather than contributing a measurement"
                )

            unrankable = sorted(
                df.filter(pl.col("group_key").is_in(list(small)))["asset"].to_list()
            )
            for asset in unrankable:
                flags[asset].append(f"{factor.name}/{sub.column}:small_group")

            df = df.with_columns(
                pl.when(pl.col(sub.column).is_null() | pl.col("group_key").is_in(list(small)))
                .then(pl.lit(NEUTRAL))
                .otherwise(pl.col(out))
                .alias(out)
            )

    return df


def _percentile(sub: SubFactor) -> pl.Expr:
    """Percentile of an asset's measurement within its group, on 0-100.

    `100 * (rank - 0.5) / n` rather than `100 * (rank - 1) / (n - 1)`: the
    midpoint form never hands out a literal 0 or 100, which would say the best
    asset in a five-name group is as extreme as a measurement can get. Ties take
    the average rank, so two identical measurements score identically — anything
    else would make the score depend on row order.
    """
    ranked = pl.col(sub.column).rank(method="average").over("group_key")
    n = pl.col(sub.column).count().over("group_key")
    pct = 100.0 * (ranked - 0.5) / n
    return pct if sub.higher_is_better else 100.0 - pct


def _blend(df: pl.DataFrame, factors: tuple[Factor, ...], total_weight: Decimal) -> pl.DataFrame:
    """Sub-factors equal-weighted into parents, parents weighted into a composite."""
    for factor in factors:
        columns = [pl.col(percentile_column(s.column)) for s in factor.sub_factors]
        df = df.with_columns(
            (sum(columns[1:], columns[0]) / len(columns)).alias(factor_column(factor.name))
        )

    weighted = [
        pl.col(factor_column(f.name)) * float(f.weight / total_weight) for f in factors
    ]
    return df.with_columns(sum(weighted[1:], weighted[0]).alias("composite"))
