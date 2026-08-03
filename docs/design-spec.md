# AI Trader — Design Spec

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
2. **No capital before evidence.** A strategy reaches live money only after it has cleared the
   backtest harness (positive expectancy after costs, survives 2× slippage, survives walk-forward)
   *and* paper-traded within the backtest's error bars. Both gates. In that order.
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

## 2. Reused, not rebuilt

`trading-journal/backtest/` is the most valuable existing asset for this project and it is already
a standalone Python package that the journal image never touches. It carries:

- the **causality guarantee** — build features over full history, rebuild over every prefix,
  require row *i* identical both times. A feature is causal exactly when deleting the future
  doesn't change it. This is the single most important test this project will inherit.
- `engine/causal.py` — a cursor that **raises** on lookahead rather than discouraging it.
- `validate/` — holdout + walk-forward splits by session, one-axis sweeps that report **the
  plateau's centre, never the peak**, cost and intrabar sensitivity error bars, and the
  disclosure-first report ordering (what was *not* enforced, before any number).

**Do not extract it into a shared package yet.** Depend on it in place (path/git dependency) until
this repo is a real second consumer with real requirements. Pre-factoring against an imagined
consumer produces the wrong interface. Extraction is a Phase 4+ decision.

What must be adapted, not copied: the harness is session-anchored to `America/New_York` with a
trade date rolling at 18:00 ET. Crypto is 24/7 with no sessions. The **calendar is an interface**
(§4.3), and the crypto implementation is "always open, UTC days". Equities later reuses the
harness's existing session logic almost verbatim.

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
  7. DIFF        target state - actual state = the minimal set of orders
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

**That preference is honoured everywhere it doesn't break §0.1.** It cannot be honoured
*everywhere*, and the reason is worth stating rather than quietly compromising:

| Layer | Language | Why not the other one |
|---|---|---|
| `api`, frontend | **Rust + React/TS** | Directly the journal's pattern. No reason to differ. |
| `executor`, venue adapters | **Rust** | A long-running process holding trade credentials. Explicit error types, no GIL, no silent `None`/float surprises, real retry and idempotency discipline. Rust is *better* here, not merely acceptable. |
| `planner` (steps 1–8), backtest, validation | **Python** | scipy/cvxpy for MVO, pandas/numpy for the feature frame, statsmodels for cross-sectional regression, and the Anthropic SDK. No mature Rust equivalent for the optimiser or the harness. |

**The decisive constraint is §0.1, not library convenience.** The research path and the live path
must be *one* implementation of steps 1–8, or the Phase 1 gate measures nothing. The backtest
harness is Python and is reused in place (§2). Therefore the planner is Python — because the
alternative is two implementations of the strategy, which is the failure mode this whole document
exists to prevent. Rewriting steps 1–8 in Rust means rewriting the harness too, and then
maintaining the *equivalence* of two numerical stacks forever.

**The Plan is the process boundary**, and it already had the right shape for this: immutable,
schema'd, risk-cleared, persisted, and addressed by id (§5.3, §8.2). Rust never needs to know how
a plan was computed — only that it exists, validates, and hasn't been superseded.

```
  cron -> planner (Python)  --writes Plan row-->  Postgres  --reads plan_id-->  executor (Rust)
```

**Contract discipline, since this is a cross-language seam:**

- The Plan schema is the contract. Define it **once** (JSON Schema), generate `serde` types and
  Python models from it. Hand-maintained parallel structs will drift, and the drift will be
  discovered in production.
- A round-trip test in CI: Python writes a Plan, Rust parses it, values compare equal. This test
  failing is a release blocker, not a warning.
- Version the schema. `executor` refuses a Plan whose schema version it does not know — fail
  closed (§0.3), never best-effort parse.

**Two things named `paper`, deliberately kept distinct** — conflating them silently voids Phase 2:

| | Where | What it is |
|---|---|---|
| `sim` fill model | Python, inside the backtest | simulates fills against historical bars: ±1 tick, commissions, pessimistic stop fills, stop-before-target |
| `paper` venue | Rust, a `VenueAdapter` impl | a fake broker on live real-time data — exercises the **real** execution, reconciliation and error-handling path |

Phase 2's gate is about the operational path, not the strategy. Running it through the Python
simulator instead of the Rust executor would test nothing that Phase 1 hasn't already tested.

**Packaging:** two images (`planner`, `service`) rather than the journal's single container, both
deployed by Flux into the same namespace. This is a real cost of the split — two toolchains, two
CI jobs — and it is accepted knowingly in exchange for not maintaining two strategy
implementations.

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
discloses it above the numbers (§11).

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
| max per-run turnover | % NAV; catches a runaway signal producing churn |
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

### 6.3 Gates that stop the run

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
| research hypothesis generation | **no** — offline | proposes features; the harness kills the bad ones. Highest EV, zero runtime risk |
| plan review / anomaly flagging | **no** — advisory | flags, never blocks, never modifies |
| post-trade narration | **no** — after the fact | the "why" record; the AI-side analogue of the journal's judgment block |

Explicitly excluded: choosing assets, choosing sizes, evaluating a numeric threshold, or
constructing an order. Those are arithmetic. Code does arithmetic.

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

**Current models have seen the future.** Ask one to assess a 2024 announcement and it knows how
that played out. The leakage is invisible, produces no error, and makes a backtest look
extraordinary.

This partitions the use cases and the partition is not negotiable:

- **Extraction is safe.** "Does this proposal change the emission rate, and to what value?"
  Knowing the future does not help; the answer is in the document. Feed it the document, nothing
  else, and never the date's market outcome.
- **Judgment is contaminated.** "Was this bullish?" is unusable in backtest. Hindsight passes
  straight through. If you want a judgment-shaped feature, **only forward-tested results count** —
  paper it for months and disregard the backtest entirely.

Build extraction; keep interpretation in the deterministic layer downstream. The holdout split is
the tool that catches a contaminated feature: impossible in-sample, dead out-of-sample.

### 7.4 Non-LLM ML

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
  ai-trader data pull|inspect|verify
  ai-trader features build [--as-of]
  ai-trader plan [--as-of] [--dry-run]        # steps 1-7, prints the plan + risk report
  ai-trader execute <plan-id>                 # step 8-9, requires an existing plan
  ai-trader reconcile
  ai-trader backtest [...]                    # via the harness
  ai-trader validate [...]                    # holdout, sweeps, walk-forward
  ai-trader pause | resume | flatten
```

### 8.2 MCP later — eyes, not hands

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
| **2** Paper | `executor` live on real-time data, `paper` venue, alerting, reconciliation | ≥6 weeks unattended; paper results inside the backtest's error bars; **zero unexplained reconciliation mismatches** |
| **3** Live, small | venue selected (§10), real credentials, small fixed capital | 4+ weeks; **realised cost within the model's error bars** (§6.1); no gate ever bypassed manually |
| **4** First LLM feature | one extraction feature, materialized (§7.2) | beats the Phase-1 baseline out of sample, after inference cost. If not — delete it |
| **5** MCP + portfolio view | read surface over this + journal + harness | — |
| **6** Equities | equities `DataSource`, broker adapter, real `Calendar`, equities factor library | Phase 1–3 gates again, independently, for that book |

**Phase 1 is the whole project.** Everything before it is plumbing and everything after is
extension. If Phase 1's gate fails, the correct outcome is a different strategy — or none — not a
softer gate.

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
  one of those constants goes through §9's gates and `validate/sweep.py`'s plateau rule, or it
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

---

## 11. Conventions that matter

- **`ts_utc` is always the bar's OPEN**, timezone-aware. Sources stamping the interval end convert
  at the adapter boundary and never again. Getting this wrong shifts every bar by one interval and
  hands the strategy a free look at the future — the most damaging bug available here, and a silent
  one.
- **Bars are immutable.** Fix the source, not the store.
- **Point-in-time universe.** `universe_members` records what was eligible *on that date* and is
  never backfilled. Backfilling it is survivorship bias with extra steps — the delisted, the
  rugged, and the dead must remain in history exactly as they were.
- **Money is decimal, never float.** Sizes, prices, fees, NAV.
- **Every number in a report carries its `n`**, and an inadequate sample says so before the number,
  not in a footnote.
- **Report what was not enforced, first.** Inherited from the harness: any run with a declared but
  unenforced rule states that above its results, so no number is read as more complete than it is.
