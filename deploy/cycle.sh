#!/usr/bin/env bash
# One decision-and-execution cycle on the cluster. Same commands a human runs.
# Exit codes are the point: a non-zero stage fails the CronJob and shows red.
set -euo pipefail
BOT=${BOT_BINARY:-/usr/local/bin/bot}
CP=${PLANNER_BINARY:-/usr/local/bin/crypto-portfolio}
CFG=${PORTFOLIO_CONFIG:-/app/config.toml}
BOTCFG=${BOT_CONFIG:-/app/bot.json}
DATA=${DATA_ROOT:-/data}
MARKET_DATA=${MARKET_DATA_ROOT:-$DATA}
STATE=$DATA/state
mkdir -p "$STATE"
say() { echo "[$(date -u +%H:%M:%SZ)] $*"; }

exec 9>"$STATE/cycle.lock"
flock -n 9 || { say "a cycle is already running; leaving it alone"; exit 0; }

# Catch-up only. A period closes when the NEXT run is recorded, so at this
# point today's run does not exist yet and yesterday's period is still open -
# settling here can only pick up days a previous cycle missed. The settle that
# does today's work runs after the execution below.
say "settle (catch-up)"
$BOT --config $BOTCFG settle || say "settle failed; continuing - the next cycle will pick it up"

# Each venue owns its universe and state. Only bars/funding are shared.
if [[ "$DATA" != "$MARKET_DATA" ]]; then
  for part in bars funding; do
    if [[ -L "$DATA/$part" ]]; then
      [[ "$(readlink "$DATA/$part")" == "$MARKET_DATA/$part" ]] || { say "wrong shared $part target"; exit 1; }
    elif [[ -e "$DATA/$part" ]]; then
      say "$DATA/$part must be a link to shared market data"; exit 1
    else
      ln -s "$MARKET_DATA/$part" "$DATA/$part"
    fi
  done
fi
say "pull"
# Both venue schedules take this lock before updating shared partitions.
(
  flock 8
  "$CP" data-pull --config "$CFG" --data-root "$MARKET_DATA" --days 8
) 8>"$MARKET_DATA/market-data.lock"

say "universe (venue-screened)"
# Snapshots are append-only and never backfilled - that rule is what makes a
# historical universe reconstruction honest. A cycle re-run on the same day is
# not a violation of it, it is the idempotent case, so tolerate exactly that
# one refusal and nothing else.
MARKETS=$(mktemp "$STATE/markets.XXXXXX")
BOOK=$(mktemp "$STATE/book.XXXXXX")
trap 'rm -f "$MARKETS" "$BOOK"' EXIT
"$BOT" --config "$BOTCFG" markets > "$MARKETS"
if ! out=$($CP universe-rank --config $CFG --data-root $DATA \
      --as-of "$(date -u +%Y-%m-%d)T00:00:00Z" --step-days 1 \
      --tradeable "$MARKETS" 2>&1); then
  case "$out" in
    *"already exists; snapshots are append-only"*)
      say "today's snapshot is already recorded; keeping it" ;;
    *) echo "$out" >&2; exit 1 ;;
  esac
else
  echo "$out"
fi

say "plan (frozen rank artefact, live book)"
"$BOT" --config "$BOTCFG" book > "$BOOK"
$CP plan --config $CFG --data-root $DATA \
  --as-of "$(date -u +%Y-%m-%d)T00:00:00Z" --for-execution \
  --book "$BOOK" --out "$STATE/plan.json"

# What the book would charge us for this plan, read before anything is sent.
# The cost model's spread is a constant and its impact coefficient is assumed,
# and paper cannot correct either: a paper fill is the real mark moved by a
# configured slippage, so fitting a model to it recovers the constant. The
# resting book is the only honest source that costs no capital, and one reading
# a day for the names actually traded is what turns it into a distribution.
#
# Never fatal. A missing measurement is a gap in a research series, not a reason
# to skip a day's trading.
say "book (measured spread, for the cost model)"
$BOT --config $BOTCFG book-capture --plan "$STATE/plan.json" \
  --out "$DATA/liquidity/spreads.json" || say "book capture failed; continuing"

# The decision-lag gate refuses to fill a decision far behind the fill, because
# the backtest says a 24h lag takes Sharpe from 2.22 to 0.29. On the daily
# schedule the lag is five minutes and the gate never fires. A BOOTSTRAP run is
# the one honest exception: starting from flat, holding the target book on a
# stale decision beats holding nothing until midnight, and the next scheduled
# cycle corrects it. Paper-only - the flag is refused under a live venue.
say "execute"
if [ "${BOOTSTRAP:-0}" = "1" ]; then
  $BOT --config $BOTCFG run --plan "$STATE/plan.json" --accept-decision-lag
else
  $BOT --config $BOTCFG run --plan "$STATE/plan.json"
fi

# Now that this run is recorded, the period the PREVIOUS run opened has a
# closing mark and can be attributed. Doing this only at the top of the cycle
# left every day settled twenty-four hours late: the run list at that moment
# ends at yesterday, so yesterday had no closer and was skipped.
#
# Idempotent, so the catch-up pass above and this one cannot double-count.
say "settle (the period this run just closed)"
$BOT --config $BOTCFG settle || say "settle failed; continuing - the next cycle will pick it up"

say "done"
