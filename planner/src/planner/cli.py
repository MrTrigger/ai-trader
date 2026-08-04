"""The CLI. Everything the system can do is a command here.

Design spec section 0.5: reproducible without the model, without the MCP,
without the network. Interactive surfaces are lenses over this, never a
dependency of it - the scheduler runs the same commands you do.

Output discipline, from section 11: **what was not enforced is reported before
any number**, so a plan is never read as more complete than it is.
"""

from __future__ import annotations

import argparse
import sys
import uuid
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path

import polars as pl

from . import inspect as inspect_mod
from . import plan as plan_mod
from . import features, pipeline, scores, state, store, universe
from .config import Config, DEFAULT_CONFIG_PATH
from .sources import BinancePublic


def _utc(text: str) -> datetime:
    dt = datetime.fromisoformat(text)
    return dt.replace(tzinfo=timezone.utc) if dt.tzinfo is None else dt.astimezone(timezone.utc)


def _config(args) -> Config:
    return Config.load(Path(args.config) if args.config else DEFAULT_CONFIG_PATH)


# ---------------------------------------------------------------------------
# data
# ---------------------------------------------------------------------------


def cmd_data_pull(args) -> int:
    config = _config(args)
    end = _utc(args.end) if args.end else datetime.now(timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0
    )
    start = end - timedelta(days=args.days)
    src = BinancePublic()
    try:
        import polars as pl

        frames = []
        for asset in config.universe:
            df = src.fetch_bars(asset, interval_s=config.interval_s, start=start, end=end)
            print(f"  {asset}: {df.height} bars")
            if df.height:
                frames.append(df)
        if not frames:
            print("no bars fetched", file=sys.stderr)
            return 1
        combined = pl.concat(frames, how="vertical_relaxed")
        issues = store.write(combined, root=Path(args.data_root), source=src.name)
    finally:
        src.close()

    print(f"\nwrote {combined.height} bars from {src.name}")
    for i in issues:
        print(f"  [{i.severity.value}] {i.code} x{i.count}: {i.detail}")
    return 0


def cmd_data_inspect(args) -> int:
    config = _config(args)
    inv = store.inventory(root=Path(args.data_root))
    if inv.height == 0:
        print("store is empty")
        return 0
    for r in inv.iter_rows(named=True):
        print(
            f"{r['asset']:<6} {r['interval_s']:>7}s  {r['rows']:>6} bars  "
            f"{r['first_ts'][:10]} .. {r['last_ts'][:10]}  {r['content_hash']}"
        )
    return 0


def cmd_data_verify(args) -> int:
    """Evidence that ts_utc means the bar OPEN. See planner/inspect.py."""
    config = _config(args)
    bars = store.read(root=Path(args.data_root), interval_s=config.interval_s)
    if bars.is_empty():
        print("store is empty", file=sys.stderr)
        return 1
    reports = inspect_mod.continuity(bars)
    failed = 0
    for r in reports:
        print(r.render())
        if not r.ok:
            failed += 1
    if failed:
        print(f"\n{failed} series failed the open/close convention check", file=sys.stderr)
        return 1
    return 0


# ---------------------------------------------------------------------------
# universe
# ---------------------------------------------------------------------------


def cmd_universe_record(args) -> int:
    config = _config(args)
    as_of = _utc(args.as_of) if args.as_of else datetime.now(timezone.utc)
    members = universe.from_config(config.universe, reason="configured (Phase 0)")
    try:
        path = universe.record(
            members,
            as_of=as_of,
            source="config",
            root=Path(args.data_root),
            overwrite=args.overwrite,
        )
    except FileExistsError as exc:
        print(str(exc), file=sys.stderr)
        return 1
    print(f"recorded {len(members)} members for {as_of.date()} -> {path}")
    return 0


def cmd_universe_list(args) -> int:
    days = universe.snapshots(root=Path(args.data_root))
    if not days:
        print("no universe snapshots recorded")
        return 0
    for d in days:
        members = universe.load(datetime(d.year, d.month, d.day, tzinfo=timezone.utc),
                                root=Path(args.data_root))
        eligible = [m.asset for m in members if m.eligible]
        print(f"{d}  {len(eligible):>3} eligible  {', '.join(eligible[:10])}")
    return 0


# ---------------------------------------------------------------------------
# book
# ---------------------------------------------------------------------------


def cmd_book_init(args) -> int:
    """Seed the Phase 0 book.

    Stands in for the venue's balances until there is a venue. From Phase 2 the
    book is read from the venue with a read-only key and reconciled, and this
    command goes away.
    """
    path = Path(args.data_root) / "book.json"
    if path.exists() and not args.force:
        print(f"{path} exists; pass --force to replace it", file=sys.stderr)
        return 1
    book = state.Portfolio(
        cash=Decimal(args.cash), positions=[], as_of=datetime.now(timezone.utc)
    )
    state.save(book, path)
    print(f"book seeded with {args.cash} -> {path}")
    return 0


def cmd_book_show(args) -> int:
    path = Path(args.data_root) / "book.json"
    book = state.load(path)
    print(f"cash {book.cash}")
    for p in book.positions:
        print(f"  {p.asset:<6} {p.qty}")
    if not book.positions:
        print("  (flat)")
    return 0


# ---------------------------------------------------------------------------
# plan
# ---------------------------------------------------------------------------


def cmd_scores(args) -> int:
    """Show the scored cross-section at `as_of`.

    A lens, not a decision: nothing in the decision path consumes these yet.
    §10.2 leaves the strategy undecided, and the point of being able to look at
    a cross-section is to decide it against evidence.
    """
    config = _config(args)
    as_of = _utc(args.as_of) if args.as_of else datetime.now(timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0
    )
    root = Path(args.data_root)

    horizon = pipeline.usable_horizon(as_of, config.interval_s)
    bars = store.read(root=root, interval_s=config.interval_s, until=horizon)
    if bars.is_empty():
        print(f"no bars at or before {horizon.isoformat()}", file=sys.stderr)
        return 2

    try:
        members = universe.load(as_of, root=root)
    except FileNotFoundError as exc:
        print(f"GATE FAILED: {exc}", file=sys.stderr)
        return 2
    eligible = [m.asset for m in members if m.eligible]

    featurizable = list(eligible)
    if config.benchmark and config.benchmark not in featurizable:
        featurizable.append(config.benchmark)

    frame = features.build(
        bars.filter(pl.col("asset").is_in(featurizable)), benchmark=config.benchmark
    )
    cross = features.latest(frame).filter(pl.col("asset").is_in(eligible))

    result = scores.score(
        cross,
        factors=scores.BASELINE,
        groups=config.clusters if args.by_cluster else None,
    )

    # Disclosures first, always. A score read before its caveats has already
    # misled - and a neutral score looks exactly like a measured average one.
    print("DISCLOSURES (read before any number below)")
    print(
        "  ! these factors are a candidate cross-section, not a chosen strategy. "
        "They have not been through the backtest harness and claim no edge."
    )
    for note in result.disclosures:
        print(f"  ! {note}")
    print()

    print(f"as of {as_of.isoformat()}   horizon {horizon.isoformat()}")
    print(f"scoring {result.scoring_version}   features {features.FEATURE_SET_VERSION}")
    print(
        "grouped by "
        + ("configured clusters" if args.by_cluster else f"{scores.UNGROUPED!r} (one cross-section)")
    )

    factor_names = [f.name for f in scores.BASELINE]
    header = f"\n{'asset':<8}{'group':<12}{'composite':>10}"
    for name in factor_names:
        header += f"{name:>12}"
    print(header + "   flags")

    ordered = result.frame.sort("composite", descending=True)
    for row in ordered.iter_rows(named=True):
        line = f"{row['asset']:<8}{row['group_key']:<12}{row['composite']:>10.1f}"
        for name in factor_names:
            line += f"{row[scores.factor_column(name)]:>12.1f}"
        flags = row["degenerate_flags"]
        print(line + ("   " + ", ".join(flags) if flags else ""))

    return 0


def cmd_plan(args) -> int:
    config = _config(args)
    as_of = _utc(args.as_of) if args.as_of else datetime.now(timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0
    )

    try:
        result = pipeline.run(
            as_of=as_of,
            config=config,
            mode="dry",
            data_root=Path(args.data_root),
            created_at=_utc(args.created_at) if args.created_at else None,
        )
    except pipeline.GateFailure as exc:
        # Fail closed: a gate failure is a reason to do nothing, loudly.
        print(f"GATE FAILED: {exc}", file=sys.stderr)
        print("no plan produced; the book is untouched", file=sys.stderr)
        return 2

    doc = result.document
    if args.json:
        sys.stdout.write(plan_mod.canonical_json(doc))
        return 0

    _render(doc, result)

    if args.out:
        path = Path(args.out)
        digest = plan_mod.write(path, doc)
        print(f"\nwritten to {path}  digest {digest[:16]}")
    return 0


def _render(doc: dict, result: pipeline.PlanResult) -> None:
    # Disclosures first. A number read before its caveats has already misled.
    if doc["warnings"]:
        print("DISCLOSURES (read before any number below)")
        for w in doc["warnings"]:
            print(f"  ! [{w['kind']}] {w['message']}")
        print()

    p = doc["provenance"]
    print(f"plan     {doc['plan_id']}")
    print(f"as of    {doc['as_of']}   mode={doc['mode']}   status={doc['status'].upper()}")
    print(
        f"built by {p['constructor']} (requested {p['constructor_requested']})  "
        f"universe {p['universe_size']}  inputs {p['inputs_hash']}"
    )
    print(
        f"NAV      {doc['nav']['total']} {doc['quote_currency']}   "
        f"cash {doc['nav']['cash']}   gross {doc['nav']['gross_exposure']}   "
        f"net {doc['nav']['net_exposure']}"
    )

    print("\nRISK")
    for c in doc["risk_report"]["checks"]:
        mark = "ok  " if c["passed"] else "FAIL"
        print(f"  [{mark}] {c['name']:<22} {c['value']:>12} / {c['limit']}")
    if doc["risk_report"]["rejected_reason"]:
        print(f"  REJECTED: {doc['risk_report']['rejected_reason']}")

    print("\nTARGETS")
    if not doc["targets"]:
        print("  (flat)")
    for t in doc["targets"]:
        print(f"  {t['asset']:<6} {t['direction']:<5} {t['weight']:>10}")

    print("\nORDERS")
    if not doc["orders"]:
        print("  (none)")
    for o in doc["orders"]:
        print(
            f"  {o['side']:<4} {o['asset']:<6} {o['qty']:>16}  {o['reason']:<9} "
            f"est {o['est_cost_bps']}bps"
        )

    ce = doc["cost_estimate"]
    print(f"\nCOST     {ce['total_quote']} {doc['quote_currency']}  ({ce['total_bps']} bps of NAV)")

    if result.skipped:
        print("\nSKIPPED")
        for s in result.skipped:
            print(f"  - {s}")
    if result.notes:
        print("\nNOTES")
        for n in result.notes:
            print(f"  - {n}")


def cmd_plan_verify(args) -> int:
    """The Phase 0 gate: the same decision computed twice is one plan."""
    config = _config(args)
    as_of = _utc(args.as_of) if args.as_of else datetime.now(timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0
    )

    digests, ids = [], []
    for i in range(args.runs):
        result = pipeline.run(
            as_of=as_of,
            config=config,
            mode="dry",
            data_root=Path(args.data_root),
            # Deliberately different wall-clock stamps: created_at is the one
            # field allowed to differ, and holding it constant would make this
            # check pass for the wrong reason.
            created_at=datetime(2026, 1, 1, tzinfo=timezone.utc) + timedelta(hours=i),
        )
        digests.append(plan_mod.digest(result.document))
        ids.append(result.document["plan_id"])

    same_digest = len(set(digests)) == 1
    same_id = len(set(ids)) == 1
    for i, (d, pid) in enumerate(zip(digests, ids)):
        print(f"run {i + 1}: digest {d[:16]}  plan_id {pid}")

    if same_digest and same_id:
        print(f"\nPASS: {args.runs} runs, identical decision content and plan id")
        return 0
    print("\nFAIL: runs diverged - the decision path is not deterministic", file=sys.stderr)
    return 1


# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="ai-trader", description="Crypto portfolio planner")
    p.add_argument("--config", help=f"config TOML (default {DEFAULT_CONFIG_PATH})")
    p.add_argument("--data-root", default=str(store.DEFAULT_ROOT), help="data directory")
    sub = p.add_subparsers(dest="group", required=True)

    data = sub.add_parser("data", help="market data").add_subparsers(dest="cmd", required=True)
    pull = data.add_parser("pull", help="fetch bars into the store")
    pull.add_argument("--days", type=int, default=400)
    pull.add_argument("--end", help="UTC end, exclusive (default today 00:00)")
    pull.set_defaults(func=cmd_data_pull)
    data.add_parser("inspect", help="what is in the store").set_defaults(func=cmd_data_inspect)
    data.add_parser(
        "verify", help="check ts_utc is the bar OPEN"
    ).set_defaults(func=cmd_data_verify)

    uni = sub.add_parser("universe", help="point-in-time membership").add_subparsers(
        dest="cmd", required=True
    )
    rec = uni.add_parser("record", help="append today's snapshot")
    rec.add_argument("--as-of")
    rec.add_argument("--overwrite", action="store_true", help="only to fix a recording error")
    rec.set_defaults(func=cmd_universe_record)
    uni.add_parser("list", help="recorded snapshots").set_defaults(func=cmd_universe_list)

    book = sub.add_parser("book", help="portfolio state").add_subparsers(
        dest="cmd", required=True
    )
    binit = book.add_parser("init", help="seed the Phase 0 book with cash")
    binit.add_argument("--cash", required=True)
    binit.add_argument("--force", action="store_true")
    binit.set_defaults(func=cmd_book_init)
    book.add_parser("show", help="current holdings").set_defaults(func=cmd_book_show)

    sc = sub.add_parser("scores", help="the scored cross-section (a lens, not a decision)")
    sc.add_argument("--as-of", help="decision timestamp, ISO 8601 (default: today UTC)")
    sc.add_argument(
        "--by-cluster",
        action="store_true",
        help="rank within configured clusters instead of across the whole universe",
    )
    sc.set_defaults(func=cmd_scores)

    pl_ = sub.add_parser("plan", help="produce a plan (steps 1-8, no side effects)")
    pl_sub = pl_.add_subparsers(dest="cmd")
    pl_.add_argument("--as-of", help="UTC decision timestamp (default today 00:00)")
    pl_.add_argument("--out", help="write the plan JSON here")
    pl_.add_argument("--json", action="store_true", help="print canonical JSON only")
    pl_.add_argument("--created-at", help="override the wall-clock stamp")
    pl_.set_defaults(func=cmd_plan)

    ver = pl_sub.add_parser("verify", help="the Phase 0 gate: determinism across runs")
    ver.add_argument("--as-of")
    ver.add_argument("--runs", type=int, default=2)
    ver.set_defaults(func=cmd_plan_verify)

    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
