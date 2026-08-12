"""Orchestrate purged expanding Stockholm model fits and Rust replays.

This module owns no feature, label, prediction, portfolio, cost, or metric
calculation. It derives session-aligned fold boundaries from the Rust matrix,
invokes the fitting-only Python entry point, invokes the Rust replay for each
strictly-forward test block, and asks the existing summary script to stitch the
Rust reports.
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
    parser.add_argument("--clip-quantile", type=float, default=0.005)
    parser.add_argument("--stress-multiple", type=float, default=2.0)
    parser.add_argument(
        "--direction-overlay",
        action="store_true",
        help="enable the fixed Rust OMX direction baseline in every replay",
    )
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    if args.out.exists() and any(args.out.iterdir()) and not args.force:
        parser.error(f"{args.out} is not empty; use --force or a new experiment directory")
    if not args.binary.is_file():
        parser.error(f"Rust binary does not exist: {args.binary}")
    manifest, sessions = matrix_dates(args.matrix)
    horizon = int(manifest["horizon_sessions"])
    folds = build_folds(sessions, args.start, args.end, args.folds, horizon)
    args.out.mkdir(parents=True, exist_ok=True)
    plan = {
        "kind": "stockholm_purged_expanding_walk_forward_plan",
        "matrix": str(args.matrix),
        "horizon_sessions": horizon,
        "reward": args.reward,
        "model_family": args.model_family,
        "objective": args.objective,
        "ensemble_seeds": args.seeds,
        "clip_quantile": args.clip_quantile,
        "ridge_lambda": (
            args.ridge_lambda if args.model_family in ("ridge", "hybrid") else None
        ),
        "direction_overlay": args.direction_overlay,
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
    summarizer = Path(__file__).with_name("summarize_stockholm.py")
    for fold in folds:
        model = args.out / f"model-{fold.number}.json"
        run(
            [
                sys.executable,
                str(trainer),
                "--matrix",
                str(args.matrix),
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
            ]
        )
        for scenario, multiple in (("base", 1.0), ("stress", args.stress_multiple)):
            report = args.out / f"fold-{fold.number}-{scenario}.json"
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
                    "--cost-multiple",
                    str(multiple),
                ]
                + (["--direction-overlay"] if args.direction_overlay else [])
            )
    for scenario in ("base", "stress"):
        command = [
            sys.executable,
            str(summarizer),
            "--out",
            str(args.out / f"report-{scenario}.json"),
        ]
        for fold in folds:
            command.extend(
                ["--fold", str(args.out / f"fold-{fold.number}-{scenario}.json")]
            )
        run(command)


if __name__ == "__main__":
    main()
