# futures-noise — the NQ four-sleeve book as an ai-trader bot

**Rust end to end** (operator mandate 2026-08-06): the decision core is
`service/crates/noise-book` (scanner machines, trail management, session
runtime, kill rail) on `service/crates/features-cme` (session framing +
the bar-feature catalog — the ONE implementation both training and the
runtime link; models select features by catalog name). The binary is
`service/crates/futures-bot` (`replay`, `features`, live IB via
rust-ibapi next). Acceptance gate: `./run.sh parity` must reproduce the
committed `parity-fixture.json` — fill counts and net dollars per sleeve,
exactly. The Python implementation in trading-journal remains the lab's
reference (it regenerates the fixture after an INTENDED strategy change
via `./run.sh parity-py`); it no longer runs in production.


Bot #2 (docs/futures-bot-proposal.md). The DECISION CORE does not live in
this repo: it is the parity-proven engine in `trading-journal/backtest`
(scanners, position management, feeds, runtime), consumed as a path
dependency. This directory is the OPERATIONAL binding: identity, config,
state location, and the parity gate that proves the wiring reproduces the
validated backtest before any live bar is trusted.

## One code path, enforced

* The runtime here IS `backtest.bot.BookRuntime` — not a port, an import.
* `parity_check.py` replays a fixed window through the full stack and
  compares against `parity-fixture.json` (committed). Any drift — in this
  repo's wiring OR in the journal repo's engine — fails the check. Run it
  after touching either side:

      ./run.sh parity

## Running

    ./run.sh replay              # stored bars through the whole stack
    ./run.sh parity              # the gate (Rust vs committed fixture)
    ./run.sh ib-check            # Gateway readiness probe (run when the
                                 #  market-data subscription activates)
    ./run.sh shadow              # LIVE bars, no orders ever: the Book
                                 #  simulates fills and publishes to the
                                 #  records DB (fill-model calibration)
    ./run.sh live                # arms per .env flags; mirrors the book
                                 #  with market orders; reconciles at
                                 #  session boundaries, halts on mismatch

State publishes to the shared records DB (step 4): the heartbeat document
to `bot_status`, fills to `fills` (content-keyed, idempotent — replays and
crash-recovery reruns never double-insert), the crash-recovery snapshot to
`snapshots`, and controls are read from the newest `control_events` row —
the same tables every bot in the fleet reports to, and the only store that
survives the pod. `run.sh` sources `.env` for DATABASE_URL; without it the
old `var/futures/state/botstate/` file contract remains as announced dev
fallback (the parity gate forces it, hermetically). Identity: `bot_id =
futures-noise`, registered and DB-gated like every bot (fail closed when
DATABASE_URL is set).

## Venue

`venue_id = ib`. The IB adapter (ib_async executor with two-flag arming,
reconciliation rail, ib-check probe) lives with the decision core in
trading-journal/backtest/src/backtest/bot/venue.py (a §3.5-style
"credentials live where the work is" judgment); the registry's `ib` venue
row already exists and `open_live("ib")` refuses with a pointer here.
