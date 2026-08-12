"""Aggregate completed Rust Stockholm fold reports.

The Rust replay has already selected positions and computed every period's P&L.
This script only stitches disjoint forward folds and reports evaluation metrics.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

import numpy as np


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fold", type=Path, action="append", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--min-sharpe", type=float, default=2.0)
    args = parser.parse_args()

    folds = [json.loads(path.read_text()) for path in args.fold]
    specifications = {
        (
            fold.get("model_family", "legacy_unspecified"),
            fold.get("feature_set_version", "legacy_unspecified"),
            fold.get("reward", "absolute_return"),
            fold.get("objective", "l1"),
            fold.get("ensemble_seeds", 1),
        )
        for fold in folds
    }
    if len(specifications) != 1:
        raise ValueError("fold model specifications differ")
    model_family, feature_set_version, reward, objective, ensemble_seeds = specifications.pop()
    diagnostics = [fold.get("diagnostics") for fold in folds]
    if any(item is None for item in diagnostics):
        raise ValueError("every fold must contain Rust prediction diagnostics")
    borrow_diagnostics = [fold.get("borrow_diagnostics", {}) for fold in folds]
    returns = np.asarray(
        [step["period_return"] for fold in folds for step in fold["steps"]],
        dtype=np.float64,
    )
    benchmark_metadata = [fold.get("benchmark") for fold in folds]
    if any(item is None for item in benchmark_metadata):
        raise ValueError("every fold must contain an aligned benchmark comparison")
    benchmark_symbols = {item["symbol"] for item in benchmark_metadata}
    if len(benchmark_symbols) != 1:
        raise ValueError("fold benchmark symbols differ")
    benchmark_returns = np.asarray(
        [
            step["benchmark_period_return"]
            for fold in folds
            for step in fold["steps"]
        ],
        dtype=np.float64,
    )
    if len(benchmark_returns) != len(returns) or not np.isfinite(benchmark_returns).all():
        raise ValueError("benchmark returns are missing or not aligned")
    cadence = {fold["cadence_sessions"] for fold in folds}
    if len(cadence) != 1:
        raise ValueError("fold cadences differ")
    periods_per_year = 252.0 / cadence.pop()
    nav = np.cumprod(1.0 + returns)
    benchmark_nav = np.cumprod(1.0 + benchmark_returns)
    peaks = np.maximum.accumulate(np.concatenate(([1.0], nav)))
    drawdowns = np.concatenate(([1.0], nav)) / peaks - 1.0
    benchmark_peaks = np.maximum.accumulate(
        np.concatenate(([1.0], benchmark_nav))
    )
    benchmark_drawdowns = (
        np.concatenate(([1.0], benchmark_nav)) / benchmark_peaks - 1.0
    )
    mean = float(returns.mean()) if len(returns) else 0.0
    vol = float(returns.std()) * math.sqrt(periods_per_year) if len(returns) else 0.0
    benchmark_mean = float(benchmark_returns.mean()) if len(returns) else 0.0
    benchmark_vol = (
        float(benchmark_returns.std()) * math.sqrt(periods_per_year)
        if len(returns)
        else 0.0
    )
    active = returns - benchmark_returns
    tracking_error = (
        float(active.std()) * math.sqrt(periods_per_year) if len(active) else 0.0
    )
    covariance = (
        float(np.mean((returns - mean) * (benchmark_returns - benchmark_mean)))
        if len(returns)
        else 0.0
    )
    benchmark_variance = (
        float(np.mean((benchmark_returns - benchmark_mean) ** 2))
        if len(returns)
        else 0.0
    )
    fold_rows = [
        {
            "fold": index + 1,
            "start": fold["start"],
            "end": fold["end"],
            "return": fold["metrics"]["total_return"],
            "sharpe": fold["metrics"]["sharpe"],
            "max_drawdown": fold["metrics"]["max_drawdown"],
            "mean_gross": fold["metrics"]["mean_gross"],
            "mean_net": fold["metrics"]["mean_net"],
            "benchmark_return": fold["benchmark"]["total_return"],
            "benchmark_sharpe": fold["benchmark"]["sharpe"],
            "portfolio_minus_benchmark_return": fold["benchmark"][
                "portfolio_minus_benchmark_total_return"
            ],
            "mean_rank_ic": fold["diagnostics"]["mean_rank_ic"],
            "directional_accuracy": fold["diagnostics"]["directional_accuracy"],
            "reward_scale": fold.get("reward_scale"),
            "borrow_fee_row_coverage": (
                fold.get("borrow_diagnostics", {}).get("matrix_rows_with_fee", 0)
                / max(1, fold.get("borrow_diagnostics", {}).get("matrix_rows", 0))
            ),
            "short_fee_position_coverage": (
                fold.get("borrow_diagnostics", {}).get(
                    "short_position_periods_with_fee", 0
                )
                / max(
                    1,
                    fold.get("borrow_diagnostics", {}).get(
                        "short_position_periods", 0
                    ),
                )
            ),
        }
        for index, fold in enumerate(folds)
    ]
    report = {
        "kind": "stockholm_walk_forward_summary",
        "survivorship_status": folds[0]["survivorship_status"],
        "model_family": model_family,
        "feature_set_version": feature_set_version,
        "reward": reward,
        "objective": objective,
        "ensemble_seeds": ensemble_seeds,
        "folds": fold_rows,
        "periods": len(returns),
        "positive_folds": sum(row["return"] > 0 for row in fold_rows),
        "total_return": float(nav[-1] - 1.0) if len(nav) else 0.0,
        "annualised_return": (
            float(nav[-1] ** (periods_per_year / len(nav)) - 1.0)
            if len(nav)
            else 0.0
        ),
        "annualised_volatility": vol,
        "sharpe": mean * periods_per_year / vol if vol else 0.0,
        "max_drawdown": float(drawdowns.min()) if len(drawdowns) else 0.0,
        "target_sharpe": args.min_sharpe,
        "diagnostics": {
            "observations": sum(item["observations"] for item in diagnostics),
            "decision_dates": sum(item["decision_dates"] for item in diagnostics),
            "mean_rank_ic": sum(
                item["mean_rank_ic"] * item["decision_dates"] for item in diagnostics
            )
            / sum(item["decision_dates"] for item in diagnostics),
            "positive_rank_ic_dates": sum(
                item["positive_rank_ic_dates"] for item in diagnostics
            ),
            "directional_accuracy": sum(
                item["directional_accuracy"] * item["observations"]
                for item in diagnostics
            )
            / sum(item["observations"] for item in diagnostics),
            "mean_absolute_error": sum(
                item["mean_absolute_error"] * item["observations"]
                for item in diagnostics
            )
            / sum(item["observations"] for item in diagnostics),
            "buckets": [],
        },
        "benchmark": {
            "symbol": benchmark_metadata[0]["symbol"],
            "name": benchmark_metadata[0]["name"],
            "return_type": benchmark_metadata[0]["return_type"],
            "currency": benchmark_metadata[0]["currency"],
            "source": benchmark_metadata[0]["source"],
            "periods": len(benchmark_returns),
            "total_return": (
                float(benchmark_nav[-1] - 1.0) if len(benchmark_nav) else 0.0
            ),
            "annualised_return": (
                float(
                    benchmark_nav[-1] ** (periods_per_year / len(benchmark_nav))
                    - 1.0
                )
                if len(benchmark_nav)
                else 0.0
            ),
            "annualised_volatility": benchmark_vol,
            "sharpe": (
                benchmark_mean * periods_per_year / benchmark_vol
                if benchmark_vol
                else 0.0
            ),
            "portfolio_minus_benchmark_total_return": (
                float(nav[-1] - benchmark_nav[-1]) if len(nav) else 0.0
            ),
            "portfolio_minus_benchmark_annualised_return": (
                float(
                    nav[-1] ** (periods_per_year / len(nav))
                    - benchmark_nav[-1]
                    ** (periods_per_year / len(benchmark_nav))
                )
                if len(nav)
                else 0.0
            ),
            "max_drawdown": (
                float(benchmark_drawdowns.min())
                if len(benchmark_drawdowns)
                else 0.0
            ),
            "tracking_error": tracking_error,
            "information_ratio": (
                float(active.mean()) * periods_per_year / tracking_error
                if tracking_error
                else 0.0
            ),
            "correlation": (
                covariance
                / (float(returns.std()) * float(benchmark_returns.std()))
                if len(returns) and returns.std() and benchmark_returns.std()
                else 0.0
            ),
            "beta": covariance / benchmark_variance if benchmark_variance else 0.0,
        },
        "long_pnl": sum(fold["metrics"]["long_pnl"] for fold in folds),
        "short_pnl": sum(fold["metrics"]["short_pnl"] for fold in folds),
        "cost_drag": sum(fold["metrics"]["cost_drag"] for fold in folds),
        "long_positions": sum(fold["metrics"]["long_positions"] for fold in folds),
        "short_positions": sum(fold["metrics"]["short_positions"] for fold in folds),
        "mean_gross": sum(
            fold["metrics"]["mean_gross"] * fold["metrics"]["periods"] for fold in folds
        )
        / len(returns),
        "mean_net": sum(
            fold["metrics"]["mean_net"] * fold["metrics"]["periods"] for fold in folds
        )
        / len(returns),
        "borrow_diagnostics": {
            field: sum(item.get(field, 0) for item in borrow_diagnostics)
            for field in (
                "matrix_rows",
                "matrix_rows_with_fee",
                "short_position_periods",
                "short_position_periods_with_fee",
                "observed_holding_cost_drag",
                "fallback_holding_cost_drag",
                "availability_penalty_drag",
            )
        },
        "passed": False,
        "disclosures": folds[0]["disclosures"],
    }
    direction_presence = [fold.get("direction_metrics") is not None for fold in folds]
    if any(direction_presence) and not all(direction_presence):
        raise ValueError("direction overlay is present in only some folds")
    if all(direction_presence):
        direction_returns = np.asarray(
            [
                step["direction_market_return"]
                for fold in folds
                for step in fold["steps"]
            ],
            dtype=np.float64,
        )
        if not np.isfinite(direction_returns).all():
            raise ValueError("direction-layer returns are missing or invalid")
        direction_nav = np.cumprod(1.0 + direction_returns)
        direction_peaks = np.maximum.accumulate(
            np.concatenate(([1.0], direction_nav))
        )
        direction_drawdowns = (
            np.concatenate(([1.0], direction_nav)) / direction_peaks - 1.0
        )
        direction_mean = float(direction_returns.mean())
        direction_vol = float(direction_returns.std()) * math.sqrt(periods_per_year)
        direction_attributions = [
            step["direction"] for fold in folds for step in fold["steps"]
        ]
        report["direction_layer"] = {
            "cost_status": "gross_before_execution_costs",
            "periods": len(direction_returns),
            "total_return": float(direction_nav[-1] - 1.0),
            "annualised_return": float(
                direction_nav[-1]
                ** (periods_per_year / len(direction_returns))
                - 1.0
            ),
            "annualised_volatility": direction_vol,
            "sharpe": (
                direction_mean * periods_per_year / direction_vol
                if direction_vol
                else 0.0
            ),
            "max_drawdown": float(direction_drawdowns.min()),
            "mean_budget_gross": float(
                np.mean(
                    [
                        item["decision"]["budget"]["max_gross"]
                        for item in direction_attributions
                    ]
                )
            ),
            "mean_budget_net": float(
                np.mean(
                    [
                        item["decision"]["budget"]["target_net"]
                        for item in direction_attributions
                    ]
                )
            ),
            "regime_periods": {
                regime: sum(
                    item["decision"]["regime"] == regime
                    for item in direction_attributions
                )
                for regime in (
                    "strong_up",
                    "up",
                    "neutral",
                    "down",
                    "strong_down",
                )
            },
        }
    for bucket_index in range(10):
        parts = [item["buckets"][bucket_index] for item in diagnostics]
        observations = sum(part["observations"] for part in parts)
        denominator = max(1, observations)
        report["diagnostics"]["buckets"].append(
            {
                "bucket": bucket_index + 1,
                "observations": observations,
                "mean_prediction": sum(
                    part["mean_prediction"] * part["observations"] for part in parts
                )
                / denominator,
                "mean_realised_return": sum(
                    part["mean_realised_return"] * part["observations"]
                    for part in parts
                )
                / denominator,
                "directional_accuracy": sum(
                    part["directional_accuracy"] * part["observations"]
                    for part in parts
                )
                / denominator,
            }
        )
    report["passed"] = (
        report["survivorship_status"] == "POINT_IN_TIME"
        and all(fold.get("survivorship_status") == "POINT_IN_TIME" for fold in folds)
        and report["total_return"] > 0
        and report["sharpe"] >= report["target_sharpe"]
        and report["positive_folds"] >= math.ceil(len(folds) / 2)
    )
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(
        f"{args.out}: {model_family}/{feature_set_version} "
        f"{reward}/{objective}/{ensemble_seeds} seed(s), "
        f"return {report['total_return']:.1%}, Sharpe {report['sharpe']:.2f}, "
        f"max DD {report['max_drawdown']:.1%}, {report['positive_folds']}/{len(folds)} "
        f"positive, IC {report['diagnostics']['mean_rank_ic']:+.4f}; "
        f"{report['benchmark']['symbol']} "
        f"{report['benchmark']['total_return']:.1%}, "
        f"excess {report['benchmark']['portfolio_minus_benchmark_total_return']:.1%}, "
        f"passed={report['passed']}"
    )


if __name__ == "__main__":
    main()
