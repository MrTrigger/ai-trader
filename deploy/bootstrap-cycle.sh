#!/usr/bin/env bash
# Run a cycle at startup if one is overdue, so a fresh deployment takes
# positions now rather than sitting flat until the next scheduled hour.
#
# Not a second code path: it calls the same cycle.sh the CronJob does. The only
# difference is BOOTSTRAP=1, which permits the decision-lag override for this
# one run - see cycle.sh for why that is the honest exception rather than a
# loosening of the gate.
#
# Idempotent by design: it asks the run history first and does nothing if a
# clean run landed inside the window. So a pod restart at 03:00 does not
# re-trade a book the 00:05 cycle already set.
set -euo pipefail
WINDOW_HOURS="${BOOTSTRAP_WINDOW_HOURS:-20}"
BOT=/usr/local/bin/bot
BOTCFG=/app/bot.json
say() { echo "[$(date -u +%H:%M:%SZ)] bootstrap: $*"; }

# Enough rows that a refusal or a venue-change at the head cannot hide the
# executed run behind it: --limit 1 did exactly that, and the script concluded
# a bot with 22 open positions had never run.
if ! hist=$($BOT --config $BOTCFG history --limit 25 2>/dev/null); then
  # Fail CLOSED. A missed bootstrap costs one cycle; a bootstrap that fires
  # because it could not read the history trades a stale decision against a
  # book it knows nothing about. The scheduled cycle is along in hours anyway.
  say "cannot read the run history - skipping rather than trading blind"
  exit 0
fi

last=$(printf '%s' "$hist" | python3 -c "
import json,sys
try:
    rows = json.load(sys.stdin)
except Exception:
    print('UNREADABLE'); raise SystemExit
clean = [r for r in rows if r.get('outcome') == 'executed']
print(clean[0]['recorded_at'] if clean else '')
")

if [ "$last" = "UNREADABLE" ]; then
  say "run history did not parse - skipping rather than trading blind"
  exit 0
fi

if [ -n "$last" ]; then
  age=$(python3 -c "
from datetime import datetime, timezone
t = datetime.fromisoformat('$last'.replace('Z', '+00:00'))
print(int((datetime.now(timezone.utc) - t).total_seconds() // 3600))
")
  if [ "$age" -lt "$WINDOW_HOURS" ]; then
    say "last clean run was ${age}h ago, inside the ${WINDOW_HOURS}h window - nothing to do"
    exit 0
  fi
  say "last clean run was ${age}h ago; running a catch-up cycle"
else
  say "no clean run on record; running the first cycle"
fi

BOOTSTRAP=1 exec /usr/local/bin/cycle.sh
