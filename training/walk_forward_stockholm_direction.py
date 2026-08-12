"""Orchestrate purged expanding fits of the separate direction model.

Feature construction, labels, model inference, stateful exposure decisions,
and performance metrics remain in Rust. Python only derives session-aligned
fold boundaries, invokes fitting, and invokes each Rust replay.
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
        sessions = [
            date.fromisoformat(json.loads(line)["date"])
            for line in source
            if line.strip()
        ]
    if manifest.get("kind") != "stockholm_direction_training_manifest":
        raise ValueError("first row is not a Stockholm direction matrix manifest")
    if sessions != sorted(set(sessions)):
        raise ValueError("direction matrix sessions are not strictly increasing")
    return manifest, sessions


def build_folds(
    sessions: list[date], start: date, end: date, count: int, horizon: int
) -> list[Fold]:
    if count <= 0 or horizon <= 0:
        raise ValueError("fold count and horizon must be positive")
    test = [session for session in sessions if start <= session <= end]
    if len(test) < count * horizon * 2:
        raise ValueError("direction test interval is too short for independent folds")
    first = sessions.index(test[0])
    block = (len(test) // count // horizon) * horizon
    if block == 0:
        raise ValueError("direction fold blocks contain no complete holding period")
    folds = []
    for index in range(count):
        test_offset = index * block
        test_end_offset = len(test) - 1 if index == count - 1 else (index + 1) * block - 1
        global_start = first + test_offset
        cutoff_index = global_start - horizon - 1
        if cutoff_index < 0:
            raise ValueError("not enough pre-test direction history for the purge")
        folds.append(
            Fold(
                number=index + 1,
                trained_through=sessions[cutoff_index],
                test_start=test[test_offset],
                test_end=test[test_end_offset],
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
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--start", type=date.fromisoformat, required=True)
    parser.add_argument("--end", type=date.fromisoformat, required=True)
    parser.add_argument("--folds", type=int, default=6)
    parser.add_argument("--clip-quantile", type=float, default=0.01)
    parser.add_argument("--objective", choices=("l2", "l1", "huber"), default="l2")
    parser.add_argument(
        "--reward", choices=("absolute_return", "direction_sign"), default="absolute_return"
    )
    parser.add_argument("--max-gross", type=float, default=1.0)
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
        "kind": "stockholm_direction_purged_expanding_walk_forward_plan",
        "matrix": str(args.matrix),
        "horizon_sessions": horizon,
        "clip_quantile": args.clip_quantile,
        "objective": args.objective,
        "reward": args.reward,
        "max_gross": args.max_gross,
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
    trainer = Path(__file__).with_name("train_stockholm_direction.py")
    for fold in folds:
        model = args.out / f"model-{fold.number}.json"
        report = args.out / f"fold-{fold.number}.json"
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
                "--clip-quantile",
                str(args.clip_quantile),
                "--objective",
                args.objective,
                "--reward",
                args.reward,
            ]
        )
        run(
            [
                str(args.binary),
                "direction-backtest",
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
                "--max-gross",
                str(args.max_gross),
            ]
        )
    summary_command = [
        str(args.binary),
        "summarize-direction",
        "--out",
        str(args.out / "report.json"),
    ]
    for fold in folds:
        summary_command.extend(["--fold", str(args.out / f"fold-{fold.number}.json")])
    run(summary_command)


if __name__ == "__main__":
    main()
