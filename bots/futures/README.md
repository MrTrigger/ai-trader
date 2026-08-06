# futures-noise — the NQ four-sleeve book as an ai-trader bot

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
    ./run.sh shadow              # live-data readiness loop (IB gateway)
    ./run.sh parity              # the gate

State publishes to `var/futures/state/botstate/` (state.json heartbeat,
journal.jsonl fills, control.json, runtime snapshot) — the same contract
the journal's dashboard used; ai-trader's multi-bot UI reads it via the
registry's state_dir column. Identity: `bot_id = futures-noise`, registered
and DB-gated like every bot (fail closed when DATABASE_URL is set).

## Venue

`venue_id = ib`. The IB adapter (ib_async executor with two-flag arming,
reconciliation rail, ib-check probe) lives with the decision core in
trading-journal/backtest/src/backtest/bot/venue.py until step 4 moves
operational records to Postgres; the registry's `ib` venue row already
exists and `open_live("ib")` refuses with a pointer here until then.
