# Running it

Three processes, none of which knows about the others at runtime. They meet through files.

```
  planner (Python)          bot (Rust)                  api (Rust)
  read-only venue key       trade-scoped key            no key
  decides                   executes                    shows
        │                        │                          │
        └──── plan.json ────────►│                          │
                                 ├──► runs/*.json ─────────►│
                                 ├──► venue-state.json ────►│
                                 │◄─── controls.json ◄──────┘
```

The arrow that matters is the last one. The dashboard writes controls and reads everything else;
it cannot place an order because it holds nothing that could. See design spec §3.3 and §8.2.

## The daily loop

```bash
# 1. Data and marks. Whatever holds the price feed writes state_dir/marks.json.
ai-trader --config config/default.toml data pull

# 2. Decide. Emits a plan; no side effects, no venue key that can trade.
ai-trader --config config/default.toml plan --out var/bot/plan.json

# 3. Execute. Runs the slice window, then exits.
bot --config config/bot.json run --plan var/bot/plan.json
```

Step 3 is the only one that can move capital, and it is one process per run: cron starts it, it
does one thing, it exits. Everything it cares about is on disk before it goes.

The exception is the execution window itself — `run` stays alive across its slices, because that
*is* the run. With the default schedule the first slice goes out an hour after the plan's `as_of`
and the second an hour after that, so the process lives for roughly two hours. Cron should not
start the next one on top of it; the freshness gate and the already-executed gate will refuse a
double-run, but refusing is not a schedule.

## Watching it

```bash
api --state-dir var/bot --initial-cash 30000 \
    --expectation docs/research/backtest.json
```

Then <http://127.0.0.1:7434>. Loopback only, with no flag to change that.

Everything on the page is folded from the fill log, which is the only stored truth. There is no
cached NAV anywhere, because a cached NAV is a second opinion about the account with no way to
tell which one is right — and the wrong one looks exactly as authoritative on a dashboard.

The same numbers without a browser:

```bash
bot --config config/bot.json status      # health, controls, exit code 1 if anything is off
bot --config config/bot.json history     # recent runs, newest first
bot --config config/bot.json positions   # what the venue says we hold
```

`status` carries its answer in the exit code as well as the JSON, so a monitoring script does not
have to parse anything to know something is wrong.

## Stopping it

| | What it does | Where |
|---|---|---|
| **halt** | Plans, never executes. The strongest stop. | dashboard **or** CLI |
| **pause** | Only orders that reduce exposure go out. A paused book can still get out. | dashboard **or** CLI |
| **resume** | Permits trading again. | CLI only |
| **flatten** | Closes every open position at market, now. | CLI only |

```bash
bot --config config/bot.json halt    --reason "drawdown breach" --by magnus
bot --config config/bot.json resume  --reason "reviewed, cause understood" --by magnus
bot --config config/bot.json flatten --reason "weekend risk" --by magnus --confirm
```

A reason and a name are required every time, including from the dashboard. The person who finds
the bot stopped at 3am is usually not the person who stopped it, and "halted" without "why" invites
the worst available response, which is to clear it and see.

**Halt does not close anything.** It stops the system taking on new risk and does nothing about the
risk already on the book. Getting flat is `flatten`, which works even while halted — the moment you
most want to stop trading is usually the moment you most want to be out.

### Why the dashboard cannot resume

Halting and pausing only ever reduce what the system may do, so they should be one click away.
Resuming grants trading authority and flattening moves capital. Neither belongs one mis-click away
in a browser tab that has been open since Tuesday, so both require a terminal — which in practice
means a human who has read *why* it stopped before restarting it.

## Failing closed

- **No control file, or an unreadable one, means halted.** A deleted file, a failed disk or a
  botched deploy costs opportunity, not money.
- **A stale plan is refused.** Past `max_plan_age_minutes` it was computed against prices that no
  longer exist.
- **A plan that already ran is refused.** Re-running it would submit against a book that has moved,
  and none of the order ids would collide because the quantities differ.
- **Reconciliation drift halts and is never repaired.** Our fill-log positions against the venue's;
  a mismatch means one of our assumptions is wrong, and trading on top of it turns a bug into a
  loss.
- **A failed order stops the run** with the remaining orders named. A partially applied plan
  matches no plan at all; the next run diffs from wherever the book actually is.
- **Every run is recorded, including the ones that did nothing.** A halted run and a run that never
  happened look identical in a directory of successes.

## Files under `state_dir`

| File | Written by | What it is |
|---|---|---|
| `controls.json` | operator, CLI or dashboard | the kill switch and pause, with who and why |
| `marks.json` | the price feed | `{"BTC": "64000.50"}`; the bot only reads it |
| `venue-state.json` | `bot` | the paper venue's books — fill log, resting orders, idempotency map |
| `runs/*.json` | `bot` | one file per run; timestamp-prefixed, so a listing is chronological |

One file per run rather than one growing log: a half-written line in a shared log corrupts the
whole history, and a run that died mid-write should cost its own record and no others.

## What is not built yet

- **A live venue adapter.** `paper` is the only one implemented, and `bot` refuses to start with
  any other `venue` value rather than discovering it missing at the first order.
- **A price feed.** `marks.json` is written by hand today. The bot reads marks and never fetches
  them: a bot with its own feed is a bot with its own opinion about value.
- **Adaptive execution.** Slices are a fixed hourly schedule. Real execution reads the order book —
  accelerating into depth, waiting out thin patches, posting rather than crossing. A fixed time
  slice is the honest floor (design spec §6.3).
