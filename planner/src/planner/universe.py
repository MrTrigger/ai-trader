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

Phase 0 records an explicitly configured list. That is honest as a forward
record - it is what was eligible on the day it was written - and it is
**not** usable for backtesting history that predates the first snapshot. Phase 1
replaces the configured list with a real ranking source; the storage contract
here does not change.
"""

from __future__ import annotations

import json
from dataclasses import asdict
from datetime import date, datetime, timezone
from pathlib import Path

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
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
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
