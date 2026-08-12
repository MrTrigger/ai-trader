#!/usr/bin/env bash
# Keep the expensive, exclusive IB session aligned with actual bot demand.
#
# The container itself is a Kubernetes native sidecar and therefore lives for
# the pod's lifetime. The Gateway does not: one lease file is held by every
# supervised IB-backed bot process. No leases means no Java, Xvfb, socat, or
# login competing with an operator's paper Gateway.
set -uo pipefail

DEMAND_DIR=${IB_GATEWAY_DEMAND_DIR:-/run/aitrader/ib-demand}
IDLE_SECONDS=${IB_GATEWAY_IDLE_SECONDS:-45}
POLL_SECONDS=${IB_GATEWAY_POLL_SECONDS:-1}
GATEWAY_COMMAND=${IB_GATEWAY_COMMAND:-/home/ibgateway/scripts/run.sh}

gateway_pid=""
gateway_kind=""
idle_since=""
# When a login could not be selected, say so once and wait, rather than
# repeating it every poll while the leaseholder times out with its own message.
refusal_said=""

say() { echo "[$(date -u +%H:%M:%SZ)] ib-gateway-demand: $*"; }

has_demand() {
  find "$DEMAND_DIR" -mindepth 1 -maxdepth 1 -type f -print -quit 2>/dev/null |
    grep -q .
}

# Which money the current leaseholders need. Paper and live are DIFFERENT IBKR
# logins, so this is not a flag on one session — it decides which credentials
# the Gateway starts with, and a wrong guess is a Gateway holding an account
# the bot will refuse (it verifies its account against the ones the Gateway
# reports) ten minutes later, looking exactly like a bad password.
#
# `live` wins a disagreement only to make it visible: one Gateway cannot serve
# both, and the paper bot failing loudly beats a live bot silently talking to a
# paper account.
demanded_kind() {
  local kind=paper
  local f
  for f in "$DEMAND_DIR"/*.lease; do
    [ -e "$f" ] || continue
    if grep -qx 'account_kind=live' "$f" 2>/dev/null; then
      kind=live
    fi
  done
  printf '%s' "$kind"
}

# Put the right login in the environment the Gateway image reads. Returns
# non-zero when it cannot, so the caller reports it instead of starting a
# Gateway that will sit at a login dialog.
select_credentials() {
  local kind=$1
  # Once per refusal, not once per poll.
  local complain=say
  [ -z "$refusal_said" ] || complain=:
  if [ "$kind" = live ]; then
    # A live Gateway holds real money. The registry binding is a deliberate
    # act, but it is also one click on the dashboard's venue picker, and that
    # is not enough on its own to open a live broker session.
    case "${IB_ALLOW_LIVE_GATEWAY:-no}" in
      yes | true | 1) ;;
      *)
        $complain "a bot is bound to the LIVE account but IB_ALLOW_LIVE_GATEWAY is not yes — refusing to log in"
        return 1
        ;;
    esac
    TWS_USERID=${IB_LIVE_USERNAME:-}
    TWS_PASSWORD=${IB_LIVE_PASSWORD:-}
    TRADING_MODE=live
  else
    TWS_USERID=${IB_PAPER_USERNAME:-}
    TWS_PASSWORD=${IB_PAPER_PASSWORD:-}
    TRADING_MODE=paper
  fi
  if [ -z "$TWS_USERID" ] || [ -z "$TWS_PASSWORD" ]; then
    $complain "no $kind credentials in the environment (IB_${kind^^}_USERNAME / IB_${kind^^}_PASSWORD)"
    return 1
  fi
  export TWS_USERID TWS_PASSWORD TRADING_MODE
  return 0
}

gateway_alive() {
  [ -n "$gateway_pid" ] && kill -0 "$gateway_pid" 2>/dev/null
}

start_gateway() {
  local kind
  kind=$(demanded_kind)
  if ! select_credentials "$kind"; then
    # Do not spin: the leaseholder is waiting and will time out with its own
    # message. Retrying every second would bury both.
    return 1
  fi
  say "IB-backed bot demand appeared ($kind); starting Gateway as $TWS_USERID"
  "$GATEWAY_COMMAND" &
  gateway_pid=$!
  gateway_kind=$kind
  idle_since=""
}

stop_gateway() {
  if gateway_alive; then
    say "${1:-no IB-backed bots remain}; stopping Gateway"
    kill -TERM "$gateway_pid" 2>/dev/null || true
    wait "$gateway_pid" 2>/dev/null || true
  fi
  gateway_pid=""
  gateway_kind=""
  idle_since=""
}

shutdown() {
  stop_gateway
  exit 0
}
trap shutdown INT TERM

mkdir -p "$DEMAND_DIR"
say "idle; waiting for an IB-backed bot lease in $DEMAND_DIR"

while true; do
  if has_demand; then
    idle_since=""
    # A Gateway logged into the wrong account is not usable by the bot that
    # asked for it — different login, different account numbers — so switch
    # rather than leave it holding the other one.
    if gateway_alive && [ -n "$gateway_kind" ] && [ "$gateway_kind" != "$(demanded_kind)" ]; then
      stop_gateway "demand changed from $gateway_kind to $(demanded_kind)"
      refusal_said=""
    fi
    if ! gateway_alive; then
      # Reap an exited child before replacing it.
      [ -z "$gateway_pid" ] || wait "$gateway_pid" 2>/dev/null || true
      gateway_pid=""
      if start_gateway; then
        refusal_said=""
      else
        refusal_said=1
      fi
    fi
  elif gateway_alive; then
    now=$(date +%s)
    if [ -z "$idle_since" ]; then
      idle_since=$now
      say "last lease released; waiting ${IDLE_SECONDS}s before shutdown"
    elif [ $((now - idle_since)) -ge "$IDLE_SECONDS" ]; then
      stop_gateway
      say "idle; waiting for an IB-backed bot lease in $DEMAND_DIR"
    fi
  else
    # Reap a Gateway that died without being asked to stop. It will be started
    # again on the next pass if demand has reappeared.
    if [ -n "$gateway_pid" ]; then
      wait "$gateway_pid" 2>/dev/null || true
      gateway_pid=""
      idle_since=""
    fi
  fi
  sleep "$POLL_SECONDS"
done
