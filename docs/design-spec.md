# AI Trader — Design Spec

> **Implementation update (2026-08-07):** sections below that assign the
> crypto planner, feature computation, backtest, or validation to Python are a
> historical design record. Those components now live in Rust
> (`features-crypto` and `crypto-portfolio`). Python is retained only to fit a
> model from the final Rust-emitted matrix and does not compute or preprocess
> model inputs. See the repository README for the current map.

> **Brief for a coding agent.** Read §0 first — the principles constrain every decision below.
> This spec is **phased and gated on purpose**. Each phase has an exit gate stated in evidence,
> not in features. Do not build a later phase until the prior gate is passed. Do not put capital
> at risk before Phase 3, and do not reach Phase 3 by deciding to — reach it by passing Phase 2.

**What:** A single-user, always-on system that manages a portfolio of liquid crypto assets on a
hours-to-weeks horizon: it maintains a universe, computes features, produces a target portfolio,
enforces portfolio-level risk, executes the difference, and reconciles against venue truth.

**Later:** the same engine manages equities and other securities through a second venue adapter and
a market calendar. That is a Phase 6 concern and shapes the interfaces now (§4), nothing else.

**Not this:** [`trading-journal`](../../trading-journal) — that records *a human's* discretionary
futures trades and the judgment behind them. See §1.

---

## 0. Guiding principles (non-negotiable)

1. **The money path is deterministic.** Every decision that moves capital is made by code that,
   given identical inputs, produces identical outputs. No LLM sits between a signal and an order.
   This is the principle the rest of the document exists to protect.
2. **No capital before evidence — and the available evidence differs by layer.** The deterministic
   core reaches live money only after clearing the backtest harness (positive expectancy after
   costs, survives 2× slippage, survives walk-forward) *and* paper-trading within the backtest's
   error bars. Both gates, in that order.
   A **judgment-layer** feature (§7.3) cannot clear the first gate at all, because its backtest is
   contaminated. Capital behind one is therefore funding an experiment, not deploying a validated
   strategy. That is a legitimate thing to do deliberately and a bad thing to do by accident, so it
   is stated here: know which of the two you are doing, and size it accordingly.
3. **Fail closed.** Missing data, stale data, an incomplete universe, an unreachable venue, or a
   position that disagrees with the venue means **do nothing and alert**. Never act on partial
   knowledge. The default state of an automated trader is *flat and idle*, not *guessing*.
4. **The model cannot invent a trade.** Where an LLM is present, its output space is bounded and
   typed — a feature value, a veto, a label. Never an order, never a size, never a target.
   See §7.
5. **Reproducible without the model, without the MCP, without the network.** Every action the
   system can take is a CLI command you can run yourself. Interactive surfaces are lenses over
   that CLI, never a dependency of it.
6. **Reconcile against the venue, always.** Our position record is a hypothesis; the venue's is
   truth. Every run verifies them and refuses to trade on a mismatch.
7. **The event log is the asset.** Fills, plans, runs, and NAV snapshots are append-only. Positions
   are *derived*, never authoritative. History is never silently edited — same honesty principle as
   the journal, for the same reason.
8. **One risk layer, enforced at plan construction.** Position-level logic cannot breach a
   portfolio-level limit, because the plan is rejected before it can. See §6.

---

## 1. Why this is not part of `trading-journal`

Logged decision. The journal stays as scoped; this is a separate repo, image, and deployment.

| | `trading-journal` | `ai-trader` |
|---|---|---|
| Core record | discrete **closed trade** (one entry, one exit) | continuous **position state** + append-only fills |
| The valuable half | judgment block — compliance, violation, `sweep_depth_atr`, one-line reason | features, plan provenance, model/rule version |
| Who decides | a human, discretionarily | code, deterministically |
| Uptime | none — human-triggered, writes rarely | always-on daemon, capital at risk |
| Auth posture | none by design, LAN-gated behind `envoy-internal` | holds venue credentials; must never share that boundary |
| Failure blast radius | you lose logging for a day | you lose money |

The journal's `trades` schema encodes a human's judgment about a discretionary trade. An
AI-managed position has none of those columns and needs several the journal has no concept of
(lots, funding accrued, NAV series, plan lineage). Merging them makes every column nullable and
guts the judgment block, which the journal's own spec §0.6 calls the half that carries the
analytical value.

**Precedent:** `backtest/` already lives beside the journal, shares its vocabulary, and is never
built into its image — because the journal's product spec §Phase 3 says outright not to fold it in.
Same reasoning, stronger case.

**Unification happens at the read layer.** A single portfolio view (and later an MCP, §8) queries
all three systems and composes the answer. That is how "my whole portfolio in one place" gets
solved — without coupling three write paths. Build it after two systems produce real numbers; its
shape is unguessable before then.

---

## 2. What is inherited from `trading-journal/backtest`, and what is not

**The discipline, not the code. There is no dependency.** An earlier draft of this section called
for a path dependency on that package. Reading it rather than describing it showed that would not
work, and the reasons are worth recording so the question is not reopened by someone who has only
read the summary.

### 2.1 Why the code does not transfer

| | `trading-journal/backtest` | what this project needs |
|---|---|---|
| Unit of analysis | a **discrete trade** — entry, stop, target | a **continuously rebalanced book** of weights |
| Headline metric | `expectancy_r`, `win_rate`, `net_r` — R-multiples | NAV series, drawdown, turnover, cost drag |
| Splits | by **session**, attributed by session date | by UTC date; there are no sessions |
| Position model | `engine/portfolio.py`: one position at a time, `max_trades_per_day`, `daily_stop_r` | 20 names concurrently, sized by weight |
| Causality device | `BarCursor` — a one-way walk over **one session's** bars, single instrument | a **cross-sectional** cursor: every asset's row at time *T* |
| Stated ceiling | its README: *"It cannot validate an edge"* — "not obviously broken" | §9 Phase 1: expectancy after costs, 2× slippage, walk-forward |

The decisive detail is that `validate/` is **not a harness parameterised over a strategy**. Every
substantive module in it — `sweep`, `walkforward`, `sensitivity`, `report` — does
`from ..engine import run_backtest, Rules, RunConfig`. It is wired to that engine's signature and
to rule files with fields like `stop_offset_ticks` and `trigger_offset_ticks`. `sweep.SWEEPABLE`
is a hardcoded list of them. None of those concepts exist here.

Only `splits.py` and `metrics.py` are engine-free, and both are trade-and-session shaped.

An R-multiple is undefined for a position that is resized every rebalance rather than exited at a
stop. That is not an impedance mismatch to adapt around; it is a different measurement.

### 2.2 What is inherited

Principles, and they are worth as much as they ever were:

- the **causality guarantee** — build features over full history, rebuild over every prefix,
  require row *i* identical both times. A feature is causal exactly when deleting the future
  doesn't change it. Enforced in `features-crypto` prefix-invariance tests.
- **lookahead should raise, not be discouraged.** `crypto-portfolio` selects a closed-bar horizon
  before calling the shared Rust decision function; training uses the same T-1d/T-1h snapshots.
- **report the plateau's centre, never the peak**, and sweep one axis at a time.
- **cost sensitivity as an error bar, not a parameter** — re-run at 2× slippage and see what
  survives. §9's Phase 1 gate is stated in exactly these terms.
- **disclosure-first ordering** — what was *not* enforced, before any number.
- **every number carries its `n`**, and an inadequate sample says so above the result.

### 2.3 What the backtest here actually is

`crypto_portfolio::decide(as_of=T)` replayed over history with a simulated fill model, accumulating a NAV
series. That is not a convenience — it is the only construction that satisfies §0.1, because the
backtest and the live run are then *literally the same function*. A harness with its own engine
cannot provide that no matter how good the engine is, which is the deeper reason the dependency
was the wrong idea rather than merely an awkward one.

Extraction of anything shared into a common package remains a Phase 4+ decision, and now needs a
second consumer that actually wants the same abstraction — which, per §2.1, this one does not.

---

## 3. Architecture

### 3.1 The run loop

One pass. Ordered. Each step is a pure function of the prior step's output plus stored state.

```
  1. OBSERVE     fetch bars, universe, venue balances/positions
                 -> gates: freshness, completeness, reachability, reconciliation
  2. FEATURIZE   build the point-in-time feature frame (materialized, causal)
  3. SIGNAL      rules/model -> per-asset desired direction + conviction
  4. RISK MODEL  covariance + factor exposures (§4.4) -> an INPUT to construction
  5. CONSTRUCT   PortfolioConstructor (§4.5): expected return net of costs,
                 subject to soft constraints -> target weights
  6. RISK GATE   enforce hard portfolio limits -> plan REJECTED or ACCEPTED  (§6)
  7. DIFF        target state - actual state, within a turnover budget (§6)
                 -> the minimal set of orders, plus what was deferred
  8. PLAN        persist an immutable Plan (this is the artifact; §5.3)
  9. EXECUTE     submit orders for a Plan id  (skipped in dry-run)
 10. RECONCILE   re-fetch venue truth, verify, record fills + NAV snapshot
 11. REPORT      structured run record; notify on anomaly or rejection
```

Steps 1–8 have **no side effects on capital** and are runnable any time, on any historical date.
That is what makes the whole thing backtestable: the backtest is steps 1–8 over history with a
simulated fill model, and live trading is the same code with steps 9–10 attached.

> **This is the central design property.** If backtest and live ever diverge in steps 1–8,
> the backtest is measuring a strategy you do not run. A test asserts the two paths produce
> identical plans given identical inputs. §3.5 explains how the language split preserves this.

**Risk appears twice on purpose, and the two are not the same thing.** Step 4 estimates
covariance and factor exposures which step 5 *optimises against* — risk shaping construction.
Step 6 enforces hard limits that reject a plan outright — risk vetoing construction. A system
with only the gate produces plans that are legal but badly diversified; a system with only the
model produces elegant plans that can still breach a limit. Both, in that order.

### 3.2 Ordering rules (why this order)

- **Exits before entries** — frees capital before sizing, so entries never size off a stale NAV.
  Falls out of target-state diffing naturally; assert it anyway.
- **Target-state convergence, not imperative orders.** The engine computes *what should be true*
  and diffs. This makes every run idempotent and crash-safe: re-running after a partial failure
  converges rather than duplicating. (Borrowed from the one genuinely good idea in the signum
  prompts.)
- **No re-entry into an asset exited in the same run.** Kills intra-run churn from a signal that
  flips on stale intermediate state.

### 3.3 Deployment

Same homelab, same GitOps, **separate namespace** from the journal — this is the security boundary
that matters. Three processes:

| Process | Language | Trigger | Venue credentials | Role |
|---|---|---|---|---|
| `planner` | Python | cron (daily at first) | **read-only key** | steps 1–8, emits a Plan |
| `executor` | Rust | plan accepted, or CLI | **trade-scoped, no withdrawal** | steps 9–10 |
| `api` | Rust | on demand | **none** | read-only over stored state; serves the frontend, CLI and MCP |

The language split falls out of §3.5, but it buys a security property worth stating on its own:
**the process that decides never holds a key that can trade, and the process that trades never
decides.** `planner` reads balances and positions with a read-only venue key; only `executor`
holds a key that can move capital, and it only ever acts on a stored, risk-cleared Plan id.
`api` holds nothing.

All venue keys come from SOPS secrets and are IP-allowlisted.

### 3.4 Storage

| Data | Store | Why |
|---|---|---|
| bars, features | Parquet, partitioned | matches the harness's existing store; immutable; cheap re-reads for sweeps |
| positions, orders, fills, plans, runs, NAV | Postgres | transactional, needs correctness under concurrent reconcile |
| LLM feature outputs | Parquet + content-hash cache | materialized once, never recomputed — §7.2 |

**Bars are immutable. Fix the source, not the store.** `ts_utc` is always the bar's OPEN. Both
conventions inherited from the harness; adapters convert at the boundary and never again.

### 3.5 Implementation stack, and why it is split

**Preference:** Rust in the backend, React + TypeScript in the frontend — matching
`trading-journal` (axum + sqlx, Vite + Tailwind), which is the stack actually being maintained.

**That preference is now honoured throughout the decision and research path.** Python has one
narrow, offline role: fit a model from a final matrix emitted by Rust.

| Layer | Language | Why not the other one |
|---|---|---|
| `api`, frontend | **Rust + React/TS** | Directly the journal's pattern. No reason to differ. |
| `executor`, venue adapters | **Rust** | A long-running process holding trade credentials. Explicit error types, no GIL, no silent `None`/float surprises, real retry and idempotency discipline. Rust is *better* here, not merely acceptable. |
| planner, features, backtest, validation, reports | **Rust** | One feature implementation and one decision function structurally prevent train/live and replay/live drift. |
| model fitting only | **Python** | LightGBM fitting consumes final `x_*` values and targets. It may not calculate, align, rank, impute, or select production features. |

**§0.1 constrains both shape and ownership in the current implementation.** The research path and
live path call the same Rust `decide()` function, while `features-crypto` owns the exact feature and
rank-normalisation code used by training and inference. Sweeps, holdouts, walk-forward validation,
IC, evidence records, and reports are also Rust. A notebook may inspect exported evidence, but it
cannot become an alternative feature or strategy engine.

The process split still buys the security property in §3.3: the process that decides holds no key
that can trade. It no longer creates a language split.

**The Plan is the process boundary**, and it already had the right shape for this: immutable,
schema'd, risk-cleared, persisted, and addressed by id (§5.3, §8.3). Rust never needs to know how
a plan was computed — only that it exists, validates, and hasn't been superseded.

```
  cron -> crypto-portfolio (Rust)  --writes Plan JSON-->  bot/executor (Rust)
```

**Contract discipline at the process seam:**

- The Plan schema and shared Rust `plan` crate are the contract. The producer round-trips every
  emitted document through the executor parser in tests.
- Version the schema. `executor` refuses a Plan whose schema version it does not know — fail
  closed (§0.3), never best-effort parse.

**Two things named `paper`, deliberately kept distinct** — conflating them silently voids Phase 2:

| | Where | What it is |
|---|---|---|
| `sim` fill model | Rust, inside `crypto-portfolio::backtest` | crosses the spread, charges commission, and fills only at the next excluded bar's open |
| `paper` venue | Rust, a `VenueAdapter` impl | a fake broker on live real-time data — exercises the **real** execution, reconciliation and error-handling path |

Phase 2's gate is about the operational path, not the strategy. Running only the Rust historical
fill model instead of the Rust executor would test nothing that Phase 1 has not already tested.

**Packaging:** the planner and executor are separate Rust binaries with different authority. The
offline Python fitting environment is not deployed in the trading runtime.

---

## 4. Interfaces (the parts that make Phase 6 additive)

Five interfaces carry all venue, asset-class and method variation. Everything else is shared.

### 4.1 `VenueAdapter`

```
  get_markets()      -> [Market]   symbol, tick, lot, min_notional, capabilities
  get_balances()     -> [Balance]
  get_positions()    -> [Position]
  place_order(o)     -> OrderAck
  cancel_order(id)
  get_fills(since)   -> [Fill]
```

`Market.capabilities` declares: fractional sizing, short, leverage, funding. The engine reads
capabilities and adapts; it never branches on venue name.

**Implementations, in build order:**

1. `paper` — **first, and it stays first-class forever.** A fake broker over live real-time data:
   accepts orders, simulates fills, reports positions and balances, and exercises the real
   execution and reconciliation path. Lets the entire system be built and run with no venue
   decision and no capital. Distinct from the backtest's `sim` fill model — see §3.5.
2. `okx` / `hyperliquid` — whichever Phase 3 selects. See §10.
3. an equities broker — Phase 6.

### 4.2 `DataSource`

Bars and universe membership, decoupled from the venue. Public market data needs no account
anywhere, so **data is not on the critical path for the venue decision** — this is deliberate and
it is what makes §10 deferrable at zero cost.

### 4.3 `Calendar`

```
  is_open(ts) -> bool
  sessions_between(a, b) -> [Session]
  observation_time(day) -> ts      # when the engine is allowed to act
```

Crypto: always open, UTC day boundaries. Equities: the harness's existing session logic, holidays,
half-days, halts. This interface is why the equities adapter is additive rather than a rewrite.

### 4.4 `RiskModel`

```
  estimate(features, returns, as_of) -> RiskModelArtifact
      covariance          asset x asset, predicted
      factor_exposures    asset x factor
      specific_variance   per asset
      marginal_contribution(weights) -> per-asset MCTR
```

Consumed by `PortfolioConstructor` (step 5) and reported by step 11. **Point-in-time and
materialised like any other feature** (§7.2 rules apply): estimated from data available at
`as_of`, stored, never recomputed during a sweep.

Implementations, in order: `sample_covariance` (shrunk, e.g. Ledoit–Wolf — the honest baseline),
then a cross-sectional factor model once there are enough factors to justify one. Crypto needs
this at least as much as equities: correlation collapse is the dominant failure mode of a broad
crypto book, and the §6 cluster limit is only a crude proxy for it.

### 4.5 `PortfolioConstructor`

```
  construct(signals, risk_model, costs, current, constraints) -> TargetWeights
```

Swappable and, more importantly, **comparable** — the harness can score constructors against each
other on the same signals rather than the choice being settled by argument.

| Implementation | Notes |
|---|---|
| `equal_weight` | the baseline every other one must beat |
| `conviction_tilt` | equal-weight base, scaled by signal rank; cheap and robust |
| `mvo` | maximise `μᵀw − λ·wᵀΣw`, Σ from `RiskModel`, μ **net of costs** (§6) |

`mvo` must declare a deterministic fallback on non-convergence (`conviction_tilt`) and **record in
the Plan which constructor actually produced the weights**. A silent fallback that isn't recorded
makes a backtest uninterpretable.

---

## 5. Data model (the schema is the asset)

Design it once, now. The importer-shaped lesson from the journal applies: fields that a later
phase needs are cheaper to add today than to migrate into.

### 5.1 Core separation

Mirroring the journal's execution/judgment split, with a different axis:

- **Observation** (objective, reproducible): bars, features, universe membership. Point-in-time.
- **Decision** (derived, versioned): signals, target weights, plans. Carries `ruleset_version`,
  `feature_set_version`, and — if any model contributed — `model_id` and `model_version`.
- **Execution** (venue truth): orders, fills, funding, fees, balances.

Any decision must be re-derivable from its observation inputs and its versions alone. If it is not,
the run is not reproducible and that is a bug, not a limitation.

### 5.2 Tables (sketch — refine in Phase 0)

```
assets            canonical id, symbol aliases per venue, asset_class, decimals
universe_members  (as_of, asset_id, rank, eligible, reason)   -- point-in-time, never backfilled
bars              parquet: asset, interval, ts_utc(OPEN), ohlcv
features          parquet: asset, ts, <feature columns>, feature_set_version
scores            parquet: asset, ts, <sub_factor cols>, <factor cols>, composite,
                            group_key, degenerate_flags, scoring_version
risk_models       parquet: as_of, covariance, factor_exposures, specific_variance,
                            risk_model_version
cost_estimates    parquet: as_of, asset, spread_bps, impact_coef, total_bps
runs              id, started_at, mode(dry|live), status, gate results, versions
signals           run_id, asset_id, direction, conviction, inputs_hash
plans             id, run_id, created_at, status, constructor, target_weights(jsonb),
                  risk_report(jsonb), schema_version, rejected_reason
orders            plan_id, asset_id, side, qty, type, venue_order_id, status
fills             order_id, ts, price, qty, fee, venue_fill_id  -- append-only
positions         derived view over fills; never the source of truth
nav_snapshots     ts, nav_usd, gross_exposure, net_exposure, benchmark_beta, cash
alerts            ts, severity, kind, payload
```

**`scores` is separate from `features` on purpose.** Features are raw measurements; scores are
*cross-sectional transforms* of them — rank within a group, normalise, blend. That distinction
matters because a group-relative rank is not point-in-time reproducible from one asset's row
alone: it depends on which assets were in the universe that day. Storing scores alongside the
`universe_members` snapshot that produced them is what keeps them replayable.

**`degenerate_flags` carries its own honesty.** A factor with insufficient history (fewer
snapshots than its lookback needs) returns a neutral score and *says so*, rather than silently
contributing a flat 50 that reads as a real measurement. Any report including a degenerate factor
discloses it above the numbers (§12).

`nav_snapshots` is the thing the journal has no equivalent of and the thing every performance
question depends on. Write one every run, and on every reconcile, regardless of whether anything
traded.

### 5.3 Plans are immutable artifacts

A `Plan` is the complete description of what the system intends: target weights, the diff, the
orders, the risk report that cleared it, and **which constructor produced it**. Once written it
never changes; a superseded plan is `status = superseded`, not edited.

This matters for four reasons: it is what `execute_plan` takes as input (§8), it is the
cross-language contract between `planner` and `executor` (§3.5), it is what makes a failed run
resumable, and it is the audit record that answers "why did it do that in March".

---

## 6. Risk (the layer the alternatives don't have)

**Every limit is enforced at plan construction. A plan that breaches any limit is rejected whole —
never partially applied, never truncated silently.**

The failure mode this prevents is specific and common: per-asset signals are correlated. In a broad
crypto rally, twenty assets trigger on the same day. Per-asset sizing with no portfolio cap then
"wants" a multiple of NAV, and the system either over-allocates or dies half-built.

| Limit | Notes |
|---|---|
| max gross exposure | % NAV. **Hard.** The one that prevents the above. |
| max net exposure | separate from gross once shorts exist |
| max single position | % NAV |
| max position count | keeps the book auditable by a human |
| max cluster exposure | % NAV per correlated group — see below |
| max benchmark beta | `\|wᵀβ\|` against BTC (crypto) or SPY (equities) — see §6.2 |
| min position notional | below venue minimum → drop, don't submit |
| max drawdown | from peak NAV → auto-pause, alert, require manual resume |
| kill switch | a flag; when set the engine plans but never executes |

**Cluster exposure is not optional.** 100 crypto assets are approximately one asset. Without a
grouping constraint, an equal-weight book across the top 100 is a leveraged beta bet wearing a
diversification costume. Start crude — L1 ecosystem, sector, or a rolling correlation cluster —
and improve it. Crude and present beats sophisticated and Phase 5.

**Longs need an exit that isn't a daily close.** A once-daily close-based exit on a 24/7 market
leaves a position unattended for 24 hours. Either carry a resting stop at the venue or run the
exit check more often than the rebalance. Decide in Phase 1; do not ship Phase 3 without it.

**Turnover is not a limit here, and the distinction is load-bearing.** Every limit above describes
the **resulting portfolio** — how concentrated, how many names, how much gross. Turnover describes
the **transition**. Putting it in this gate made an initial build from flat unreachable: going 0%
to 75% invested *is* 75% turnover, so the first run of any deployment breached a 50% cap and was
rejected whole. Found by running the real pipeline, not by reasoning about it.

It is a **budget in the diff** (§3.1 step 7): trades are taken largest-drift-first — the trade
closing the most distance buys the most convergence per unit spent — until the budget is exhausted,
and the remainder is deferred to the next run. The destination is never vetoed, only paced.

**Exits are exempt and are never deferred.** A position we no longer want is risk we are still
carrying, and pacing our way out of it is the same exposure held longer for the sake of a number.
If exits alone exceed the budget they all still go, and the overspend is disclosed.

**Whatever is deferred is disclosed on the plan** (`turnover_capped`). A plan that quietly did less
than it said reads afterwards as a plan that failed.

### 6.1 Costs belong in the objective, not only in the report

Estimated transaction cost per asset — spread, commission, and market impact as a function of
trade size against recent volume — is **subtracted from expected return before optimisation**
(step 5), not merely measured afterwards.

This is not a refinement. An optimiser that cannot see costs will happily propose a rebalance
whose gross improvement is smaller than the spread it crosses to get there, and no turnover cap
fixes that — a cap limits how much churn you do, not whether the churn was worth doing. Costs in
the objective is what makes the constructor decline a trade *on its merits*.

The realised-versus-estimated spread is then a first-class output (step 11) and a Phase 3 gate:
if the cost model is wrong, every plan the optimiser produced was solving the wrong problem.

### 6.2 Benchmark beta

Estimated per asset against a configured benchmark — BTC for crypto, SPY for equities — and
constrained at the portfolio level. Same code, different benchmark.

Crypto arguably needs it more than equities do. A long book of alts with no beta constraint is a
leveraged BTC position wearing a diversification costume, and the P&L attribution (step 11) will
credit "alpha" for what was simply beta in a bull market. Constrain it, then attribute against it.

### 6.3 Capacity (deferred, but the numbers are known)

**Not a Phase 1 problem.** At a $30k book impact is negligible — Sharpe falls from
2.70 to 2.15 even at four times the assumed impact coefficient, and the median order
is 0.05% of the volume in the hour it trades. This section exists so the work is not
re-derived when it matters, which is above roughly **$300k**.

The binding constraint above that is not cost but **feasibility**. A square-root
impact model returns a finite number for an order larger than all the liquidity
available, so cost-based sizing fails silently: at $3M the 99th-percentile order was
138% of its execution hour's entire volume. Participation has to be a constraint in
its own right, not an output.

Three mechanisms, measured in Phase 1 and recorded here rather than built:

**Cap by liquidity, not only by NAV.** A position's cap is the lower of
`max_single_position` and `participation_limit × hourly_volume / NAV`. 5% of an
hour's volume is the workable limit; 2% strands more capital than the impact it
avoids.

**Spill the remainder down the ranking.** Whatever a capped name cannot absorb goes
to names with headroom, extending *past* the profitability threshold when — and only
when — the alternative is idle capital. A marginal name beats no position; it does
not beat a good one. This raised Sharpe from 2.53 to 2.65 **at $30k, where liquidity
never binds**, purely from holding ~31 names instead of ~20, so it is worth adopting
before capacity is the reason.

**Scale in and out over several hours.** N slices divide each order by N against the
same per-hour liquidity, so impact falls as `1/√N` while the cap rises
proportionally. The cost is signal decay — measured at Sharpe 2.22 at a one-hour
fill lag, 2.09 at four, 1.86 at eight — so the optimal window grows with the
account: 2h at $30k, 8h at $3M, 4h at $10M. Exits need this as much as entries.

Together these move practical capacity from about $300k to about $10M.

**What is deliberately not modelled.** A fixed time-slice schedule is the crudest
execution that works. Real execution reads the order book — accelerating into depth,
waiting out thin patches, posting passively rather than crossing — and maker fills
alone were worth roughly +110pp over the Phase 1 window. **Every capacity number
above is therefore a floor.** Modelling better needs order-book depth history, which
the store does not have and the public archives do not publish at a size worth
pulling. That is an execution-layer concern (§9, Phase 3+), not a research one.

---

### 6.4 Gates that stop the run

Fail closed, alert, exit non-zero:

| Gate | Condition |
|---|---|
| freshness | newest bar older than the interval's tolerance → **stop** |
| completeness | universe smaller than expected → **stop**, do not evaluate exits |
| reachability | venue unreachable after bounded retries → **stop** |
| **reconciliation** | our derived positions ≠ venue positions → **stop, alert loudly, never auto-correct** |
| plan sanity | plan breaches any §6 limit → **reject the plan**, alert, leave book untouched |

Completeness deserves the emphasis: a truncated universe manufactures false "asset dropped out"
exits and will liquidate a healthy book on a bad API response. This is the highest-severity silent
failure available to this class of system.

Reconciliation drift is never auto-corrected. A mismatch means one of our assumptions is wrong;
trading on top of a wrong assumption is how a small bug becomes a large loss.

---

## 7. Where AI is, and is not

**LLM as feature generator: yes. LLM as decision maker: no.** The line is whether the output is
consumed by a deterministic function, or *is* the action.

### 7.1 Permitted roles

| Role | In the money path | Notes |
|---|---|---|
| unstructured → numeric feature | yes, as a materialized column | the real one — §7.2 |
| negative screening / veto | yes, as a boolean | failure mode is "don't trade", the safe direction |
| portfolio-manager recommendation | yes, as a **signal** | `{asset: BUY\|SELL\|HOLD, confidence}` consumed by the constructor. See below |
| research hypothesis generation | **no** — offline | proposes features; the harness kills the bad ones. Highest EV, zero runtime risk |
| plan review / anomaly flagging | **no** — advisory | flags, never blocks, never modifies |
| post-trade narration | **no** — after the fact | the "why" record; the AI-side analogue of the journal's judgment block |

Explicitly excluded: choosing sizes, evaluating a numeric threshold, or constructing an order.
Those are arithmetic. Code does arithmetic.

**On the portfolio-manager role.** A per-asset BUY/SELL/HOLD is permitted because it is a *signal,
not an order*: the constructor still turns labels into weights, the risk gate still evaluates them,
the diff still produces the orders. The model's output space does not contain an executable
instruction, which is the line that matters — `{BTC: BUY}` does not, `sell 0.3 BTC at market` does.
Everything downstream is unchanged, including the choice between acting automatically (the risk
gate is the "other checks") and requiring approval (`propose_plan` → `execute_plan`); both already
exist and neither needs a new mechanism.

What it costs is not architecture but **evidence**. A BUY/SELL/HOLD is squarely the judgment
category of §7.3, so its backtest means nothing and its evidence is forward-only (§7.5). That turns
it from something sweepable in an afternoon into something learned about over a quarter. Which is
also why the deterministic baseline is built first: the PM's contribution is only measurable *as a
difference against something*, and that something has to exist and be trusted before the difference
means anything.

### 7.2 Rules for an LLM-derived feature

1. **Bounded, typed output.** A score, label, or extracted value. Schema-validated at the boundary;
   a response that fails validation is a null feature, not a retry-until-it-parses loop.
2. **Materialized, never live.** Compute once per `(input_hash, model_id, model_version)`, store
   it, never recompute. An LLM feature becomes backtestable the moment inference output is treated
   as *stored data* rather than a call — after that the backtest reads a table, and the harness's
   causality test applies unchanged.
3. **Fails neutral.** Inference error, timeout, or invalid output → feature is null → the strategy
   treats null as no-signal → the position is not taken. Never fail toward action.
4. **Earns its slot.** Strategy-with-feature must beat strategy-without out of sample, after
   inference cost. Otherwise it is removed. One feature at a time, each through the same gate.

### 7.3 The contamination problem — read before building any LLM feature

**The leakage is in the weights, not the prompt.** This is the sharp form and it is the one that
matters: feeding a model strictly point-in-time inputs does *nothing* about the fact that it was
pretrained on the window it is being asked to predict. Careful prompt hygiene cannot fix it, and a
system that claims "the agent only saw data available on that date" has addressed the easy half of
the problem and none of the hard half.

This is not a theoretical worry. §11.3 works through a published multi-agent trading result whose
entire test window predates the training cutoff of every model it used, while the paper claims no
look-ahead on prompt-level grounds. There is now a benchmark for measuring the effect —
[Look-Ahead-Bench](https://arxiv.org/pdf/2601.13770).

The partition that follows is not negotiable:

- **Extraction is safe.** "Does this proposal change the emission rate, and to what value?" Knowing
  the future does not help; the answer is in the document. Feed it the document, nothing else, and
  never the date's market outcome.
- **Judgment is contaminated.** "Was this bullish?" is unusable in backtest. Hindsight passes
  straight through.

**Which decides where each layer's evidence comes from:**

| Feature type | Contaminated | Evidence from |
|---|---|---|
| price/volume derived — momentum, vol, liquidity | no; no model involved | backtest, full harness, cheap |
| LLM **extraction** — typed facts out of a document | barely | backtest, with the caveat stated |
| LLM **judgment** — bullish?, BUY/SELL/HOLD | **yes, in the weights** | forward only (§7.5) |

Read that table as an argument for where the strategy's *weight* should sit. The top row is the
only place backtesting is both valid and cheap, which is a reason for the deterministic core to
carry most of the strategy and for the model layer to be a bounded overlay whose contribution is
measured rather than depended on.

**One partial fix worth knowing:** pin a model whose training cutoff predates the test window.
Contamination is then genuinely eliminated for that window, not merely mitigated. The costs are
real — worse model, and a window that shrinks as time passes — but for a one-off "does this feature
have *any* historical signal" it is a legitimate experiment and cheaper than the alternatives.

### 7.4 Evaluating an LLM feature, and what it costs

Materialization (§7.2 rule 2) buys less than it first appears, and being precise about which is
which prevents a development process built on a false expectation.

**What it buys:** sweeps of everything *downstream* of the feature — constructor, sizing,
thresholds, limits — are clean and cheap, because the model layer is held constant while one thing
varies. Plans stay replayable and auditable. And it makes attribution possible at all: with a
reproducible core and a measured overlay you can answer *did the model help*, which you cannot if
everything moves at once.

**What it does not buy:** a free comparison of prompt v1 against prompt v2. A prompt change is a
new `feature_set_version` and a full re-materialization. There is no way around that.

#### The noise floor comes first

Two tables built from the same prompt differ by sampling alone. Until that difference is measured,
a difference between two *different* prompts means nothing.

1. Materialize the same prompt twice → tables A and A′. Backtest both. The gap is the noise floor.
2. Materialize v2 → table B. If `result(B) − result(A)` sits inside that band, the change did
   nothing measurable. Outside it, there is an effect.

So **one prompt change costs three or four full re-materializations**, not one. That is the real
price of a model in the strategy and it should drive the design.

Do this on the *first* LLM feature, before building a second. If the noise floor is wider than any
plausible prompt improvement, the instrument cannot resolve what you are adjusting — and you would
rather learn that in a day than after designing a development loop around it.

#### Granularity is decided by iteration cost, not sophistication

- A daily cross-section over 30 assets × 3 years is ~33k calls per materialization. At 3–4 per
  iteration, the budget binds after two or three prompt revisions. That is not a development loop.
- An event-driven feature — fires on a filing, an unlock, a governance proposal — might be 200–500
  calls over the same history. A re-materialization is minutes, the noise floor is cheap, and
  twenty iterations are affordable.

The case for event-driven LLM features is therefore not purity. It is that event granularity is the
only one at which the prompt can actually be *developed*. A per-bar agent debate is something you
can build once and never meaningfully improve.

### 7.5 Forward testing: what it can and cannot establish

Where backtesting is invalid (§7.3) the evidence has to come from forward testing, and it is worth
being blunt about its limits before building a process on it.

**It cannot establish a modest edge in a useful timeframe.** The standard error on an estimated
Sharpe is roughly `sqrt(1/T)` for `T` years; separating Sharpe 0.5 from zero at two standard errors
is on the order of sixteen years. Hours-to-weeks holds give 25–50 semi-independent periods a year.
No paper-trading run proves an edge, and any claim that one did was measuring something else.

**What it does establish, in weeks:**

- the plumbing — executed-vs-planned, realised-vs-modelled cost, reconciliation breaks
- gross failure — far cheaper to detect than edge
- feature stability — the §7.4 noise floor, the highest information per token available

That is the honest reading of the Phase 2 gate: it proves the *system* works and the strategy is
not obviously broken. It was never going to prove the strategy works.

**Measure the signal, not the portfolio.** Portfolio P&L yields one observation per rebalance.
The rank correlation between a signal and subsequent returns — the information coefficient —
yields one *per asset* per period. Thirty assets over a year is ~1,500 observations instead of ~50:
a 30× speedup on the same calendar time, answering the question that actually matters (does this
rank assets better than chance). Construction and sizing are then evaluated separately in the
deterministic layer where backtesting is valid. IC works on the contaminated layer too, because it
is measured forward.

### 7.6 Worked example: a news agent

The most instructive case, because it fails in four independent ways and the fix is the same one.

1. **Contamination is worse than general.** The model was trained *on news*. Given a pre-cutoff
   article it may have memorised the subsequent coverage of that exact event. It is not inferring,
   it is recalling.
2. **The data problem is bigger than the model problem.** An honest backtest needs the corpus as it
   existed at time T: first-publication timestamps (not crawl dates, which run hours off), original
   text (not the silently-edited current version), and the articles that were later deleted (or it
   is survivorship bias again). A timestamp off by hours is the same class of bug as bar
   open-vs-close — except there is no continuity check to catch it. Trustworthy point-in-time crypto
   news corpora are not purchasable at a sane price.
3. **The mechanism is unstated.** News in liquid majors is priced in seconds to minutes. A daily
   rebalancer is trading the residual after everyone faster has finished. That can be real —
   post-event drift and underreaction are documented — but it is a different claim than "we read
   the news", and it is the claim that would have to be tested.
4. **Often the wrong sensor entirely.** Unlock schedules are on-chain and known months ahead;
   listings are structured exchange announcements; upgrades are governance proposals; exploits show
   in on-chain flows before they are written up. Where a fact is available exactly, reading a
   journalist's description of it is the expensive, lossy, timestamp-ambiguous path to the same
   thing. A news agent should only cover what genuinely has no structured source.

**The version that survives** is the same split as everywhere: let the model read, let the code
decide. Not *is this bullish* but *what happened, as typed facts*:

```
{event: "listing" | "delisting" | "unlock" | "upgrade" | "exploit" | "regulatory_action",
 asset: "SOL", effective_date: "2026-08-14",
 magnitude: {kind: "supply_pct", value: "0.05"},
 confidence: 0.9}
```

Extraction, so contamination barely bites and the output is verifiable against its source. The
mapping from "5% of supply unlocks in three days" to a position adjustment is code, and therefore
backtestable. And it is event-granular, which per §7.4 is the only scale at which the prompt can be
developed.

**One thing worth starting early regardless.** A point-in-time news corpus cannot be bought and
cannot be reconstructed later — but it can be *recorded*. A daily crawl storing headline, body,
source, first-seen timestamp and a content hash, immutable, under the same conventions as the bar
store, costs almost nothing to run. Started now it yields in twelve months something no amount of
money buys today; not started, twelve months from now is exactly where today is. No model goes
anywhere near it — it is a store, a source and an immutability rule. See §10.5.

### 7.7 Non-LLM ML

Gradient boosting over the feature frame for ranking or sizing, regime clustering, volatility
forecasting — legitimate and often where the actual lift is. Caveat: crypto regimes are unstable
and the usable sample is small, so these overfit viciously. Plateau-not-peak and walk-forward apply
double. Not before Phase 4.

---

## 8. Interactive surfaces

### 8.1 CLI first

Everything is a command. The scheduler invokes the same commands cron does; nothing has a private
path into the engine.

```
  crypto-portfolio data-pull|data-inspect|data-verify
  crypto-portfolio features|scores|training-matrix
  crypto-portfolio plan|plan-verify
  crypto-portfolio backtest|sweep|gate|ic|research|report
  bot run|reconcile|halt|pause|resume|flatten
```

Two Rust binaries implement this today. `crypto-portfolio` decides and emits a Plan. `bot` takes a
plan file and does steps 9–10, and owns
`halt | pause | resume | flatten | adopt`. The split is §3.3's security boundary, not a packaging
preference: the process that decides never holds a key that can trade.

```
  bot --config bot.json run --plan plan.json   # execute, across the slice window
  bot --config bot.json status | history | positions | reconcile
  bot --config bot.json halt | pause | resume  --reason R --by WHO
  bot --config bot.json flatten --reason R --by WHO --confirm
  bot --config bot.json adopt   --reason R --by WHO --confirm
```

**Our record is kept apart from the venue's, or the reconcile in §0.6 is theatre.** The bot writes
every order id it authorises to an append-only ledger *before* submitting it. Folding "our"
positions from the venue's own fill log — which is what it did first — puts the venue on both sides
of the comparison, and a check that cannot disagree with itself would never catch a compromised key,
a second process on the account, or a stale order from an earlier deployment.

Connecting to an account that already holds positions is therefore a **halt**, not something
absorbed: adopting whatever the venue reports is exactly the auto-repair the executor refuses to do.
`adopt` is the one sanctioned exception — manual, attributed, recorded as a run, and refusing by
default when there are fills we never authorised. It exists because the alternative is a system that
can never be pointed at a funded account at all.

### 8.2 The dashboard is a lens with hands, and the hands belong to the bot

The `api` process serves an operations dashboard on loopback, laid out as a command centre rather
than a report: what is running and against which account on the left, the book in the middle — NAV,
its curve, the basket, positions, balances, open orders, run history — and the controls, the risk
gate, reconciliation, health and an activity feed down the right.

It carries the full control set — halt, pause, resume, flatten, adopt — and holds **no venue
credentials**. It performs none of them: it invokes the `bot` binary, which owns the key, the gates
and the run record. That is principle 5 taken literally — *every action the system can take is a CLI
command you can run yourself; interactive surfaces are lenses over that CLI, never a dependency of
it.* A button and a shell are the same code path, so there is exactly one implementation of what
"flatten" means and no second one to drift.

The properties that make this safe are worth stating, because "the web page can flatten the book" is
otherwise alarming:

- the argument vector is assembled from a closed list of actions; nothing the browser sends becomes
  a flag, an option or a path, and the only free text is the reason and the operator's name;
- the subprocess is spawned without a shell and with a cleared environment;
- every action requires a name and a reason, passes the bot's own gates, and lands in the run
  history;
- flatten asks the operator to type the word back, being the one control whose effect cannot be
  undone by pressing something else;
- started without `--bot`, the dashboard shows everything and changes nothing — and that is the
  default.

The risk card shows the **planner's own gate** — every limit, what it measured, and whether it held
— carried into the run record rather than recomputed here. A dashboard that derived its own version
of a limit would be a second opinion about the thing that enforces it. Colour follows the money
everywhere on the page except net exposure, which is a deviation from dollar-neutral rather than a
gain, and is coloured as drift.

Live performance is shown with its sample size in the column header, not in a footnote. Below the
threshold the live column is greyed and the Sharpe reads *not yet*: a ratio from a few weeks is
noise with a decimal point, and placing it beside a backtest figure invites the comparison it
cannot support.

### 8.3 MCP later — eyes, not hands

Build it as a thin wrapper once the CLI surface has stabilised through real use. Designing the tool
contract before you know the operations means rewriting it.

**The pattern that makes it safe:**

```
  propose_plan(as_of, overrides?) -> Plan     # pure, no side effects, freely callable
  execute_plan(plan_id, confirm_token)        # takes an ID, never orders
```

There is no `place_order` tool and never will be. The model can compute, diff, and explain plans
without limit, but **its output space does not include an arbitrary order** — execution is a
lookup of something the deterministic engine already produced and the risk layer already cleared.

Split read and write into separate servers. The write server exposes at most `execute_plan`,
`pause`, `flatten`. Read is always available; write is explicit, narrow, idempotent, logged.

**Tool results are data, never instructions.** Configuration lives in versioned files loaded by the
engine — never in a fetchable record whose contents the model is told to obey. A system holding
trading credentials must not take instructions from anything it fetches.

---

## 9. Phased build order

Each gate is stated as evidence. "It's done" is not a gate.

| Phase | Build | **Gate to proceed** |
|---|---|---|
| **0** Skeleton | repo, storage, `DataSource`, `paper` adapter, Plan schema + round-trip test (§3.5), CLI shell | `plan --dry-run` produces a plan from real bars, twice, byte-identical; Rust parses a Python-written Plan in CI |
| **1** Strategy v1 | deterministic rules, feature frame, `scores`, `equal_weight` constructor, cost model, risk layer (§6), full backtest through the harness | positive expectancy after costs; **survives 2× slippage**; walk-forward beats baseline out of sample; sample size adequate or the run says so |
| **1b** Better construction | `RiskModel` (shrunk covariance), `conviction_tilt`, `mvo` | each constructor beats `equal_weight` out of sample, or it is deleted. No constructor ships on elegance |
| **2** Paper | `executor` live on real-time data, `paper` venue, alerting, reconciliation | ≥6 weeks unattended; **plumbing, not edge** (§7.5): executed matches planned, realised cost inside the model's error bars, **zero unexplained reconciliation mismatches**, and results not grossly outside the backtest |
| **3** Live, small | venue selected (§10), real credentials, small fixed capital | 4+ weeks; realised cost within the model's error bars (§6.1); no gate ever bypassed manually |
| **4** First LLM feature | one **extraction** feature, materialized (§7.2); noise floor measured first (§7.4) | noise floor established before any comparison; then beats the Phase-1 baseline out of sample after inference cost. If not — delete it |
| **5** MCP + portfolio view | read surface over this + journal + harness | — |
| **6** Equities | equities `DataSource`, broker adapter, real `Calendar`, equities factor library | Phase 1–3 gates again, independently, for that book |

**Phase 1 is the whole project.** Everything before it is plumbing and everything after is
extension. If Phase 1's gate fails, the correct outcome is a different strategy — or none — not a
softer gate.

**Phase 2's "≥6 weeks" is asserted, not derived.** It is long enough to surface plumbing failures
and far too short to establish an edge (§7.5 does the arithmetic). Flagged rather than quietly
inherited: if that number is ever the thing standing between the system and capital, it deserves
an argument, and it does not currently have one.

### 9.1 What Phase 6 inherits, and what it doesn't

The equities book reuses **everything above the interfaces**: the run loop, the Plan artifact, the
risk layer, both constructors, the cost model, reconciliation, the executor, the reporting, and
every gate. That is the payoff for §4 existing.

What does **not** transfer is the factor library. Momentum survives; book-to-price, Piotroski
F-score, accruals, estimate revisions, Form 4 insider flow and 13F institutional flow have no
crypto analogue, and crypto's own factors (emissions, unlocks, TVL, staking ratio, exchange flows)
have no equity analogue. The *scoring framework* is shared — sub-factors, equal-weight within
parent, group-relative percentile rank, weighted composite, `degenerate_flags`. The factors
themselves are a per-asset-class plugin directory.

Concretely: a mature equities layer looks like scoring → LLM extraction over filings and calls →
covariance-aware construction → hard vetoes → execution → attribution. That is the loop in §3.1
with a different factor directory and a different `DataSource`. It is not a second system, and
building it as one would mean writing the constructor, risk model, cost model, executor and
reporting twice.

Two failure modes worth naming now, because they are the ones that shipped-looking equities
systems tend to have:

- **Parameter surface without a holdout.** Two dozen sub-factors, a composite weight vector, and
  regime-conditional weight switching is an enormous researcher-degrees-of-freedom budget. Every
  one of those constants goes through §9's gates and `crypto_portfolio::validate`'s plateau rule, or it
  does not ship. This is the entire reason Phase 6 repeats the Phase 1–3 gates independently.
- **A scheduled run that computes differently from a full run.** Skipping expensive inputs on the
  daily cron (filings, institutional flow) means live scores are not backtest scores. If an input
  is optional, its absence is a `degenerate_flag` that the backtest can reproduce — not an
  undeclared second code path.

### 9.2 Explicitly not in scope

Intraday/HFT. Options. Cross-venue arbitrage. Market making. Multi-user. Anything custodial for
anyone else. Leverage above 1× before Phase 3 is proven flat-out boring. A UI before Phase 5.

---

## 10. Open questions (unresolved, and how they get resolved)

These are genuinely undecided. Recording them so they are not silently decided by default.

**10.1 Venue — deferred deliberately.** OKX EU is MiCA + MiFID II licensed and deep in spot;
Hyperliquid is self-custodial but perps-first, thin in spot, and unauthorised in the EU, with
precedent (JELLY, March 2025) for validators force-settling positions at a chosen price. Given the
FTX experience, custody exposure is weighted heavily — and note that a hours-to-weeks strategy is
*deployed nearly all the time*, so "sweep idle cash to cold storage" protects the fraction that
matters least.

**This is resolved by evidence, not preference.** Phase 1 runs the universe-breadth sweep: does the
edge survive on the top ~30 liquid names, or does it need the long tail? If it survives narrow,
self-custody is viable and the custody question closes itself. If it only appears in the tail, that
is itself a finding — an edge living exclusively in illiquid small caps is frequently illiquidity
premium rather than alpha, and `validate/sensitivity.py`'s 2× slippage test is the thing that
tells the difference. Run that before concluding breadth is load-bearing.

Until then: `paper` adapter, public data, no account anywhere.

**10.2 Strategy.** Undecided. Channel-breakout trend-following is a legitimate, documented family
and a reasonable *baseline to beat*, not a destination. Whatever v1 is, it clears §9 Phase 1 or it
does not ship.

**10.3 Rebalance frequency.** Daily is the assumed start. Hours-to-weeks holds do not obviously
need daily evaluation, and every rebalance costs spread. Sweep it in Phase 1 — one axis, plateau
centre, not the peak.

**10.4 Tax and reporting (Sweden).** Spot crypto, MiFID-regulated derivatives, and unregulated
offshore perps have materially different treatment, and the venue choice locks in which one is
generated for years. Out of scope for this spec and not a question for a coding agent — resolve it
with a qualified advisor before Phase 3.

**10.5 The news corpus — deferred, but time-sensitive in one direction.** Per §7.6, a trustworthy
point-in-time news corpus cannot be bought and cannot be reconstructed after the fact. It can only
be *recorded forward*. Whether a news feature is ever built is undecided and does not need deciding
now; whether the corpus exists to decide it with is determined by when recording starts.

Shape if started: a daily crawl storing headline, body, source, first-seen timestamp and content
hash, immutable, under the bar store's conventions (§12). No model anywhere near it — a source, a
store and an immutability rule. Roughly Phase-0-sized, and it does not touch the decision path.

The cost of starting is small and recurring; the cost of not starting is that this question looks
identical in twelve months. That asymmetry is the whole argument, and it is the only open question
here where delay is not free.

---

## 11. Prior art reviewed

Three existing systems were read before and during this design. None is a base for this project;
each contributed something, and each demonstrates a failure mode worth naming. Recorded so the
reasoning is not re-litigated later.

The pattern across all three: **operational craft is high, evidence discipline is absent.** Each
is a well-engineered machine with no instrument that measures whether it works. That absence is
what §9's gates exist to prevent, and it is the single largest difference between this design and
the alternatives.

### 11.1 signum.money — LLM-executed rules

Prompt-driven daily rebalancer. An LLM fetches a Gaussian-channel trend detector, evaluates
crossovers across ~100 assets, and places the orders itself.

**Adopted:** its operational hygiene, which is better than most production trading daemons.
Fail-closed on any data fetch; a freshness gate requiring the signal date to be today; a
**completeness gate** (fewer rows than expected → stop, do not evaluate exits) which correctly
identifies that a truncated universe manufactures false "dropped out" exits; exits before entries;
declarative target-state convergence rather than imperative order placement; no re-entry into an
asset exited in the same run. All of these are in §3.2 and §6.4.

**Rejected:** the LLM is doing arithmetic. Every rule is a hard float comparison, so the model adds
nondeterminism and inference cost while contributing no judgment. Compounding problems: no
portfolio-level cap at all (twenty correlated breakouts at 8% NAV each "wants" 160% of NAV, and the
prompt has no answer), no stop between daily runs on a 24/7 market, and — the design flaw worth
remembering — the bot fetches `aiRules` from its own config records and is instructed to obey them,
which makes untrusted fetched data an instruction channel into a system holding trading
credentials. §8.3's "tool results are data, never instructions" is a direct response.

### 11.2 A seven-layer equities stack ("Meridian")

Layered build: data → scoring (8 factors, 27 sub-factors, sector-relative percentile) → LLM
analysis of filings/calls/insider data → portfolio construction (MVO + conviction-tilt) → risk
(Barra-style factor model, pre-trade vetoes, circuit breakers, stress tests) → execution →
reporting.

**Adopted:** four things, all now in this spec — `PortfolioConstructor` as a swappable *and
comparable* interface with a recorded fallback (§4.5); a `RiskModel` whose covariance is an input
to construction and not only a veto (§4.4, §3.1); transaction costs subtracted from expected return
*before* optimisation (§6.1); benchmark-beta as a first-class constraint (§6.2). Its analysis cache
keyed by `(analyzer, ticker, artifact_id)` with TTL is independently the same idea as §7.2's
materialization rule, and its "no Claude analysis available → 100% quantitative, no penalty" is
independently §7.2's fail-neutral.

**Rejected:** no validation layer anywhere in seven layers. The parameter surface is enormous — 27
sub-factors, an 8-weight composite, regime-conditional weight switching on VIX bands, crowding at
0.4, MCTR flag at 1.5×, top-5% gets 1.5× — and nothing in the system could tell you whether any
constant is load-bearing or noise. Also: hardcoded FOMC dates that will go stale silently, and a
daily cron running `--no-filings --no-13f`, meaning scheduled scores are computed differently from
a full run — precisely the backtest/live divergence §0.1 forbids.

### 11.3 TradingAgents (Tauric Research)

[arXiv:2412.20138](https://arxiv.org/abs/2412.20138) ·
[repo](https://github.com/TauricResearch/TradingAgents) ·
[tauric.ai](https://tauric.ai/). A LangGraph firm simulation: four analysts
(fundamentals, sentiment, news, technical) → bull and bear researchers debating for N configurable
rounds → a research manager judging the debate → a trader → three risk perspectives
(aggressive/conservative/neutral) → a portfolio manager approving or rejecting.

**Adopted:** the bull/bear/judge debate is a genuinely good structure — for *research*, in §7.1's
offline hypothesis-generation slot, where its output is "what to test" and the harness still
adjudicates. The aggressive/conservative/neutral split is perspective-diverse verification, which
catches failure modes that N identical checkers cannot; it applies to advisory plan review. Their
reflection log (realised returns fed back as lessons) is the right instinct in an unbacktestable
form — the disciplined version is realised-outcome statistics as a materialised point-in-time
feature, not prompt memory.

Also a third independent arrival at propose/execute: their portfolio manager approves or rejects
the trader's proposal, as signum's approval step and Meridian's approve/reject cards do. Three
unrelated teams landing on the same seam is evidence the seam is real, and it is §8.3's.

**Rejected — and this is the concrete case that §7.3 and §7.5 are built on.** From the paper
itself:

| | |
|---|---|
| Test window | **1 Jan – 29 Mar 2024** (~60 trading days) |
| Universe | **5 large-cap tech names** — AAPL, NVDA, MSFT, META, GOOGL |
| Models | gpt-4o-mini, gpt-4o, o1-preview |
| Transaction costs | **not modelled** — zero occurrences of "transaction cost" or "slippage" in 38 pages |
| Look-ahead claim | *"Agents make decisions based solely on data available up to each trading day, ensuring no future data is used (eliminating look-ahead bias)."* |

Every model used postdates the test window, so all three were trained on data spanning the period
they are "predicting". The quoted claim addresses the prompt and is silent on the weights, which is
exactly the distinction §7.3 turns on — and it is why prompt-level point-in-time discipline cannot
be accepted as evidence of no look-ahead.

Independently of contamination, the result would not clear this project's Phase 1 gate: five assets
over one quarter is a sample the harness would flag before reporting any number; a single
three-month window is not walk-forward; and a strategy with no cost model has not been shown to
survive costs, let alone 2× slippage. The period was also strongly favourable for those specific
names, so the benchmark comparison carries most of the burden — and the baselines are the part a
reader cannot re-run.

None of that makes the architecture uninteresting. It makes the *number* uninformative, which is a
different and more common failure.

---

## 12. Conventions that matter

- **`ts_utc` is always the bar's OPEN**, timezone-aware. Sources stamping the interval end convert
  at the adapter boundary and never again. Getting this wrong shifts every bar by one interval and
  hands the strategy a free look at the future — the most damaging bug available here, and a silent
  one.
- **Bars are immutable.** Fix the source, not the store.
- **Point-in-time universe.** `universe_members` records what was eligible *on that date* and is
  never backfilled. Backfilling it is survivorship bias with extra steps — the delisted, the
  rugged, and the dead must remain in history exactly as they were.
- **Money is decimal, never float.** Sizes, prices, fees, NAV.
- **Contract artifacts are written as bytes, never as text.** `Path.write_bytes`, never
  `write_text`. Text mode translates newlines per platform *and* translates them back on the way
  in, so a round-trip test passes on the very machine producing the wrong bytes — which is how a
  CRLF Plan fixture got committed and would have failed CI on its first run. The fixture test
  compares bytes and asserts no `\r`; `.gitattributes` marks the fixture and schema `-text` so git
  never rewrites them either.
- **Every number in a report carries its `n`**, and an inadequate sample says so before the number,
  not in a footnote.
- **Report what was not enforced, first.** Inherited from the harness: any run with a declared but
  unenforced rule states that above its results, so no number is read as more complete than it is.
