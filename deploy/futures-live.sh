#!/usr/bin/env bash
# One futures-bot process, in the cluster. This is what `bots.launch` points
# at, so it is what the dashboard's Start button runs and what the api's
# supervisor relaunches.
#
# The laptop equivalent is `bots/futures/run.sh live`. They differ in exactly
# one thing and it is not the strategy: where the warmup bars come from. On the
# laptop the lab's parquet store exports them through Python; here there is no
# Python and no parquet, so IB's own history is the source — which is why the
# Gateway runs in this pod and why this script waits for it.
set -uo pipefail
BOT=/usr/local/bin/futures-bot
BARS=${FUTURES_BARS:-/data/futures/bars.jsonl}
WARMUP_DAYS=${FUTURES_WARMUP_DAYS:-45}
HOST=${IB_PAPER_HOST:-127.0.0.1}
PORT=${IB_PAPER_PORT:-4004}
say() { echo "[$(date -u +%H:%M:%SZ)] futures: $*"; }

# The bar cache and the registered state dir. Neither holds anything durable —
# the book, the fills and the snapshot are all in Postgres — but the dashboard
# reads the state dir as its pre-first-publish fallback, so an absent one shows
# up as a bot with no contract.
mkdir -p "$(dirname "$BARS")" "${FUTURES_STATE_DIR:-/data/futures/state}"

# The Gateway is a sidecar, so it is starting at the same time as everything
# else and takes a minute or two to log in. Exiting immediately would work —
# the supervisor relaunches — but it would fill the log with failures that are
# not failures. Wait, then say so if it never came.
for i in $(seq 1 60); do
  if (exec 3<>"/dev/tcp/$HOST/$PORT") 2>/dev/null; then
    say "gateway answering on $HOST:$PORT after ${i}0s"
    break
  fi
  if [ "$i" = 60 ]; then
    say "no IB Gateway on $HOST:$PORT after 10 minutes — giving up so this is visible as a failure"
    exit 1
  fi
  sleep 10
done

# The noise band needs 14 completed sessions before it reads at all, so the
# warmup is not optional and a short cache is the same as no cache. backfill
# seeds when there is nothing and extends when there is; both need the Gateway,
# which is why this is here and not in an initContainer that would run before
# the sidecar exists.
for attempt in 1 2 3; do
  if $BOT backfill --bars "$BARS" --days "$WARMUP_DAYS"; then
    break
  fi
  say "backfill attempt $attempt failed"
  [ "$attempt" = 3 ] && { say "warmup bars unavailable — refusing to trade on a short history"; exit 1; }
  sleep 20
done

# --live is a deployment decision, not a code one: it arms order flow (still
# subject to IB_ALLOW_ORDERS and, for a live account, IB_ALLOW_LIVE). Shadow is
# one env change away, which is the point.
ARM=""
case "${FUTURES_LIVE:-yes}" in
  yes | true | 1) ARM="--live" ;;
  *) say "FUTURES_LIVE=${FUTURES_LIVE:-} — running SHADOW (no orders)" ;;
esac

say "starting the loop${ARM:+ (armed)}"
exec $BOT run --bars "$BARS" $ARM
