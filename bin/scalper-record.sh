#!/usr/bin/env bash
# Hourly book-cost recording for the scalper research pipeline (Plan 3,
# docs/scalper-research.md §2).
#
#   bin/scalper-record.sh [top] [seconds] [interval]
#
# Defaults: top=25, seconds=3600, interval=10 - one hour of 10-second
# snapshots across the live top-25 markets, matching a plain hourly cron
# tick. Each invocation stops starting new rounds once `seconds` has
# elapsed (an in-flight round is allowed to finish, so it can overrun by up
# to one round) and then exits; the next cron tick starts the next one, so
# a hung recorder is replaced within the hour instead of silently stopping
# coverage for the rest of the day.
#
# Cron it:
#   0 * * * *  cd /path/to/ai-trader && bin/scalper-record.sh >> var/live/scalper-record.log 2>&1
set -euo pipefail

TOP="${1:-25}"
SECONDS_ARG="${2:-3600}"
INTERVAL="${3:-10}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SCALPER_DATA=./service/target/release/scalper-data
[ -x "$SCALPER_DATA" ] || SCALPER_DATA=./service/target/debug/scalper-data
[ -x "$SCALPER_DATA" ] || {
    echo "scalper-record: no scalper-data binary - cargo build -p scalper-data first" >&2
    exit 2
}

"$SCALPER_DATA" record-books \
    --data-root data \
    --top "$TOP" --seconds "$SECONDS_ARG" --interval "$INTERVAL"
