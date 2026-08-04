"""Point-in-time universe membership.

The rule this module exists to enforce, from design spec section 11:

    universe_members records what was eligible ON THAT DATE and is never
    backfilled.

Backfilling it is survivorship bias with extra steps. If today's top-30 list is
applied to 2024, the backtest gets a universe selected by having *survived* to
2026 - which is information no 2024 decision could have had, and which flatters
every result computed from it.

So snapshots are append-only observations. A backtest reads the snapshot for its
own date, and if there isn't one, it refuses to run rather than reaching for the
nearest.

Phase 0 recorded an explicitly configured list. `by_liquidity` is the Phase 1
replacement, and the distinction it turns on is worth stating precisely, because
it is the difference between a legitimate reconstruction and a bias:

    Reconstructing a RULE from complete history is point-in-time.
    Reconstructing a SURVIVOR LIST is not.

"Top N by trailing median quote turnover among assets with bars at T" uses only
inputs that were knowable at T, so computing it today for a past date is honest.
Downloading today's top-30 list and applying it to 2024 is not, and no amount of
care makes it so.

**That only holds if the bars include the dead.** Ranking over a store built
from currently-listed symbols reintroduces exactly the bias this module exists
to prevent - see `sources.binance_archive`, which reaches delisted symbols for
this reason.
"""

from __future__ import annotations

import json
from dataclasses import asdict
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import polars as pl

from .sources.base import UniverseMember

DEFAULT_ROOT = Path(__file__).resolve().parents[3] / "data"


def _snapshot_dir(root: Path) -> Path:
    return root / "universe"


def _path(root: Path, day: date) -> Path:
    return _snapshot_dir(root) / f"{day.isoformat()}.json"


def record(
    members: list[UniverseMember],
    *,
    as_of: datetime,
    source: str,
    root: Path = DEFAULT_ROOT,
    overwrite: bool = False,
) -> Path:
    """Append a snapshot for `as_of`'s UTC date.

    Refuses to silently replace an existing snapshot. A universe that changed
    after the fact is either a bug or a decision, and both deserve to be
    explicit rather than absorbed.
    """
    if as_of.tzinfo is None:
        raise ValueError("as_of must be timezone-aware")

    day = as_of.astimezone(timezone.utc).date()
    path = _path(root, day)
    if path.exists() and not overwrite:
        raise FileExistsError(
            f"a universe snapshot for {day} already exists at {path}. "
            "Snapshots are observations, not settings - pass overwrite=True only "
            "if you are correcting a recording error, never to change history."
        )

    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {
        "as_of": as_of.astimezone(timezone.utc).isoformat(),
        "recorded_at": datetime.now(timezone.utc).isoformat(),
        "source": source,
        "members": [asdict(m) for m in members],
    }
    # Bytes, not text: a snapshot is an immutable observation, and its bytes
    # should not depend on which OS recorded it. See plan.canonical_bytes.
    path.write_bytes((json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8"))
    return path


def load(as_of: datetime, *, root: Path = DEFAULT_ROOT) -> list[UniverseMember]:
    """The snapshot for `as_of`'s date, or raise.

    Deliberately does not fall back to the nearest earlier snapshot. Quietly
    substituting a different day's universe is exactly the kind of small
    convenience that makes a backtest unreproducible.
    """
    day = as_of.astimezone(timezone.utc).date()
    path = _path(root, day)
    if not path.exists():
        raise FileNotFoundError(
            f"no universe snapshot for {day}. Snapshots are never backfilled: "
            "record one going forward, or run against a date that has one."
        )
    payload = json.loads(path.read_text(encoding="utf-8"))
    return [UniverseMember(**m) for m in payload["members"]]


def eligible(as_of: datetime, *, root: Path = DEFAULT_ROOT) -> list[str]:
    return [m.asset for m in load(as_of, root=root) if m.eligible]


def snapshots(*, root: Path = DEFAULT_ROOT) -> list[date]:
    d = _snapshot_dir(root)
    if not d.exists():
        return []
    return sorted(date.fromisoformat(p.stem) for p in d.glob("*.json"))


def from_config(assets: list[str], *, reason: str = "configured") -> list[UniverseMember]:
    """Build members from an ordered list, rank by position."""
    return [
        UniverseMember(asset=a.upper(), rank=i + 1, eligible=True, reason=reason)
        for i, a in enumerate(assets)
    ]


def by_liquidity(
    bars: pl.DataFrame,
    *,
    as_of: datetime,
    top_n: int,
    lookback_days: int = 30,
    min_history_bars: int = 90,
    min_turnover: float = 0.0,
) -> list[UniverseMember]:
    """Rank assets by trailing median quote turnover as of `as_of`.

    Every input is knowable at `as_of`: the median is over bars that closed at
    or before it, and an asset with no recent bar is not ranked at all. Given a
    bar store that contains delisted assets, running this for a past date
    reproduces what the rule would have chosen then.

    **Median, not mean.** One exchange-outage day or one listing-day spike
    should not decide what the book is allowed to hold. Same reasoning as
    `features.adv_quote`, and the same window by default.

    Ineligible assets are **returned, not dropped**. A universe snapshot that
    silently omits what it considered is not a record of a decision - and the
    delisted in particular have to stay visible, or reading history back gives
    the survivors again by a different route.
    """
    if top_n <= 0:
        raise ValueError(f"top_n must be positive, got {top_n}")
    if bars.is_empty():
        return []

    as_of = as_of.astimezone(timezone.utc)
    window_start = as_of - timedelta(days=lookback_days)

    # Only bars that closed at or before `as_of`. A bar opening at `as_of` is
    # still forming, and including it would hand the ranking a partial day.
    visible = bars.filter(pl.col("ts_utc") < as_of)
    if visible.is_empty():
        return []

    recent = visible.filter(pl.col("ts_utc") >= window_start)

    stats = (
        visible.group_by("asset")
        .agg(
            pl.len().alias("bars_available"),
            pl.col("ts_utc").max().alias("last_bar"),
        )
        .join(
            recent.group_by("asset").agg(
                pl.col("quote_volume").median().alias("turnover"),
                pl.len().alias("recent_bars"),
            ),
            on="asset",
            how="left",
        )
    )

    # An asset whose newest bar predates the window has stopped trading. That
    # is a delisting, and it is the single most important thing this ranking
    # has to notice rather than skip past.
    newest = visible["ts_utc"].max()
    stale_before = newest - timedelta(days=lookback_days)

    scored = []
    for row in stats.iter_rows(named=True):
        turnover = row["turnover"]
        if row["last_bar"] < stale_before:
            reason = f"no bars since {row['last_bar'].date()}: delisted or halted"
            eligible = False
        elif row["bars_available"] < min_history_bars:
            reason = f"{row['bars_available']} bars, needs {min_history_bars}"
            eligible = False
        elif turnover is None:
            reason = "no turnover estimate in the lookback window"
            eligible = False
        elif turnover < min_turnover:
            reason = f"median turnover {turnover:.0f} below {min_turnover:.0f}"
            eligible = False
        else:
            reason = f"median {lookback_days}d turnover {turnover:.0f}"
            eligible = True
        scored.append((row["asset"], turnover or 0.0, eligible, reason))

    # Rank by turnover among the eligible; ineligible sort after, by name, so
    # the snapshot is stable rather than dependent on frame order.
    scored.sort(key=lambda r: (not r[2], -r[1], r[0]))

    members: list[UniverseMember] = []
    for position, (asset, _turnover, eligible, reason) in enumerate(scored, start=1):
        if eligible and position > top_n:
            eligible = False
            reason = f"rank {position}, outside the top {top_n}"
        members.append(
            UniverseMember(asset=asset, rank=position, eligible=eligible, reason=reason)
        )
    return members
