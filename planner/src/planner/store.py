"""Partitioned Parquet bar store.

Layout:

    data/bars/asset=BTC/interval_s=86400/2026.parquet
    data/bars/asset=BTC/interval_s=3600/2026-07.parquet
    data/bars/_manifests/<utc-stamp>-<asset>-<interval>.json

Bars are immutable. Fix the source, not the store. A re-pull of an existing
partition merges and de-duplicates on timestamp keeping the newer rows, so an
exchange correction lands cleanly without the store ever holding a second truth
for the same bar.

Every write emits a manifest - source, range, row count, validation issues,
content hash. A plan has to be reproducible, and that starts with knowing
exactly which bytes it was computed from.

Mirrors `trading-journal/backtest/store.py`. The one divergence is the
partition key: daily bars are partitioned by year rather than by month, because
a month of daily bars is thirty rows and a Parquet file per thirty rows is all
overhead.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path

import polars as pl

from .bars import CONTENT_COLUMNS, Issue, Severity, conform, empty_frame, has_errors, validate

DEFAULT_ROOT = Path(__file__).resolve().parents[3] / "data"

# At or above this interval, one file per year. Below it, one per month.
_YEARLY_PARTITION_FROM_S = 86_400


def bars_dir(root: Path, asset: str, interval_s: int) -> Path:
    return root / "bars" / f"asset={asset}" / f"interval_s={interval_s}"


def partition_key(ts: datetime, interval_s: int) -> str:
    if interval_s >= _YEARLY_PARTITION_FROM_S:
        return f"{ts.year:04d}"
    return f"{ts.year:04d}-{ts.month:02d}"


def content_hash(df: pl.DataFrame) -> str:
    """Stable hash of bar content.

    Sorts before hashing, so the same set of bars hashes identically however the
    frame happened to be ordered. This is what `inputs_hash` in a Plan points at.
    """
    h = hashlib.sha256()
    if df.is_empty():
        return h.hexdigest()[:16]
    ordered = df.sort(["asset", "interval_s", "ts_utc"]).select(CONTENT_COLUMNS)
    for row_hash in ordered.hash_rows(seed=0).to_list():
        h.update(row_hash.to_bytes(8, "little"))
    return h.hexdigest()[:16]


def write(
    df: pl.DataFrame,
    *,
    root: Path = DEFAULT_ROOT,
    source: str,
) -> list[Issue]:
    """Conform, validate, merge and write. Returns the validation issues.

    Raises on any ERROR-level issue without writing anything: a partial write of
    known-bad data is worse than no write, because the next run cannot tell the
    difference between the two.
    """
    df = conform(df)
    issues = validate(df)
    if has_errors(issues):
        detail = "; ".join(
            f"{i.code} x{i.count}: {i.detail}"
            for i in issues
            if i.severity is Severity.ERROR
        )
        raise ValueError(f"refusing to write bars with errors: {detail}")

    if df.is_empty():
        return issues

    for (asset, interval_s), group in df.group_by(["asset", "interval_s"], maintain_order=True):
        asset = str(asset)
        interval_s = int(interval_s)
        target_dir = bars_dir(root, asset, interval_s)
        target_dir.mkdir(parents=True, exist_ok=True)

        group = group.with_columns(
            pl.col("ts_utc")
            .map_elements(lambda ts: partition_key(ts, interval_s), return_dtype=pl.String)
            .alias("_partition")
        )

        for (key,), part in group.group_by(["_partition"], maintain_order=True):
            path = target_dir / f"{key}.parquet"
            merged = _merge(path, part.drop("_partition"))
            merged.write_parquet(path)

        _write_manifest(
            root,
            asset=asset,
            interval_s=interval_s,
            source=source,
            df=group.drop("_partition"),
            issues=issues,
        )

    return issues


def _merge(path: Path, incoming: pl.DataFrame) -> pl.DataFrame:
    """Existing plus incoming, newer wins on a timestamp collision."""
    if not path.exists():
        return incoming.sort("ts_utc")
    existing = pl.read_parquet(path)
    # `incoming` last so unique(keep="last") prefers the fresh rows.
    combined = pl.concat([existing, incoming], how="vertical_relaxed")
    return combined.unique(subset=["asset", "interval_s", "ts_utc"], keep="last").sort("ts_utc")


def _write_manifest(
    root: Path,
    *,
    asset: str,
    interval_s: int,
    source: str,
    df: pl.DataFrame,
    issues: list[Issue],
) -> None:
    manifest_dir = root / "bars" / "_manifests"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc)
    manifest = {
        "written_at": now.isoformat(),
        "asset": asset,
        "interval_s": interval_s,
        "source": source,
        "rows": df.height,
        "first_ts": df["ts_utc"].min().isoformat() if df.height else None,
        "last_ts": df["ts_utc"].max().isoformat() if df.height else None,
        "content_hash": content_hash(df),
        "issues": [asdict(i) | {"severity": i.severity.value} for i in issues],
    }
    stamp = now.strftime("%Y%m%dT%H%M%S%f")
    path = manifest_dir / f"{stamp}-{asset}-{interval_s}.json"
    path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")


def read(
    *,
    root: Path = DEFAULT_ROOT,
    assets: list[str] | None = None,
    interval_s: int,
    until: datetime | None = None,
) -> pl.DataFrame:
    """Read stored bars.

    `until` is inclusive and exists so a replay can be given a hard horizon:
    the planner must never be able to see a bar newer than its `as_of`, and the
    cheapest way to guarantee that is to never load one.
    """
    base = root / "bars"
    if not base.exists():
        return empty_frame()

    dirs = (
        [bars_dir(root, a, interval_s) for a in assets]
        if assets
        else [d / f"interval_s={interval_s}" for d in base.glob("asset=*")]
    )

    frames = [
        pl.read_parquet(f) for d in dirs if d.exists() for f in sorted(d.glob("*.parquet"))
    ]
    if not frames:
        return empty_frame()

    df = conform(pl.concat(frames, how="vertical_relaxed"))
    if until is not None:
        df = df.filter(pl.col("ts_utc") <= until)
    return df


_INVENTORY_SCHEMA = {
    "asset": pl.String,
    "interval_s": pl.Int32,
    "rows": pl.Int64,
    "first_ts": pl.String,
    "last_ts": pl.String,
    "content_hash": pl.String,
}


def inventory(*, root: Path = DEFAULT_ROOT) -> pl.DataFrame:
    """What is in the store: asset, interval, row count, range, hash."""
    base = root / "bars"
    rows: list[dict] = []
    if not base.exists():
        return pl.DataFrame(schema=_INVENTORY_SCHEMA)

    for asset_dir in sorted(base.glob("asset=*")):
        asset = asset_dir.name.removeprefix("asset=")
        for interval_dir in sorted(asset_dir.glob("interval_s=*")):
            interval_s = int(interval_dir.name.removeprefix("interval_s="))
            files = sorted(interval_dir.glob("*.parquet"))
            if not files:
                continue
            df = pl.concat([pl.read_parquet(f) for f in files], how="vertical_relaxed")
            rows.append(
                {
                    "asset": asset,
                    "interval_s": interval_s,
                    "rows": df.height,
                    "first_ts": df["ts_utc"].min().isoformat(),
                    "last_ts": df["ts_utc"].max().isoformat(),
                    "content_hash": content_hash(df),
                }
            )
    return pl.DataFrame(rows, schema=_INVENTORY_SCHEMA) if rows else pl.DataFrame(schema=_INVENTORY_SCHEMA)
