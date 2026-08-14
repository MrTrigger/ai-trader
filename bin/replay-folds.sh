#!/usr/bin/env bash
# Re-price a walk-forward run's folds without retraining any of them.
#
#   bin/replay-folds.sh <src-dir> <out-dir> [slippage-multiple]
#
# Each fold already has its own model, trained only on data strictly before its
# test block, so replaying is minutes where retraining is hours. That makes it
# the right tool for any change that alters EXECUTION rather than the signal:
# a slippage stress, a funding change, a turnover rule. It is the wrong tool
# for anything touching features or training, which needs the full
# `bin/walk-forward.sh`.
#
# The fold windows below are the nine expanding blocks of wf-rank-2020. They
# are written out rather than read from the source because the source's step
# list is the thing being replaced, and deriving the window from the output
# you are about to overwrite is how a comparison quietly stops comparing.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SRC="${1:-var/research/wf-rank-2020}"
OUT="${2:-var/research/wf-rank-2020-replay}"
SLIP="${3:-1}"
CP=service/target/release/crypto-portfolio
[ -x "$CP" ] || { echo "build $CP first" >&2; exit 1; }

mkdir -p "$OUT"
declare -a S=( "" 2020-09-18 2021-05-13 2022-01-05 2022-08-30 2023-04-24 2023-12-17 2024-08-10 2025-04-04 2025-11-27 )
declare -a E=( "" 2021-05-12 2022-01-04 2022-08-29 2023-04-23 2023-12-16 2024-08-09 2025-04-03 2025-11-26 2026-07-30 )

echo "replaying 9 folds from $SRC at ${SLIP}x slippage -> $OUT"
for i in $(seq 1 9); do
  echo "[fold $i] ${S[$i]} -> ${E[$i]}"
  cp "$SRC/config-$i.toml" "$OUT/config-$i.toml"
  $CP backtest --config "$OUT/config-$i.toml" --data-root data \
    --start "${S[$i]}T00:00:00Z" --end "${E[$i]}T00:00:00Z" \
    --initial-cash 100000 --slippage-multiple "$SLIP" --out "$OUT/fold-$i.json"
done
echo "done"
