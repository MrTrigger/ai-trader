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
| `planner`, backtest, validation | Python | Phase 1 is almost entirely research loop — sweeps, holdouts, factors discarded by the dozen — and that is what Python is best at |
| State | Postgres (operational) + Parquet (bars, features, scores) | |

[§0.1](docs/design-spec.md#0-guiding-principles-non-negotiable) requires research and live to be
**one** implementation of the decision path — which is satisfied by the backtest being
`pipeline.run()` replayed over history, not by the choice of language. The language choice rests on
where the work is, and Phase 1's work is research. The immutable Plan row is the boundary between
the two languages, checked by a CI round-trip test against a shared JSON Schema.

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

**The backtest is `pipeline.run()` replayed over history** — the same function the live planner
calls, with its orders filled against the bar that opened at each decision timestamp. There is no
second engine, and that is the point: §0.1 requires research and live to be one implementation of
steps 1–8, and this satisfies it by construction rather than by discipline.

Causality has two mechanisms and neither depends on remembering to be careful. `store.read(until=)`
never loads a bar past the horizon, so a bar the decision may not use is absent rather than
ignored. And fills land on the bar that *opens* at the decision timestamp — the one the planner
excluded as still forming — which is the earliest honest price. Filling at that bar's close would
be a free look at a whole interval.

```bash
ai-trader backtest --start 2026-01-01 --end 2026-08-01           # with the 2× slippage error bar
ai-trader backtest --start 2026-01-01 --end 2026-08-01 --nav out.csv
```

Dates where a gate would have failed live produce no plan here either, are absent from every
number, and are counted in the disclosures — a backtest that silently traded through a gate failure
measures a system that does not exist.

### The Phase 1 candidate

**Cross-sectional momentum with a skip period**, long-only, top ~10 of a liquidity-ranked
universe. It is a *hypothesis*, not a conclusion — `ai-trader gate` is what decides whether it
survives contact with costs, and it is built to be capable of saying no.

The skip period is the part that isn't obvious: rank on the return from **t−30d to t−7d**, not the
plain 30-day return. Short-horizon reversal is well documented in crypto, so a plain measure has
last week's reversal baked into it and the two effects partially cancel.

Long-only is not a preference — shorting spot needs margin, and
[§9.2](docs/design-spec.md#92-explicitly-not-in-scope) puts leverage above 1× out of scope before
Phase 3. Shorts arrive with a venue decision, not before it.

**How it most likely dies:** long-only crypto momentum is a leveraged BTC bet in a rising market,
and an attribution that ignores beta will call that alpha. `max_benchmark_beta` is what stops the
book expressing it, and the beta-neutral residual is what the gate should be read on.

### The data problem, and why it did not cost anything

A momentum backtest over *currently listed* assets is survivorship bias at its most flattering: the
assets momentum would have bought are disproportionately the ones that ran up and then died.

Binance keeps the dead. `exchangeInfo` retains delisted pairs as `BREAK`, the REST klines endpoint
serves their full history up to the delisting date, and `data.binance.vision` retains more still.
The store here holds **656 assets and 714k daily bars, of which 174 series end before 2026-07** —
LUNA, FTT, COCOS, BTCST among them. LUNA ranks #4 and eligible on 2022-05-13, mid-collapse, which
is exactly what a survivor-only universe could never show.

```bash
ai-trader universe rank --start 2021-10-01 --end 2026-08-01 --top 40   # point-in-time snapshots
ai-trader gate --start 2021-10-01 --end 2026-08-01                     # the four criteria
```

The distinction that makes this legitimate: **reconstructing a rule from complete history is
point-in-time; reconstructing a survivor list is not.** "Top N by trailing median turnover among
assets with bars at T" uses only inputs knowable at T. Delisted assets are recorded as delisted
rather than dropped — omitting them would hand the backtest survivors by a different route.

One caveat found while wiring it: the archive is **not internally consistent about its epoch
unit** — older dumps are milliseconds, newer ones microseconds. The unit is inferred by magnitude
and the result range-checked, because the same mistake in the other direction lands a bar in 1970,
where it sorts before everything and silently becomes the oldest row of every rolling window.

### Validation

Splits are by date and applied to *results*, never to inputs — slicing bars and replaying the slice
would start every window flat with a cold feature frame. Sweeps are one axis at a time, and report
**the plateau's centre, never the peak**; a width-1 plateau is labelled as the peak it is.

```bash
ai-trader backtest --start 2021-10-01 --end 2026-08-01   # with the 2× slippage error bar
```

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

**There is no dependency on the journal's `backtest/` harness, and there should not be.** That
harness measures discrete futures trades in R-multiples over New York sessions, one position at a
time; every module in its `validate/` package imports its own engine directly. This project
measures a continuously rebalanced multi-asset book, and an R-multiple is undefined for a position
that is resized rather than stopped out. What is inherited is the *discipline* — prefix-invariance
causality testing, plateau-not-peak sweeps, cost sensitivity as an error bar, disclosure-first
ordering, every number with its `n`. See
[§2](docs/design-spec.md#2-what-is-inherited-from-trading-journalbacktest-and-what-is-not) for the
evidence behind that call.

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
- The **strategy** ([§10.2](docs/design-spec.md#10-open-questions-unresolved-and-how-they-get-resolved)).
  `ai-trader scores` exists so this gets decided against evidence; nothing downstream can be
  finished until it is.
