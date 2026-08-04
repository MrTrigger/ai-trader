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

**Phase 0 complete; its gate is met.** No venue account, no capital, no strategy — the Phase 0
signal is an explicit placeholder that claims no edge and says so on every plan it produces.

> **The gate:** `plan --dry-run` produces byte-identical plans across two runs, **and** Rust parses
> a Python-written Plan in CI.

Both halves hold. `plan verify` passes on real bars and on synthetic bars in `test_pipeline.py`, so
CI enforces determinism without shipping a data set; the `cross-language contract` CI job
regenerates the fixture from the current planner, fails if the committed bytes drifted, and then
has Rust parse it.

Built:

| Piece | Where |
|---|---|
| Plan schema, v1.1.0 | `schema/plan.schema.json` |
| Python planner — bars, store, source, universe, decision path, CLI | `planner/` |
| Rust Plan parser (parse-only by design) | `service/crates/plan` |
| `VenueAdapter` interface + shared venue types | `service/crates/venue` |
| `paper` venue adapter | `service/crates/paper` |

```bash
ai-trader data pull --days 400 && ai-trader universe record && ai-trader book init --cash 100000
ai-trader plan --as-of 2026-08-01
ai-trader plan verify --runs 3     # the gate
```

**Phase 1 has started, from the strategy-independent end.** The risk layer
([§6](docs/design-spec.md#6-risk-the-layer-the-alternatives-dont-have)) now enforces the two limits
the spec argues hardest for, both of which catch the same failure — a book that looks diversified
and is not:

- **`max_cluster_exposure`** — gross per correlated group, over a configured grouping. §6 calls
  this not optional: a hundred crypto assets are approximately one asset, and an equal-weight book
  across them is a leveraged beta bet wearing a diversification costume. An asset the grouping does
  not name escapes the limit, and the plan discloses it by name rather than absorbing it.
- **`max_benchmark_beta`** — `|wᵀβ|` against a configured benchmark (BTC now, SPY for equities
  later; same code). Beta is a new causal feature, a trailing 90-bar regression slope. An asset
  whose beta cannot be estimated is assumed to be 1.0 — conservative by construction, since a
  missing estimate can then only tighten the limit, never relax it. Also disclosed.

Both thresholds are **asserted, not derived**. They are judgements recorded in `config/default.toml`
with their reasoning, and Phase 1's sweep is where a number like that earns its value.

`planner/tests/test_features.py` also adds the causality test the harness contributes and this repo
had been missing: build features over the full history, rebuild over every prefix, require row *i*
identical. Beta is why it exists now — every other feature is a rolling window over one asset's own
column, but beta joins a second series across assets on timestamp, which is exactly the shape of
operation that can quietly reach forward.

The **`scores` layer** ([§5.2](docs/design-spec.md#52-tables-sketch--refine-in-phase-0),
[§9.1](docs/design-spec.md#91-what-phase-6-inherits-and-what-it-doesnt)) is in too. Scores are not
features: a feature measures one asset, a score ranks it against the others in its group, which is
why a score is only replayable stored beside the universe snapshot that produced it. The framework
is shared across asset classes — sub-factors, equal weight within a parent, group-relative
percentile rank, weighted composite — and the factors are not.

Two things can go wrong and neither is allowed to be silent: a measurement can be missing, or the
group can be too small to rank within. Both score neutral and both say so, because a flat 50 that
reads downstream as a real measurement of average-ness is the quiet way a backtest gets corrupted.

```bash
ai-trader scores --as-of 2026-08-01                # rank across the whole universe
ai-trader scores --as-of 2026-08-01 --by-cluster   # rank within configured clusters
```

That command is **a lens, not a decision** — nothing in the decision path consumes scores yet, and
the factor set it displays is a candidate cross-section that has never been near the harness and
claims no edge. It exists so the strategy can be chosen against evidence rather than argument.

Still to come in Phase 1: a real signal, and the full backtest through the harness. **The strategy
itself is undecided on purpose** — see
[§10.2](docs/design-spec.md#10-open-questions-unresolved-and-how-they-get-resolved).

### Two known gaps, recorded so they are not rediscovered

- **The Rust Plan types are hand-written against the schema**, where
  [§3.5](docs/design-spec.md#35-implementation-stack-and-why-it-is-split) calls for generating them
  from it. `deny_unknown_fields` plus the fixture diff catches most drift, but not a field added to
  the schema that neither side implements. Worth closing before Phase 1 hardens the schema.
- **`paper` has no live price feed yet.** It is a fake broker over a `PriceSource`, and the only
  implementation so far is `ManualPrices`. Phase 2 adds a real-time feed as a second implementation;
  the adapter does not change when it lands.

### Local development

Linux (or WSL) — production is a Debian pod, and the Rust half needs a gcc toolchain.

```bash
python3 -m venv planner/.venv
planner/.venv/bin/pip install -e "planner[dev]"
(cd planner && ../planner/.venv/bin/pytest -q)     # 85 tests

(cd service && cargo test --all)                   # 53 tests
```

Market data is gitignored and re-pulled with the commands above (public API, no account).

Keep the repo on the Linux filesystem rather than under `/mnt/c`: Rust builds across the 9p mount
are slow enough to be worth avoiding, and it sidesteps the CRLF class of bug entirely.

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

Until it's made: `paper` adapter only — which is not a placeholder. Per
[§4.1](docs/design-spec.md#41-venueadapter) it stays first-class forever, and Phase 2's gate is run
against it for weeks.

Also open, and belonging to a human rather than to an agent:

- Phase 2's **"≥6 weeks paper"** is an invented number, not a derived one.
- The [§5.2](docs/design-spec.md#52-tables-sketch--refine-in-phase-0) table sketch is worth a review
  before Phase 1 hardens the schema.
- Whether `trading-journal/backtest` stays a path dependency or gets extracted
  ([§2](docs/design-spec.md#2-reused-not-rebuilt) says Phase 4+, deliberately).
