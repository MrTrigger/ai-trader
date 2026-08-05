# Running it

Three processes, none of which knows about the others at runtime. They meet through files.

```
  planner (Python)          bot (Rust)                  api (Rust)
  read-only venue key       trade-scoped key            no key
  decides                   executes                    shows
        │                        │                          │
        └──── plan.json ────────►│                          │
                                 ├──► runs/*.json ─────────►│
                                 ├──► ledger.jsonl ────────►│
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
    --bot ./bot --bot-config config/bot.json \
    --expectation docs/research/backtest.json
```

Then <http://127.0.0.1:7434>. Loopback only, with no flag to change that.

Without `--bot` and `--bot-config` the page shows everything and changes nothing. That is the
default: a dashboard that could act the moment it was pointed at a state directory would be one
nobody chose to give authority to.

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

## Connecting to an account that already has a book

A fresh install, a new machine, a restored backup, or the first connection to a funded venue all
look the same from inside: the venue holds positions and our ledger explains none of them.

**That is a halt, not something absorbed.** Adopting whatever the venue says is exactly the
auto-repair the executor refuses to do — it would turn evidence of a bug into an opening position.
But a permanent halt is no good either, or the system could never be pointed at a real account at
all. So there is one deliberate, attributed command:

```bash
bot --config config/bot.json reconcile     # what do the two sides say? changes nothing
bot --config config/bot.json adopt --reason "first connection, checked the account" \
    --by magnus --confirm
```

`adopt` writes an opening baseline into the ledger, with the name and the reason attached, and
records it as a run. From then on the ledger explains the book and reconciliation has teeth again.

### The order ledger

`state_dir/ledger.jsonl` is **our** record, kept deliberately apart from the venue's. Every order id
is written there *before* it is sent, so a venue fill carrying an id we never issued is visible.

That check is the point. Without it, "our" positions would be folded from the venue's own fill log —
the venue on both sides of the comparison, which cannot disagree with itself — and the reconcile that
exists to catch a compromised key, a second process on the account, or a stale order from an earlier
deployment would pass every time.

A fill we never authorised means one of: someone else trading the account, a stale order, a stolen
key — or, benignly, that this bot's ledger was lost and the venue still remembers what it did. Those
are indistinguishable from in here, so the system stops and a human decides:

```bash
bot --config config/bot.json adopt --reason "restored from backup; checked by hand" \
    --by magnus --confirm --accept-unknown-fills
```

The fills are recorded as acknowledged, with who and why, rather than erased. They are not folded
into our position — the baseline written alongside them already accounts for where the book ended
up.

## Stopping it

| | What it does |
|---|---|
| **halt** | Plans, never executes. The strongest stop. |
| **pause** | Only orders that reduce exposure go out. A paused book can still get out. |
| **resume** | Permits trading again. |
| **flatten** | Cancels every resting order, then closes every open position at market. |
| **adopt** | Takes pre-existing venue positions on as our opening state. |

All five are on the dashboard and all five are CLI commands. They are the same thing: the page runs
the `bot` binary, so there is exactly one implementation of what each control means and a button
cannot drift from a shell.

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

**Flatten cancels before it sells.** Resting orders go first, then positions close, and the
positions are re-read in between because cancelling releases inventory a resting sell had claimed.
A flatten that only sold would leave a working buy alive to fill an hour later and re-open the book
— and every record would say it worked. If any part fails the record says THE ACCOUNT IS NOT FLAT
and names what is still live.

### How the dashboard can do this without holding a key

It does not perform controls; it runs `bot`, which holds the credential. The `api` process still
has none, and the argument vector it builds comes from a closed list — nothing the browser sends
becomes a flag, an option or a path. That is the difference between a page that can *ask* for a
flatten and a web server that can place an order.

Every action still needs a name and a reason, still passes the bot's own gates, and still lands in
the run history. Flatten additionally asks you to type FLATTEN, because it is the one control whose
effect cannot be undone by pressing something else.

The api's console log records every control run from the page, marking the ones that move capital
or grant authority — "who flattened the book at 02:14" is a question that gets asked.

## Failing closed

- **No control file, or an unreadable one, means halted.** A deleted file, a failed disk or a
  botched deploy costs opportunity, not money.
- **A stale plan is refused.** Past `max_plan_age_minutes` it was computed against prices that no
  longer exist.
- **A plan that already ran is refused.** Re-running it would submit against a book that has moved,
  and none of the order ids would collide because the quantities differ.
- **Reconciliation drift halts and is never repaired.** Our ledger against the venue's book, before
  every slice rather than once per run — the execution window is hours long, and the account can
  change underneath it. A mismatch means one of our assumptions is wrong, and trading on top of it
  turns a bug into a loss.
- **A venue we cannot read is a halt, not a guess.** Reads are retried three times while the venue
  is unreachable; writes never are, because a submission that may or may not have landed is how a
  position gets doubled. Order ids carry the idempotency instead.
- **A run that submitted nothing leaves its plan runnable.** The replay guard exists to stop a
  double submission; blocking a retry after a halt would force a re-plan at exactly the moment
  something has gone wrong.
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
| `ledger.jsonl` | `bot` | our own append-only record: order ids we authorised, positions we adopted, fills we acknowledged |

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
