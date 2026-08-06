# Architecture review: from one crypto book to a multi-bot platform

Status: REVIEW + PROPOSAL, 2026-08-06. Requested by the operator: assess
whether the architecture cleanly separates data providers, bot engines,
execution, venues and accounts so that vastly different bots (crypto,
futures, stocks; different brokers and accounts) plug into one framework.
Companion to `futures-bot-proposal.md`. Grounded in a full code survey,
not just the spec.

## Verdict in one paragraph

The foundations are genuinely right and worth keeping: the immutable Plan
seam with a CI-enforced cross-language fixture; the decider/executor/api
key-separation; venue-agnostic `executor`/`runner`/`paper` crates; the
append-only ledger and unknown-fills reconciliation; fail-closed controls.
But the system is today a SINGLE-bot, SINGLE-account, SINGLE-venue
appliance: there is no bot/account/venue identity anywhere in the types,
"Live" literally means Hyperliquid, the venue types cannot express margin,
multipliers, expiries, sessions or stop orders, persistence is per-directory
JSON files rather than the spec's own §5.2 tables, and the dashboard is one
process per book hardwired to the crypto strategy's parameter names. None
of this is wrong for Phase 1; all of it blocks the multi-bot goal. The
required change is structural but additive, and cheapest NOW while N=1.

## What the survey found (facts, with locations)

1. **No identity dimension.** No `bot_id`, `account_id` or `venue_id` on
   `Plan`, `RunRecord`, ledger `Entry`, `Fill`, `Position`, or controls.
   Multi-bot today = N hand-wired `state_dir` trees + N `api` processes on
   N ports, coordinated by nothing (`var/demo`, `var/live` are already this
   pattern).
2. **Sharing an account is actively dangerous today.** `Ledger::reconcile`
   folds ALL ledger entries against ALL venue positions — a sibling bot's
   positions become `unadopted`, its fills `unknown_fills` → permanent
   halt; worse, `Runner::flatten` closes EVERYTHING on the venue, so
   flattening bot A liquidates bot B. Until identity lands, the rule must
   be: one venue account per bot, no exceptions.
3. **Authority and venue identity are conflated.** `Mode{Paper |
   LiveReadonly | Live}` where the `Live*` arms construct `Hyperliquid`
   directly; `Active` is a closed 2-variant enum; credentials are `HL_*`
   fields on one `Env` struct; five string-branches on "hyperliquid" sit in
   `bot`/`api` — the exact "never branch on venue name" violation §4.1
   forbids, in the two processes the spec calls venue-agnostic.
4. **The type vocabulary is crypto-shaped.** No margin (initial/
   maintenance), no contract multiplier, no expiry/rolls, no sessions
   (spec §4.3 `Calendar` was never built — "crypto never closes" is baked
   into `usable_horizon` and `bars.py`), `OrderType` is Market|Limit only
   (no stops), no partial fills, and `Capabilities.max_leverage`/`funding`
   are written by adapters and read by nobody (write-only capability
   mechanism). An NQ future is unrepresentable end to end.
5. **One hardcoded pipeline with two plug slots.** `pipeline.run()` is a
   single fixed 8-step function; only SIGNAL and CONSTRUCT are pluggable
   (closed instance-tuple registries). Cadence is cron-daily via
   `bin/cycle.sh`. The futures bot's 5-minute bar-close loop cannot and
   should not go through it.
6. **The spec's data model was never built.** `schema/` holds only
   `plan.schema.json`; §5.2's table sketch (assets with `asset_class`,
   plans, orders, fills, positions, runs, NAV, alerts) has no SQL, no
   migrations, no driver dependency. Persistence is JSON files per
   `state_dir` — plus a SECOND position truth (`data/book.json` on the
   planner side) that is not the ledger.
7. **No real frontend.** README's React/TS stack is aspirational; the
   dashboard is a 1,222-line `include_str!` HTML, localhost-only (good),
   one process per book, with the Strategy card filtering on a literal
   allowlist of the crypto ranker's parameter names.
8. Hygiene: `config/bot.example.json` and `var/demo/bot.json` carry a
   stale `"venue"` key that no longer parses under `deny_unknown_fields`;
   `bot feed` constructs the Hyperliquid Info client unconditionally.

## Target shape: one framework, one control plane, N engines

"One pipeline" cannot literally mean one strategy pipeline — a
target-portfolio rebalancer, a 5-minute futures book and a stock swing bot
share no SIGNAL/CONSTRUCT semantics. What they CAN share is everything
else. The invariant lifecycle every bot already follows is:

    OBSERVE (gated) -> DECIDE (bot-specific) -> RISK-GATE -> PLAN
    -> EXECUTE -> RECONCILE -> RECORD/REPORT

Make THAT the contract, and make the platform own everything but DECIDE:

```
platform/
  identity     bots, venues, accounts, bindings (bot x account x scope)
  data         DataSource registry (binance, hyperliquid, databento, ib),
               the parquet bar store (shared), Calendar registry
               (always-open | CME | XNYS), live feeds emitting completed bars
  execution    VenueAdapter registry keyed by venue_id (paper, hyperliquid,
               ib, ...); asset-class-capable types; the existing executor/
               runner/ledger, namespaced per (bot, account)
  risk         per-bot budgets (exists, in the plan gate) + a NEW global
               cross-bot admission gate at the executor (aggregate margin,
               account-level exposure, global kill)
  records      Postgres (the §5.2 sketch, plus identity columns everywhere)
  control      ONE api process over the shared records: bot registry,
               per-bot dashboard + controls, global halt
bots/
  crypto-portfolio/   today's planner, unchanged in substance = bot #1
  futures-noise/      the NQ four-sleeve book = bot #2
```

A `Bot` is then: an id, a decision core in any language that emits schema'd
Plans, a declared cadence (`cron | bar_close(market, interval) | stream`),
data subscriptions, an account binding, a risk budget, and its controls.
The Plan stays the universal seam — it needs `bot_id`, `account_id`,
`venue_id` and (for futures) contract-aware order fields; a minor-version
schema bump plus the existing round-trip CI covers both languages at once.
Notably the futures bot fits target-state diffing naturally: at each bar
close its desired state per sleeve (+1/-1/0 x units) IS a target; DIFF and
idempotent convergence apply unchanged.

## Deployment constraint (operator, 2026-08-06)

ai-trader will later deploy to the **triggerlab cluster** alongside the
journal. That settles the storage question up front: identity is DB-FIRST,
not files-then-migrate. The registries — `bots`, `venues`, `accounts`,
`account_bindings` (bot x account x credential scope) — are Postgres tables
from step 1, and they are the authoritative source the control plane,
executor and dashboards read. Per-`state_dir` JSON survives only as a
dev-mode state backend behind the same interface; identity itself is never
file-resident. Credentials stay in SOPS/env as today — the DB stores
credential *references* (which scope, which secret name), never secrets.

## Sequenced migration (each step cheap now, expensive later)

1. **Identity first, in Postgres, while N=1.** Stand up the DB with the
   four registry tables above; add `bot_id`/`account_id`/`venue_id` to
   Plan (schema bump), RunRecord, ledger entries, controls; prefix
   `client_order_id` with the bot id (kills the collision + unknown-fills
   hazard); scope `flatten` to the bot's own ledger positions, never the
   whole venue.
2. **Split authority from venue.** `Mode` keeps Paper|ReadOnly|Live;
   venue selection becomes data (`venue_id` -> adapter registry,
   `Box<dyn VenueAdapter>` or a registry fn); credentials move to
   per-venue namespaced env/SOPS (`VENUE_<ID>_*`); delete the five
   "hyperliquid" string branches from `bot`/`api`.
3. **Grow the vocabulary.** `asset_class`, contract `multiplier`, `expiry`,
   margin fields on Balance/Market, stop/stop-limit order types, partial
   fills; implement `Calendar` (§4.3) with always-open, CME and XNYS
   implementations; make capabilities READ (executor refuses what the
   market can't do).
4. **Operational records to Postgres** (identity tables exist since step
   1): build the rest of §5.2 with identity columns on every row;
   ledger/runs/fills/NAV move from per-dir JSON to shared tables;
   `state_dir` files remain a dev-mode backend behind the same interface.
   Retire `data/book.json` — positions derive from fills, as §0.7 already
   demands.
   *LANDED 2026-08-06*: migration `0003_operational.sql` (runs,
   ledger_entries, fills, control_events, bot_status, snapshots,
   venue_sim_state — every row keyed by bot_id, the bot's own document
   verbatim as jsonb next to indexed columns). Runner's
   Ledger/RunStore/ControlStore and the futures bot's state store are
   dual-backend: DATABASE_URL → Postgres (no silent fallback — an
   unreachable DB refuses, fail closed), unset → files, loudly, for dev
   and the hermetic parity gates. One control contract for every bot:
   halt/resume appends a control_events row; absence means halted.
   Verified: replay parity, crash/resume parity through the DB (exact,
   with journal rehydration), halt→bot-reads-it→resume audit trail.
   Deployment note (operator): ONE pod, N bot processes + one api, all on
   the cluster DB; `var/` holds only caches. Still open from this step:
   retire `data/book.json` on the planner side.
   *ADDENDUM (same day)*: the reporting contract is CANONICAL, not
   per-family. Status envelope (schema 1): `{schema, kind, mode, state,
   state_reason, headline, detail}` — the fleet renders the envelope
   uniformly, `detail` belongs to the bot's own page. Control document
   (schema 1): `{schema, state: halted|paused|running, reason, set_by,
   set_at, overrides}` — unknown states read as halted; bot-specific
   overrides survive halt/resume cycles. Both are owned by the records
   crate; Python conforms as a client. The api now runs with no arguments
   as the standalone fleet control plane (wraps no bot).

## Language policy (operator, 2026-08-06)

Rust owns the platform: identity, the records schema and every contract,
executor/runner, venues, control plane. Python needs a VERY GOOD reason
and is only a client of Rust-owned contracts. The two standing
exceptions: (1) parity-gated decision cores whose validated research
artifact is Python (the futures book, the crypto planner per §3.5);
(2) — RETIRED: the "Rust IB clients are immature" claim was re-checked
2026-08-06 and no longer holds (`ibapi` crate v3, async+blocking,
protobuf wire, actively maintained; also `yatws`). The IB venue adapter
is therefore planned as a Rust `VenueAdapter` behind `open_live("ib")`,
removing Python from the money path entirely; ib_async remains only
until that adapter lands and passes the arming/reconcile checks.

*Exception (1) is ALSO retired for the futures bot* (operator, same day:
"migrate the futures bot to rust as well" + "you must implement all
features in rust so there is no parity creep between training and
runtime" + "a common features crate" + per-instrument-type scoping +
per-model feature selection). Landed: `features-cme` (CME session
framing + the feature CATALOG — stable names, models declare their
subset, training exports and runtime inference resolve through the same
accessors), `noise-book` (the four-sleeve decision core), `futures-bot`
(replay/features binary, records-DB publishing). The Rust port
reproduces the committed parity fixture exactly — fill counts and net
dollars per sleeve on two independent windows. Python's remaining roles:
the research lab (reference implementation + fixture regeneration) and,
at most, training orchestration over Rust-computed features. *ALL LANDED same day* ("complete everything"): the `ib` crate implements
`VenueAdapter` over rust-ibapi (two-flag arming, always-permitted
flatten_all, front month resolved via contract_details never assumed,
lazily-connecting `IbLazy` behind `open_live("ib")`); `futures-bot run`
is the live loop (5s→5min aggregation, controls re-read every bar,
feed-stall watchdog → flatten+halt, broker-vs-model reconciliation at
session boundaries → mismatch flattens and halts, snapshot every bar
batch) with shadow (never armed) and live (mirrors Book transitions)
modes; `ib-check` is the Gateway readiness probe. Crash-recovery parity
re-proven for the Rust stack: freeze mid-session + resume from the DB
snapshot converges to the identical book. Backtest equivalence proven on
a third independent window (2026-04-15→, 274 fills: Python == Rust on
every sleeve, to the cent). Remaining before live money: run `ib-check`
against the operator's Gateway (needs the market-data subscription),
then the shadow month per the bot-spec ladder. Gap on restart mid-
session: live bars between crash and resume are not yet backfilled from
IB historical data — noted in the run loop, acceptable for shadow.
5. **One control plane.** Single `api` over the records DB; bot registry
   page; per-bot dashboard rendered from the bot's declared metadata (not
   a parameter allowlist); global + per-bot halt. Then, per README's own
   intent, the real React/TS frontend.
6. **Land bot #2** (futures-noise per `futures-bot-proposal.md`): IB
   VenueAdapter + CME Calendar + Databento/IB DataSource + the ported
   runtime as the decision core, consuming the validated engine from
   trading-journal via a pinned dependency and a replay-parity CI fixture.

Steps 1-2 are days, not weeks, and remove the only actively dangerous
behaviours. Step 6 is where the futures bot arrives — after the platform
can name it.
