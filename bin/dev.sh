#!/usr/bin/env bash
# The development loop: both halves reload themselves.
#
#   frontend  bun/vite HMR on :5174, proxying /api to the api on :7434 —
#             editing a component updates the open page without a refresh.
#   api       cargo-watch rebuilds and restarts the Rust process when any
#             crate changes. It serves frontend/dist too, so the built
#             bundle is picked up per request; you only need :5174 while
#             editing the UI.
#
# Use :5174 while working on the frontend, :7434 to see exactly what a
# deployment serves. Ctrl-C stops both.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
[ -f .env ] && { set -a; . ./.env; set +a; }

command -v cargo-watch >/dev/null || {
  echo "cargo-watch missing: cargo install cargo-watch" >&2; exit 1; }

trap 'kill 0' EXIT INT TERM

( cd frontend && bun install --silent && bun run dev ) &

cargo watch --quiet \
  --workdir "$ROOT" \
  --watch service/crates \
  --ignore 'frontend/**' \
  -x "run --manifest-path service/Cargo.toml --bin api -- \
      --state-dir var/live/state --initial-cash 30000 --quote-currency USDC \
      --bot ./service/target/debug/bot --bot-config var/live/bot.json \
      --expectation docs/research/backtest.json" &

wait
