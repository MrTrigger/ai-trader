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
idle_since=""

say() { echo "[$(date -u +%H:%M:%SZ)] ib-gateway-demand: $*"; }

has_demand() {
  find "$DEMAND_DIR" -mindepth 1 -maxdepth 1 -type f -print -quit 2>/dev/null |
    grep -q .
}

gateway_alive() {
  [ -n "$gateway_pid" ] && kill -0 "$gateway_pid" 2>/dev/null
}

start_gateway() {
  say "IB-backed bot demand appeared; starting Gateway"
  "$GATEWAY_COMMAND" &
  gateway_pid=$!
  idle_since=""
}

stop_gateway() {
  if gateway_alive; then
    say "no IB-backed bots remain; stopping Gateway"
    kill -TERM "$gateway_pid" 2>/dev/null || true
    wait "$gateway_pid" 2>/dev/null || true
  fi
  gateway_pid=""
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
    if ! gateway_alive; then
      # Reap an exited child before replacing it.
      [ -z "$gateway_pid" ] || wait "$gateway_pid" 2>/dev/null || true
      gateway_pid=""
      start_gateway
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
