#!/usr/bin/env bash
# One decision-and-execution cycle on the cluster. Same commands a human runs.
# Exit codes are the point: a non-zero stage fails the CronJob and shows red.
set -euo pipefail
BOT=/usr/local/bin/bot
CP=/usr/local/bin/crypto-portfolio
CFG=/app/config.toml
BOTCFG=/app/bot.json
DATA=/data
STATE=$DATA/state
mkdir -p "$STATE"
say() { echo "[$(date -u +%H:%M:%SZ)] $*"; }

exec 9>"$STATE/cycle.lock"
flock -n 9 || { say "a cycle is already running; leaving it alone"; exit 0; }

say "pull"
$CP data-pull --config $CFG --data-root $DATA --days 8

say "universe (venue-screened)"
$CP universe-rank --config $CFG --data-root $DATA \
  --as-of "$(date -u +%Y-%m-%d)T00:00:00Z" --step-days 1 \
  --tradeable <($BOT --config $BOTCFG markets)

say "plan (frozen rank artefact, live book)"
$CP plan --config $CFG --data-root $DATA \
  --as-of "$(date -u +%Y-%m-%d)T00:00:00Z" --for-execution \
  --book <($BOT --config $BOTCFG book) --out "$STATE/plan.json"

say "execute"
$BOT --config $BOTCFG run --plan "$STATE/plan.json"

say "done"
