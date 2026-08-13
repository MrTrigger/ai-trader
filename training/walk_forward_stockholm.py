"""Orchestrate purged expanding Stockholm model fits and Rust replays.

This module owns no feature, label, prediction, portfolio, cost, or metric
calculation. It derives session-aligned fold boundaries from the Rust matrix,
invokes the fitting-only Python entry point, invokes the Rust replay for each
strictly-forward test block, and asks the existing summary script to stitch the
Rust reports.

This whole harness is a research/development tool, not a promotable
configuration path (see README.md). Trained direction is retired: every
tested variant lost to controls, and Rust refuses --market-forecast-matrix
composition without an explicit diagnostic opt-in, which this module supplies
automatically when the caller asks for it.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import asdict, dataclass
from datetime import date
from pathlib import Path


@dataclass(frozen=True)
class Fold:
    number: int
    trained_through: date
    test_start: date
    test_end: date
    purged_decision_sessions: int


def matrix_dates(path: Path) -> tuple[dict, list[date]]:
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
        dates = {
            date.fromisoformat(json.loads(line)["date"])
            for line in source
            if line.strip()
        }
    if manifest.get("kind") != "stockholm_training_manifest":
        raise ValueError("first row is not a Stockholm Rust matrix manifest")
    return manifest, sorted(dates)


def matrix_manifest(path: Path) -> dict:
    with path.open(encoding="utf-8") as source:
        manifest = json.loads(next(source))
    if manifest.get("kind") != "stockholm_training_manifest":
        raise ValueError("first row is not a Stockholm Rust matrix manifest")
    return manifest


def build_folds(
    sessions: list[date],
    start: date,
    end: date,
    count: int,
    horizon: int,
) -> list[Fold]:
    if count <= 0 or horizon <= 0:
        raise ValueError("fold count and horizon must be positive")
    test = [session for session in sessions if start <= session <= end]
    if len(test) < count * horizon * 2:
        raise ValueError("test interval is too short for independent folds")
    first = sessions.index(test[0])
    # Boundaries are multiples of the holding cadence. That keeps each fold's
    # first decision on the same global rebalance grid and prevents a fold
    # reset from inventing extra overlapping observations.
    block = (len(test) // count // horizon) * horizon
    if block == 0:
        raise ValueError("fold blocks contain no complete holding period")
    folds = []
    for index in range(count):
        test_offset = index * block
        test_end_offset = len(test) - 1 if index == count - 1 else (index + 1) * block - 1
        test_start = test[test_offset]
        test_end = test[test_end_offset]
        global_start = first + test_offset
        # A decision label enters on d+1 and exits on d+1+H. The newest train
        # decision must therefore be at most test_start-H-1 so its exit is
        # strictly before the first test entry.
        cutoff_index = global_start - horizon - 1
        if cutoff_index < 0:
            raise ValueError("not enough pre-test history for the label purge")
        cutoff = sessions[cutoff_index]
        folds.append(
            Fold(
                number=index + 1,
                trained_through=cutoff,
                test_start=test_start,
                test_end=test_end,
                purged_decision_sessions=horizon,
            )
        )
    return folds


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument(
        "--training-matrix",
        type=Path,
        help=(
            "optional shorter-horizon Rust matrix used only for fitting; Rust "
            "explicitly aggregates its forecast to the holding matrix horizon"
        ),
    )
    parser.add_argument("--benchmark", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--start", type=date.fromisoformat, required=True)
    parser.add_argument("--end", type=date.fromisoformat, required=True)
    parser.add_argument("--folds", type=int, default=5)
    parser.add_argument(
        "--reward",
        choices=(
            "absolute_return",
            "return_per_risk",
            "relative_return",
            "relative_return_per_risk",
            "relative_rank",
        ),
        required=True,
    )
    parser.add_argument("--objective", choices=("l2", "l1", "huber"), required=True)
    parser.add_argument(
        "--model-family",
        choices=("lightgbm", "ridge", "hybrid"),
        default="lightgbm",
    )
    parser.add_argument("--seeds", type=int, default=1)
    parser.add_argument("--ridge-lambda", type=float, default=25.0)
    parser.add_argument("--calibration-sessions", type=int, default=0)
    parser.add_argument(
        "--training-window-sessions",
        type=int,
        default=0,
        help="zero uses an expanding prefix; otherwise fit only this many trailing sessions",
    )
    parser.add_argument("--clip-quantile", type=float, default=0.005)
    parser.add_argument("--stress-multiple", type=float, default=2.0)
    parser.add_argument(
        "--all-rebalance-phases",
        action="store_true",
        help="replay and equal-weight every holding-cadence offset in Rust",
    )
    parser.add_argument(
        "--direction-overlay",
        action="store_true",
        help="enable the fixed Rust OMX direction baseline in every replay",
    )
    parser.add_argument(
        "--market-forecast-matrix",
        type=Path,
        help=(
            "Rust direction matrix used to train a causal OMXSGI return model "
            "for composing relative stock forecasts"
        ),
    )
    parser.add_argument(
        "--prediction-composition",
        choices=("direct", "cross-sectional-residual-plus-market"),
        default="direct",
    )
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    relative_reward = args.reward in (
        "relative_return",
        "relative_return_per_risk",
        "relative_rank",
    )
    if relative_reward and not args.direction_overlay:
        parser.error("relative rewards require --direction-overlay")
    if relative_reward and args.market_forecast_matrix is None:
        parser.error("relative rewards require --market-forecast-matrix")
    if not relative_reward and args.market_forecast_matrix is not None:
        parser.error("absolute rewards must not use --market-forecast-matrix")
    if args.out.exists() and any(args.out.iterdir()) and not args.force:
        parser.error(f"{args.out} is not empty; use --force or a new experiment directory")
    if not args.binary.is_file():
        parser.error(f"Rust binary does not exist: {args.binary}")
    if args.training_window_sessions < 0:
        parser.error("--training-window-sessions must be non-negative")
    manifest, sessions = matrix_dates(args.matrix)
    horizon = int(manifest["horizon_sessions"])
    training_matrix = args.training_matrix or args.matrix
    training_manifest = matrix_manifest(training_matrix)
    training_horizon = int(training_manifest["horizon_sessions"])
    aggregate_short_horizon = training_matrix != args.matrix
    if aggregate_short_horizon:
        contract_fields = ("features", "feature_set_version", "survivorship_status")
        if any(training_manifest.get(field) != manifest.get(field) for field in contract_fields):
            raise ValueError("training and holding matrix feature contracts differ")
        if training_horizon >= horizon or horizon % training_horizon:
            raise ValueError(
                "training horizon must be a proper divisor of holding horizon"
            )
    folds = build_folds(sessions, args.start, args.end, args.folds, horizon)
    args.out.mkdir(parents=True, exist_ok=True)
    plan = {
        "kind": "stockholm_purged_expanding_walk_forward_plan",
        "matrix": str(args.matrix),
        "training_matrix": str(training_matrix),
        "horizon_sessions": horizon,
        "training_horizon_sessions": training_horizon,
        "prediction_horizon_scale": horizon / training_horizon,
        "reward": args.reward,
        "model_family": args.model_family,
        "objective": args.objective,
        "ensemble_seeds": args.seeds,
        "clip_quantile": args.clip_quantile,
        "ridge_lambda": (
            args.ridge_lambda if args.model_family in ("ridge", "hybrid") else None
        ),
        "calibration_sessions": args.calibration_sessions,
        "training_window_sessions": args.training_window_sessions,
        "all_rebalance_phases": args.all_rebalance_phases,
        "direction_overlay": args.direction_overlay,
        "market_forecast_matrix": (
            str(args.market_forecast_matrix) if args.market_forecast_matrix else None
        ),
        "prediction_composition": args.prediction_composition,
        "folds": [
            {
                **asdict(fold),
                "trained_through": fold.trained_through.isoformat(),
                "test_start": fold.test_start.isoformat(),
                "test_end": fold.test_end.isoformat(),
            }
            for fold in folds
        ],
    }
    (args.out / "fold-plan.json").write_text(json.dumps(plan, indent=2) + "\n")
    trainer = Path(__file__).with_name("train_stockholm.py")
    direction_trainer = Path(__file__).with_name("train_stockholm_direction.py")
    summarizer = Path(__file__).with_name("summarize_stockholm.py")
    for fold in folds:
        model = args.out / f"model-{fold.number}.json"
        market_model = args.out / f"market-model-{fold.number}.json"
        training_from = None
        if args.training_window_sessions:
            cutoff_index = sessions.index(fold.trained_through)
            from_index = max(0, cutoff_index - args.training_window_sessions + 1)
            training_from = sessions[from_index]
        run(
            [
                sys.executable,
                str(trainer),
                "--matrix",
                str(training_matrix),
                "--through",
                fold.trained_through.isoformat(),
                "--out",
                str(model),
                "--reward",
                args.reward,
                "--model-family",
                args.model_family,
                "--objective",
                args.objective,
                "--seeds",
                str(args.seeds),
                "--clip-quantile",
                str(args.clip_quantile),
                "--ridge-lambda",
                str(args.ridge_lambda),
                "--calibration-sessions",
                str(args.calibration_sessions),
            ]
            + (["--from", training_from.isoformat()] if training_from else [])
        )
        if args.market_forecast_matrix is not None:
            run(
                [
                    sys.executable,
                    str(direction_trainer),
                    "--matrix",
                    str(args.market_forecast_matrix),
                    "--through",
                    fold.trained_through.isoformat(),
                    "--out",
                    str(market_model),
                ]
            )
        for scenario, multiple in (("base", 1.0), ("stress", args.stress_multiple)):
            phase_reports = []
            offsets = range(horizon) if args.all_rebalance_phases else (0,)
            for offset in offsets:
                report = (
                    args.out / f"fold-{fold.number}-phase-{offset}-{scenario}.json"
                    if args.all_rebalance_phases
                    else args.out / f"fold-{fold.number}-{scenario}.json"
                )
                phase_reports.append(report)
                run(
                    [
                        str(args.binary),
                        "backtest",
                        "--matrix",
                        str(args.matrix),
                        "--model",
                        str(model),
                        "--start",
                        fold.test_start.isoformat(),
                        "--end",
                        fold.test_end.isoformat(),
                        "--out",
                        str(report),
                        "--benchmark",
                        str(args.benchmark),
                        "--cadence-sessions",
                        str(horizon),
                        "--rebalance-offset-sessions",
                        str(offset),
                        "--cost-multiple",
                        str(multiple),
                        "--prediction-composition",
                        args.prediction_composition,
                    ]
                    + (["--direction-overlay"] if args.direction_overlay else [])
                    + (
                        ["--aggregate-short-horizon-forecast"]
                        if aggregate_short_horizon
                        else []
                    )
                    + (
                        [
                            "--market-forecast-matrix",
                            str(args.market_forecast_matrix),
                            "--market-forecast-model",
                            str(market_model),
                            # Trained direction is retired from every
                            # promotable configuration; Rust refuses this
                            # composition without an explicit diagnostic
                            # opt-in. This whole harness is a research tool
                            # (see README), so it asks for it automatically.
                            "--trained-direction-diagnostic",
                        ]
                        if args.market_forecast_matrix is not None
                        else []
                    )
                )
            if args.all_rebalance_phases:
                command = [
                    str(args.binary),
                    "summarize-rebalance-phases",
                    "--out",
                    str(args.out / f"fold-{fold.number}-{scenario}.json"),
                ]
                for report in phase_reports:
                    command.extend(["--phase", str(report)])
                run(command)
    for scenario in ("base", "stress"):
        command = (
            [
                str(args.binary),
                "summarize-rebalance-phase-folds",
                "--out",
                str(args.out / f"report-{scenario}.json"),
            ]
            if args.all_rebalance_phases
            else [
                sys.executable,
                str(summarizer),
                "--out",
                str(args.out / f"report-{scenario}.json"),
            ]
        )
        for fold in folds:
            command.extend(
                ["--fold", str(args.out / f"fold-{fold.number}-{scenario}.json")]
            )
        run(command)


if __name__ == "__main__":
    main()
