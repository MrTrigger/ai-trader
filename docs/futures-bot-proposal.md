# Proposal: adopt the NQ futures bot as a second bot in ai-trader

Status: PROPOSAL — written 2026-08-06 from the trading-journal side, as the
coordination handshake before any code moves. Another agent works in this
repo; nothing here is built yet.

## Why (the operator's call, 2026-08-06)

trading-journal has drifted into three things: the journal, a research lab,
and — recently — a futures bot host (runtime, IB venue layer, dashboard,
recovery). The operator wants all bots running in one place with one UI.
This repo's own §1 logged decision agrees: the journal records *a human's*
discretionary trades; code-driven always-on trading belongs here. The two
repos have also begun building the same machinery twice (this repo's recent
"command centre" dashboard and "bot record / reconnect" commits vs the
journal repo's Bot tab, state contract, and snapshot-resume — convergent,
redundant).

## What would move in (all committed, parity-proven, in trading-journal)

* **The strategy**: the four-sleeve NQ noise-area book (breakout + pullback
  twins on RTH and Globex-open frames, theta 0.1, flat 15:40 ET). Evidence:
  16 years of data, gross edge persistent in 14/16 years, fold-gated
  adoptions, ~30 recorded audition verdicts (`backtest/lab/sleeve_screens.py`
  in trading-journal is the ledger). CAGR on required capital 35.5% vs S&P
  31.5% at the $150k/unit dial, maxDD -41.8% vs -65.4% (compounded).
* **The runtime**: the engine's session loop made incremental, driven by
  completed 5-min bars; replay parity is EXACT against the backtest
  (trade-for-trade), and crash-recovery parity is exact (kill mid-session
  with an open position, resume, identical results).
* **The IB venue layer**: ib_async executor with two-flag arming
  (paper: allow_orders; live: + allow_live), always-permitted flatten_all,
  broker-vs-model reconciliation; `ib-check` readiness probe; live feature
  path (per-slot sigma prep + incremental VWAP/ATR) verified equal to the
  batch frames at float precision. Only the realtime socket loop awaits an
  active market-data subscription.
* **The ops contract**: state/control/journal/snapshot documents (currently
  files; this repo's Postgres-backed bot-record pattern is the better prod
  home and matches what its recent commits already built).

## What stays in trading-journal

* The journal itself (returns to its scoped purpose: human trades).
* The research lab: data store (16y NQ/ES 1-min + GC/ZN + statistics),
  backtest engine, rules, the audition ledger, the Backtest tab. It remains
  the evidence factory and the reference implementation the bot must match.

## Fit with this spec — and the three decisions it forces

1. **Not through the portfolio planner.** The futures bot decides on 5-min
   bar closes; the planner's hours-to-weeks Plan-row cadence doesn't fit.
   Proposal: a separate always-on bot service under shared ops/UI/DB —
   "all bots in one place" ≠ "all bots through one pipeline". The §0
   principles (deterministic, fail-closed, reconcile-always, append-only
   record, no LLM in the money path) are already implemented natively.
2. **Executor language.** §3.5 puts credentials in a Rust executor; the
   proven IB path is Python ib_async, and Rust IB client libraries are
   immature. Options: (a) accept a Python executor for THIS bot with the
   same credential isolation (a §3.5-style "where the work is" judgment);
   (b) port to a Rust IB adapter (cost: rewriting a working, defensive
   executor against a weak library ecosystem). Proposal: (a), revisit at
   Phase 6 equities.
3. **One code path across repos.** The bot must keep importing the
   validated decision core (engine scanner + management) that lives in
   trading-journal. Proposal: pin it as a dependency (path/git pin) and add
   a CI parity job mirroring this repo's cross-language contract test:
   replay N stored sessions, require exact match against a committed
   fixture. Drift fails CI, same honesty mechanism as plan.schema.

Open question for the operator: do bot fills also flow to the journal?
This repo's §1 says the journal is human-only (implying: bot fills live in
ai-trader's fills log exclusively); the journal repo's bot-spec assumed
real fills join the journal. One of the two must be re-logged.

## Suggested phasing (each gated, per this repo's custom)

P1  This proposal accepted/amended by the operator and this repo's agent.
P2  Scaffold `bots/futures/` service; import the decision core; replay
    parity green in CI against the trading-journal fixture.
P3  Ops integration: bot record tables, command-centre dashboard entry,
    control plane (halt, per-sleeve switches, sizing dial $150k/unit).
P4  Shadow on live IB data (subscription pending on the operator's side);
    fill-model calibration vs simulator; then paper -> 1 MNQ by the
    journal-side bot-spec's ladder and kill criteria (rolling 60-session
    net < -$70.5k halts).

Reference material in trading-journal: docs/bot-spec.md,
docs/research-plan.md, backtest/src/backtest/bot/ (runtime, feed, venue,
state), backtest/lab/sleeve_screens.py (the verdict ledger).
