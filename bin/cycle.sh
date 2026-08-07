#!/usr/bin/env bash
# One decision-and-execution cycle: pull data, decide, execute.
#
#   bin/cycle.sh [bot-config]
#
# This is deliberately a script and not a daemon. Design spec §8.1: the
# scheduler invokes the same commands a human does, and nothing has a private
# path into the engine. A bespoke scheduler would be a second way to run things,
# and the second way is the one nobody tests.
#
# Cron it:
#   5 0 * * *  cd /path/to/ai-trader && bin/cycle.sh >> var/live/cycle.log 2>&1
#
# The marks feed is a separate, always-on process — see docs/operations.md.
#
# Exit codes are the point: anything non-zero and cron mails you. Every stage
# fails the whole cycle rather than pressing on, because a plan built on stale
# data is worse than no plan.
set -euo pipefail

CONFIG="${1:-var/live/bot.json}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BOT=./service/target/debug/bot
[ -x "$BOT" ] || BOT=./service/target/release/bot
[ -x "$BOT" ] || { echo "cycle: no bot binary — cargo build first" >&2; exit 2; }

PLANNER=./service/target/debug/crypto-portfolio
[ -x "$PLANNER" ] || PLANNER=./service/target/release/crypto-portfolio
[ -x "$PLANNER" ] || { echo "cycle: no crypto-portfolio binary — cargo build first" >&2; exit 2; }

command -v jq >/dev/null || { echo "cycle: jq is required to read bot config" >&2; exit 2; }
STATE_DIR="$(jq -er '.state_dir | strings' "$CONFIG")"
PLAN="$STATE_DIR/plan.json"
STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
DAY="$(date -u +%Y-%m-%d)"

say() { echo "[$(date -u +%H:%M:%S)] $*"; }

# 0. Refuse to run a cycle the previous one is still inside. The execution
#    window is hours long, and two overlapping runs would each diff against a
#    book the other is moving.
LOCK="$STATE_DIR/cycle.lock"
mkdir -p "$STATE_DIR"
exec 9>"$LOCK"
flock -n 9 || { say "a cycle is already running; leaving it alone"; exit 0; }

say "cycle $STAMP  config=$CONFIG"

# 1. Data. Without fresh bars the planner is deciding on yesterday.
say "pull"
"$PLANNER" data-pull --config config/default.toml --data-root data --days 7

# 2. Decide. Read-only: produces a plan and touches nothing.
#
# What the venue lists and what the account holds are facts about the exchange,
# and the exchange is the data provider in paper exactly as in live. Both are
# asked for live and piped straight in - never written down. A stored copy can
# only ever be more wrong than the venue, and a stale one is how a 30k account
# got handed a plan built for 100k, and how AAVE was refused for not being on a
# list somebody typed by hand.
#
# --for-execution stamps the plan mode "live", which is what makes the executor
# willing to run it at all. It does NOT mean real money: the bot's venue mode
# decides that, and a paper bot fed this plan fills it against the simulator.
# Without the flag the planner emits a "dry" plan and step 3 refuses it every
# single time, which is how this script sat broken.
say "plan"
"$PLANNER" universe-rank --config config/default.toml --data-root data \
    --as-of "$DAY" --top 30 --overwrite \
    --tradeable <("$BOT" --config "$CONFIG" markets)

"$PLANNER" plan --config config/default.toml --data-root data \
    --as-of "$DAY" --for-execution \
    --book <("$BOT" --config "$CONFIG" book) --out "$PLAN"

# 3. Execute. The only step that can move capital, and the only one that
#    consults the controls.
say "execute"
"$BOT" --config "$CONFIG" run --plan "$PLAN"

say "done"
