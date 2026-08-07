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
from . import backtest as backtest_mod
from . import features, gate, ic, pipeline, report, scores, state, store, universe
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


def _pull_assets(args, config, root: Path) -> tuple[list[str], str]:
    """Which assets a pull should refresh, and where that list came from.

    The default is the store, not `config.universe`. The configured list is the
    Phase 0 path — three names — while what actually gets traded is the ranked
    universe `universe rank` derives from the store. Pulling only the configured
    three left the other ~670 assets frozen at whatever date the archive load
    stopped, and `by_liquidity` then reads a stale tail as delisted. A daily
    cycle that silently stops refreshing the universe it trades is worse than
    one that fails, so the default now follows the store.
    """
    if args.assets:
        return [a.strip().upper() for a in args.assets.split(",") if a.strip()], "--assets"
    if args.universe == "config":
        return list(config.universe), "config.universe"
    known = sorted(
        p.name.removeprefix("asset=") for p in (root / "bars").glob("asset=*")
    )
    if known:
        return known, "the store"
    return list(config.universe), "config.universe (store is empty)"


def cmd_data_pull(args) -> int:
    config = _config(args)
    root = Path(args.data_root)
    end = _utc(args.end) if args.end else datetime.now(timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0
    )
    start = end - timedelta(days=args.days)
    assets, origin = _pull_assets(args, config, root)

    # Both intervals the decision path reads. `pipeline.run` featurizes hourly
    # bars alongside the daily frame, so refreshing only the daily one leaves
    # the intraday features running days behind the prices they are paired with.
    intervals = [config.interval_s]
    if not args.daily_only and 3600 != config.interval_s:
        intervals.append(3600)

    print(
        f"pull {len(assets)} asset(s) from {origin}, "
        f"{start.date()}..{end.date()}, interval(s) {intervals}"
    )
    src = BinancePublic()
    total = 0
    written = 0
    try:
        import polars as pl

        for interval_s in intervals:
            frames = []
            missing = 0
            for asset in assets:
                try:
                    df = src.fetch_bars(asset, interval_s=interval_s, start=start, end=end)
                except Exception as exc:  # one delisted symbol must not end the pull
                    missing += 1
                    if args.verbose:
                        print(f"  {asset} @{interval_s}s: {exc}")
                    continue
                if df.height:
                    frames.append(df)
                    total += df.height
                else:
                    missing += 1
                if args.verbose:
                    print(f"  {asset} @{interval_s}s: {df.height} bars")
            if not frames:
                print(f"  interval {interval_s}s: nothing fetched", file=sys.stderr)
                continue
            combined = pl.concat(frames, how="vertical_relaxed")
            issues = store.write(combined, root=root, source=src.name)
            written += combined.height
            print(
                f"  interval {interval_s}s: wrote {combined.height} bars "
                f"for {len(frames)} asset(s); {missing} returned nothing"
            )
            for i in issues:
                print(f"    [{i.severity.value}] {i.code} x{i.count}: {i.detail}")
    finally:
        src.close()

    if not written:
        print("no bars fetched", file=sys.stderr)
        return 1
    print(f"\nwrote {written} bars from {src.name}")
    return 0


def cmd_model_train(args) -> int:
    """Train the ranker and write an artefact.

    The cutoff is required rather than defaulted to "today". A default would
    quietly produce a model that has seen every date a backtest then scores,
    which is the one mistake this whole module exists to prevent.
    """
    from datetime import date as _date

    from . import features as _feat, model as _model

    config = _config(args)
    root = Path(args.data_root)
    start = _date.fromisoformat(args.start)
    through = _date.fromisoformat(args.through)
    end = _date.fromisoformat(args.end) if args.end else through

    print(f"building training frame {start} .. {end} ...", flush=True)
    frame, names = _model.build_training_frame(
        config=config, root=root, start=start, end=end
    )
    print(f"  {frame.height:,} rows, {frame['date'].n_unique():,} dates, "
          f"{len(names)} features")

    print(f"training through {through} ...", flush=True)
    artefact = _model.train(
        frame, features=names, target="y", trained_through=through,
        feature_set_version=_feat.FEATURE_SET_VERSION,
    )
    out = _model.save(artefact, Path(args.out))
    print(f"\nwrote {out}")
    print(f"  trained on {artefact.n_rows:,} rows over {artefact.n_dates:,} dates")
    print(f"  cutoff {artefact.trained_through} - it will REFUSE to score on or "
          f"before that date")
    print(f"  set `model_path = \"{out}\"` in config to use it")
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


def cmd_universe_rank(args) -> int:
    """Record point-in-time snapshots from the bar store, by liquidity rank.

    A range is allowed because a backtest needs a snapshot per decision date and
    none of them may be backfilled *as a list* — but each one is computed from
    the rule using only bars that closed by its own date, which is what makes
    reconstructing it legitimate (see `universe.by_liquidity`).

    Bluntly: this is only honest if the store contains delisted assets. Ranking
    over survivors reintroduces exactly the bias the module exists to prevent.
    """
    config = _config(args)
    root = Path(args.data_root)
    start = _utc(args.start)
    end = _utc(args.end) if args.end else start

    bars = store.read(root=root, interval_s=config.interval_s)
    if bars.is_empty():
        print("no bars in the store; nothing to rank", file=sys.stderr)
        return 2

    # Step at the decision cadence, not the bar interval: a snapshot is only
    # needed where a decision is taken, and recording daily ones for a weekly
    # rebalance writes thousands of files nothing reads.
    #
    # `--step-days` overrides that, and exists for one reason: a cadence SWEEP
    # needs snapshots on the finest grid it will test, because every coarser
    # cadence's dates are a subset of the finer one's. Recording per-cadence
    # instead would give each arm of the sweep a different universe, which is
    # the one thing a one-axis sweep must not do.
    step = (
        timedelta(days=args.step_days)
        if getattr(args, "step_days", None)
        else timedelta(seconds=config.interval_s * max(1, config.rebalance_every))
    )
    written = skipped = 0
    last_written: datetime | None = None
    day = start
    while day <= end:
        members = universe.by_liquidity(
            bars,
            as_of=day,
            top_n=args.top,
            lookback_days=args.lookback,
            min_history_bars=config.min_history_bars,
            min_turnover=float(config.min_dollar_volume),
        )
        if not members:
            skipped += 1
            day += step
            continue
        try:
            universe.record(
                members,
                as_of=day,
                source="by_liquidity",
                root=root,
                overwrite=args.overwrite,
            )
            written += 1
            last_written = day
        except FileExistsError:
            skipped += 1
        day += step

    print(f"recorded {written} snapshot(s), skipped {skipped}")
    if last_written is not None:
        last = universe.load(last_written, root=root)
        eligible = [m.asset for m in last if m.eligible]
        dead = [m for m in last if "delisted" in m.reason]
        print(f"{last_written.date()}: {len(eligible)} eligible of {len(last)} considered")
        print(f"  top: {', '.join(eligible[:12])}")
        print(f"  carried as delisted/halted: {len(dead)}")
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


def cmd_report(args) -> int:
    """Render the research view from a record the CLI produced.

    A lens, per design spec 8.1: this computes nothing. If it disappeared the
    project would lose a convenience and no evidence.
    """
    record = Path(args.record)
    if not record.exists():
        print(f"no record at {record}", file=sys.stderr)
        return 2
    out = report.write(record, Path(args.out))
    print(f"wrote {out} ({out.stat().st_size // 1024}kb, self-contained)")
    return 0


def cmd_ic(args) -> int:
    """Information coefficient of a score against subsequent returns.

    A lens on the signal rather than the portfolio (design spec 7.5): one
    observation per asset per period instead of one per rebalance, which is
    roughly 30x the evidence for the same calendar time.
    """
    results = ic.measure(
        config=_config(args),
        start=_utc(args.start),
        end=_utc(args.end),
        data_root=Path(args.data_root),
        score_column=args.score,
        horizons_days=tuple(int(h) for h in args.horizons),
    )
    print(ic.format_results(results, score_column=args.score))
    return 0


def cmd_gate(args) -> int:
    """Run the Phase 1 gate and print the verdict, whatever it is."""
    result = gate.run(
        config=_config(args),
        start=_utc(args.start),
        end=_utc(args.end),
        data_root=Path(args.data_root),
        initial_cash=Decimal(args.cash),
    )
    print(gate.format_result(result))
    # Non-zero on a failed gate: a gate that cannot fail a script is decoration.
    return 0 if result.passed else 1


def cmd_backtest(args) -> int:
    """Replay the decision path over history.

    Not a second engine: this calls the same `pipeline.run` the live planner
    calls, fills its orders against the bar that opened at each decision
    timestamp, and steps forward (design spec §2.3).
    """
    config = _config(args)
    start, end = _utc(args.start), _utc(args.end)

    multiples = tuple(Decimal(m) for m in args.slippage)
    runs = backtest_mod.sensitivity(
        config=config,
        start=start,
        end=end,
        data_root=Path(args.data_root),
        initial_cash=Decimal(args.cash),
        multiples=multiples,
    )

    baseline = runs[0]
    if not baseline.steps:
        print("no rebalance produced a plan; nothing to report", file=sys.stderr)
        for note in baseline.disclosures:
            print(f"  ! {note}", file=sys.stderr)
        return 2

    # Disclosures first. Everything below is read through them.
    print("DISCLOSURES (read before any number below)")
    for note in baseline.disclosures:
        print(f"  ! {note}")
    print()

    print(f"window   {start.date()} .. {end.date()}   interval {config.interval_s}s")
    print(f"ruleset  {config.ruleset_version}   signal {config.signal}")
    print(f"features {features.FEATURE_SET_VERSION}   constructor {config.constructor}")

    print(f"\n{'slippage':<10}{'n':>6}{'return':>10}{'CAGR':>9}{'vol':>8}"
          f"{'Sharpe':>8}{'maxDD':>9}{'turnover':>10}{'cost bps':>10}")
    for run in runs:
        m = run.metrics
        print(
            f"{str(run.slippage_multiple) + 'x':<10}{m.n:>6}"
            f"{float(m.total_return) * 100:>9.2f}%{m.cagr * 100:>8.2f}%"
            f"{m.volatility * 100:>7.1f}%{m.sharpe:>8.2f}"
            f"{float(m.max_drawdown) * 100:>8.2f}%"
            f"{float(m.turnover_per_rebalance) * 100:>9.2f}%{float(m.cost_drag_bps):>10.1f}"
        )

    if len(runs) > 1:
        survived = runs[-1].metrics.total_return > 0 and baseline.metrics.total_return > 0
        print(
            f"\n2x slippage is an error bar, not a parameter (§2.2). "
            f"{'Result survives it.' if survived else 'Result does not survive it.'}"
        )

    if baseline.metrics.insufficient_sample:
        print(
            f"\nRead none of the above as evidence of edge: {baseline.metrics.n} rebalances. "
            "§7.5 argues no paper or backtest run of a plausible length establishes a modest one."
        )

    if args.nav:
        path = Path(args.nav)
        rows = "\n".join(f"{ts.isoformat()},{nav}" for ts, nav in baseline.nav_series)
        path.write_bytes(("ts_utc,nav\n" + rows + "\n").encode("utf-8"))
        print(f"\nNAV series written to {path}")

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
    pull.add_argument(
        "--universe",
        choices=["store", "config"],
        default="store",
        help="which assets to refresh (default: every asset already in the store, "
        "because that is what `universe rank` ranks over)",
    )
    pull.add_argument("--assets", help="explicit comma-separated list, overrides --universe")
    pull.add_argument("--daily-only", action="store_true",
                      help="skip the hourly bars the intraday features read")
    pull.add_argument("-v", "--verbose", action="store_true", help="one line per asset")
    pull.set_defaults(func=cmd_data_pull)
    data.add_parser("inspect", help="what is in the store").set_defaults(func=cmd_data_inspect)
    data.add_parser(
        "verify", help="check ts_utc is the bar OPEN"
    ).set_defaults(func=cmd_data_verify)

    mdl = sub.add_parser("model", help="the learned ranker").add_subparsers(
        dest="cmd", required=True
    )
    mtr = mdl.add_parser("train", help="fit the ranker and write an artefact")
    mtr.add_argument("--start", default="2019-10-01", help="first decision date")
    mtr.add_argument("--through", required=True,
                     help="TRAINING CUTOFF (UTC date). The artefact refuses to "
                          "score on or before this, so set it before any period "
                          "you intend to evaluate on.")
    mtr.add_argument("--end", help="last decision date to build (default: --through)")
    mtr.add_argument("--out", default="data/models/ranker.pkl")
    mtr.set_defaults(func=cmd_model_train)

    uni = sub.add_parser("universe", help="point-in-time membership").add_subparsers(
        dest="cmd", required=True
    )
    rec = uni.add_parser("record", help="append today's snapshot")
    rec.add_argument("--as-of")
    rec.add_argument("--overwrite", action="store_true", help="only to fix a recording error")
    rec.set_defaults(func=cmd_universe_record)
    rank = uni.add_parser("rank", help="record snapshots by liquidity rank, from the store")
    rank.add_argument("--start", required=True, help="first snapshot date, ISO 8601")
    rank.add_argument("--end", help="last snapshot date (default: same as --start)")
    rank.add_argument("--top", type=int, default=30, help="how many are eligible")
    rank.add_argument("--lookback", type=int, default=30, help="turnover window in days")
    rank.add_argument(
        "--step-days",
        type=int,
        help="snapshot cadence in days (default: the config rebalance cadence). "
        "Use the finest grid a sweep will test.",
    )
    rank.add_argument("--overwrite", action="store_true", help="correct a recording error")
    rank.set_defaults(func=cmd_universe_rank)

    uni.add_parser("list", help="recorded snapshots").set_defaults(func=cmd_universe_list)

    book = sub.add_parser("book", help="portfolio state").add_subparsers(
        dest="cmd", required=True
    )
    binit = book.add_parser("init", help="seed the Phase 0 book with cash")
    binit.add_argument("--cash", required=True)
    binit.add_argument("--force", action="store_true")
    binit.set_defaults(func=cmd_book_init)
    book.add_parser("show", help="current holdings").set_defaults(func=cmd_book_show)

    bt = sub.add_parser("backtest", help="replay the decision path over history")
    bt.add_argument("--start", required=True, help="first decision timestamp, ISO 8601")
    bt.add_argument("--end", required=True, help="last decision timestamp, inclusive")
    bt.add_argument("--cash", default="100000", help="starting cash")
    bt.add_argument(
        "--slippage",
        nargs="+",
        default=["1", "2"],
        help="slippage multiples to run as error bars (default: 1 2)",
    )
    bt.add_argument("--nav", help="write the NAV series to this CSV")
    bt.set_defaults(func=cmd_backtest)

    rep = sub.add_parser("report", help="render the research view (a lens, computes nothing)")
    rep.add_argument("--record", required=True, help="JSON record produced by a run")
    rep.add_argument("--out", required=True, help="HTML file to write")
    rep.set_defaults(func=cmd_report)

    icp = sub.add_parser("ic", help="information coefficient of a score (7.5)")
    icp.add_argument("--start", required=True, help="first decision timestamp, ISO 8601")
    icp.add_argument("--end", required=True, help="last decision timestamp, inclusive")
    icp.add_argument("--score", default="ret_30_skip_7", help="feature column to rank on")
    icp.add_argument("--horizons", nargs="+", default=["7", "14", "30"], help="forward days")
    icp.set_defaults(func=cmd_ic)

    gt = sub.add_parser("gate", help="run the Phase 1 gate (design spec 9)")
    gt.add_argument("--start", required=True, help="first decision timestamp, ISO 8601")
    gt.add_argument("--end", required=True, help="last decision timestamp, inclusive")
    gt.add_argument("--cash", default="100000", help="starting cash")
    gt.set_defaults(func=cmd_gate)

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
