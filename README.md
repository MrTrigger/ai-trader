# AI Trader

An always-on system that manages a portfolio of liquid crypto assets on an hours-to-weeks horizon:
maintain a universe, compute features, produce a target portfolio, enforce portfolio-level risk,
execute the difference, reconcile against venue truth.

Later, the same engine runs equities through a second venue adapter and a real market calendar.

- **Design spec:** [`docs/design-spec.md`](docs/design-spec.md) — read §0 before writing code.

## The two rules everything else follows from

1. **The money path is deterministic.** No LLM sits between a signal and an order. Where a model
   is present, its output is a typed feature or a veto — never an order, size, or target.
2. **No capital before evidence.** Live money requires clearing the backtest harness *and*
   paper-trading within its error bars. Both gates, in that order.

## Stack

| Layer | Tech | Why |
|---|---|---|
| `api`, frontend | Rust (axum + sqlx), React + TS (Vite, Tailwind) | same as `trading-journal` |
| `executor` | Rust | long-running, holds trade credentials, needs explicit error handling |
| `planner`, backtest, validation | Python | scipy/cvxpy/pandas, and the backtest harness is Python |
| State | Postgres (operational) + Parquet (bars, features, scores) | |

The split is not a preference — it's forced by [§0.1](docs/design-spec.md#0-guiding-principles-non-negotiable):
research and live must be **one** implementation of the decision path, and that path is Python
because the harness is. The immutable Plan row is the boundary between the two languages, and its
schema is generated from one source with a CI round-trip test.

See [design spec §3.5](docs/design-spec.md#35-implementation-stack-and-why-it-is-split).

## Status

**Phase 0 in progress.** No venue account, no capital, no strategy — the Phase 0 signal is an
explicit placeholder that claims no edge and says so on every plan it produces.

Done: the Plan contract (schema + Python writer + Rust parser + a committed fixture CI checks both
halves against), the bar store, a public `DataSource`, point-in-time universe snapshots, and the
decision path — eligibility, construction, risk gate, cost-aware diff, orders.

```bash
ai-trader data pull --days 400 && ai-trader universe record && ai-trader book init --cash 100000
ai-trader plan --as-of 2026-08-01
ai-trader plan verify --runs 3     # the gate
```

Remaining for Phase 0: the `paper` venue adapter, and a first green CI run.

See [design spec §9](docs/design-spec.md#9-phased-build-order) for the phased order and the gates.

### Local build note

The Rust crate needs an MSVC linker on Windows. If `cargo test` fails with `linker link.exe not
found`, install the **Desktop development with C++** workload from the Visual Studio Installer.
CI builds it on Linux and does not need this.

## Relationship to `trading-journal`

Separate system, separate deployment, separate security boundary — see
[design spec §1](docs/design-spec.md#1-why-this-is-not-part-of-trading-journal).

The journal records *a human's* discretionary futures trades and the judgment behind them. This
manages a book automatically. Different core record, different uptime requirements, different blast
radius. They unify at the read layer (a portfolio view / MCP over both), never at the write layer.

The journal's `backtest/` harness is reused in place — its causality guarantee and walk-forward
validation are what Phase 1's gate is measured with. Not vendored, not extracted yet.

## Deliberately undecided

The **venue** (OKX EU vs Hyperliquid vs on-chain spot) is unresolved on purpose, and gets decided
by the Phase 1 breadth sweep rather than by preference — see
[design spec §10.1](docs/design-spec.md#10-open-questions-unresolved-and-how-they-get-resolved).
Public market data needs no account anywhere, so nothing is blocked by that decision.

Until it's made: `paper` adapter only.
