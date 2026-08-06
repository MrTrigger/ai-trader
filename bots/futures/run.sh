#!/usr/bin/env bash
# The futures bot's entrypoints. Uses the journal repo's venv (the decision
# core's own environment) — one code path includes one dependency set.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
JOURNAL="$(cd "$ROOT/../trading-journal" && pwd)"
PY="$JOURNAL/backtest/.venv/bin/python"
STATE="$ROOT/var/futures/state"
mkdir -p "$STATE"

case "${1:-}" in
  replay)
    shift
    cd "$JOURNAL/backtest" && exec "$PY" -m backtest.cli bot-replay \
      --rules rules-lab --start "${1:-2025-01-01}" --state-dir "$STATE"
    ;;
  shadow)
    cd "$JOURNAL/backtest" && exec "$PY" -m backtest.cli bot-shadow --state-dir "$STATE"
    ;;
  parity)
    exec "$PY" "$HERE/parity_check.py"
    ;;
  *)
    echo "usage: run.sh {replay [START]|shadow|parity}" >&2
    exit 2
    ;;
esac
