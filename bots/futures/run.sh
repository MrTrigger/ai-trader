#!/usr/bin/env bash
# The futures bot's entrypoints — Rust end to end (operator mandate
# 2026-08-06). The journal repo appears in exactly one role: exporting raw
# bars from its parquet store (data plumbing, no features — features have
# ONE implementation, the features-cme crate).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
JOURNAL="$(cd "$ROOT/../trading-journal" && pwd)"
PY="$JOURNAL/backtest/.venv/bin/python"
BOT="$ROOT/service/target/release/futures-bot"
BARS="$ROOT/var/futures/bars.jsonl"

# Deployment identity (DATABASE_URL above all) comes from .env. The parity
# gate scrubs it — hermetic on purpose.
if [ -f "$ROOT/.env" ]; then
  set -a; . "$ROOT/.env"; set +a
fi

ensure_bot() {
  [ -x "$BOT" ] || (cd "$ROOT/service" && cargo build --release -p futures-bot)
}

ensure_bars() {
  if [ ! -f "$BARS" ] || [ "${REFRESH_BARS:-}" = "1" ]; then
    mkdir -p "$(dirname "$BARS")"
    (cd "$JOURNAL/backtest" && "$PY" -m backtest.cli export-bars \
      --out "$BARS" --start "${BARS_FROM:-2026-03-01}")
  fi
}

case "${1:-}" in
  replay)
    shift
    ensure_bot; ensure_bars
    exec "$BOT" replay --bars "$BARS" --start "${1:-2026-07-01}"
    ;;
  parity)
    ensure_bot; ensure_bars
    env -u DATABASE_URL "$BOT" replay --bars "$BARS" \
      --start "$(python3 -c "import json;print(json.load(open('$HERE/parity-fixture.json'))['window_start'])")" \
      --end "$(python3 -c "import json;print(json.load(open('$HERE/parity-fixture.json'))['window_end'])")" \
      --fixture "$HERE/parity-fixture.json"
    ;;
  parity-py)
    # The lab's reference implementation replaying the same window — the
    # cross-implementation contract that regenerates the fixture after an
    # INTENDED strategy change (REGENERATE=1).
    exec "$PY" "$HERE/parity_check.py"
    ;;
  features)
    shift
    ensure_bot; ensure_bars
    exec "$BOT" features --bars "$BARS" "$@"
    ;;
  shadow)
    ensure_bot; ensure_bars
    exec "$BOT" run --bars "$BARS"
    ;;
  live)
    ensure_bot; ensure_bars
    exec "$BOT" run --bars "$BARS" --live
    ;;
  ib-check)
    shift
    ensure_bot
    exec "$BOT" ib-check "$@"
    ;;
  rithmic-check)
    shift
    ensure_bot
    exec "$BOT" rithmic-check "$@"
    ;;
  *)
    echo "usage: run.sh {replay [START]|parity|parity-py|features ...|shadow|live|ib-check|rithmic-check}" >&2
    echo "  REFRESH_BARS=1 re-exports bars; BARS_FROM=YYYY-MM-DD sets the export start" >&2
    exit 2
    ;;
esac
