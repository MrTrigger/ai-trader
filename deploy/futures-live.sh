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

# A cheap first gate: is anything listening at all. This is NOT a readiness
# check and must not be mistaken for one — socat accepts on the relay port from
# the moment the container starts, long before IB Gateway behind it has logged
# in, and a client that connects then gets an immediate EOF. Measured in the
# pod: the port was open and `ib-check` still answered "connection rejected:
# early eof".
for i in $(seq 1 12); do
  (exec 3<>"/dev/tcp/$HOST/$PORT") 2>/dev/null && break
  say "nothing listening on $HOST:$PORT yet"
  sleep 10
done

# The real gate is a completed IB handshake that returns data, and the honest
# way to test that is to ask for the thing we actually need. The noise band
# needs 14 completed sessions before it reads at all, so the warmup is not
# optional and a short cache is the same as no cache: backfill seeds when there
# is nothing and extends when there is.
#
# The Gateway is a sidecar, starting alongside everything else, and IBC's login
# takes a minute or two — longer if IB is slow, forever if the credentials are
# wrong. So retry patiently and then FAIL, rather than exiting straight into the
# supervisor's arms: a bot relaunching every minute against a Gateway that will
# never log in buries the one line that says why.
ok=""
for attempt in $(seq 1 20); do
  if $BOT backfill --bars "$BARS" --days "$WARMUP_DAYS"; then
    ok=1
    break
  fi
  say "no warmup bars yet (attempt $attempt/20) — is the Gateway logged in?"
  sleep 30
done
if [ -z "$ok" ]; then
  say "10 minutes without warmup bars. Check TWS_USERID/TWS_PASSWORD and the"
  say "ib-gateway container's log; refusing to trade on a short history."
  exit 1
fi

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
