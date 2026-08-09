#!/usr/bin/env bash
# Run a cycle at startup if one is overdue, so a fresh deployment takes
# positions now rather than sitting flat until the next scheduled hour.
#
# Not a second code path: it calls the same cycle.sh the CronJob does. The only
# difference is BOOTSTRAP=1, which permits the decision-lag override for this
# one run - see cycle.sh for why that is the honest exception rather than a
# loosening of the gate.
#
# No interpreter. The first version parsed `history` with python3, which is not
# in the runtime image; the error was swallowed by an `|| echo ""` and read as
# "this bot has never run", so every pod restart ran a spurious cycle against a
# 22-position book. The bot already computes the number - ask it for that
# rather than re-deriving it from JSON in a shell.
set -euo pipefail
WINDOW_HOURS="${BOOTSTRAP_WINDOW_HOURS:-20}"
BOT=/usr/local/bin/bot
BOTCFG=/app/bot.json
say() { echo "[$(date -u +%H:%M:%SZ)] bootstrap: $*"; }

if ! status=$("$BOT" --config "$BOTCFG" status 2>&1); then
  # Fail CLOSED. A missed bootstrap costs one cycle; a bootstrap that fires
  # because it could not read the state trades a stale decision against a book
  # it knows nothing about. The scheduled cycle is along in hours anyway.
  say "cannot read status - skipping rather than trading blind"
  say "$status"
  exit 0
fi

# The VALUE after the colon: a number, or the literal null when nothing has
# ever run. Filtering characters out of the whole line does not work - the key
# name contains an "n", which made every reading look like "null".
age=$(printf '%s\n' "$status" \
      | grep -m1 '"hours_since_last_run"' \
      | cut -d: -f2 | tr -d ' ,' || true)

case "$age" in
  "" )
    say "status did not report hours_since_last_run - skipping rather than trading blind"
    exit 0 ;;
  null )
    say "no run on record; running the first cycle" ;;
  *[!0-9]* )
    say "hours_since_last_run was \"$age\", which is neither a number nor null - skipping"
    exit 0 ;;
  * )
    if [ "$age" -lt "$WINDOW_HOURS" ]; then
      say "last run was ${age}h ago, inside the ${WINDOW_HOURS}h window - nothing to do"
      exit 0
    fi
    say "last run was ${age}h ago; running a catch-up cycle" ;;
esac

BOOTSTRAP=1 exec /usr/local/bin/cycle.sh
