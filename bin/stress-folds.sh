#!/usr/bin/env bash
# The 2x slippage gate, honestly.
#
#   bin/stress-folds.sh [src-dir] [out-dir]
#
# The same per-fold models from a walk-forward run, each replaying only its own
# test block, at twice the modelled slippage. The models already exist, so this
# is a replay and not a retrain: stress changes execution, not what the model
# learned. Nine folds in about twenty minutes, against several hours to retrain.
#
# Feed the result to `crypto-portfolio gate --retrained-2x`. Without it the
# gate refuses the slippage criterion rather than reading a whole-window replay
# that the leakage guard has emptied.
#
# CAVEAT worth keeping in view: doubling the MODELLED slippage is a weak stress
# when the modelled figure is 0.5bp and the book capture measures ~5bp on the
# names that matter. Twice too small is still too small. This closes the gate
# as written; it does not settle the question the gate was asking.
set -euo pipefail
cd /home/magnus/dev/magnus/ai-trader
CP=service/target/release/crypto-portfolio
SRC="${1:-var/research/wf-rank-2020}"
OUT="${2:-var/research/wf-rank-2020-2x}"
mkdir -p "$OUT"
declare -a S=( "" 2020-09-18 2021-05-13 2022-01-05 2022-08-30 2023-04-24 2023-12-17 2024-08-10 2025-04-04 2025-11-27 )
declare -a E=( "" 2021-05-12 2022-01-04 2022-08-29 2023-04-23 2023-12-16 2024-08-09 2025-04-03 2025-11-26 2026-07-30 )
for i in $(seq 1 9); do
  echo "[fold $i] ${S[$i]} -> ${E[$i]}"
  cp "$SRC/config-$i.toml" "$OUT/config-$i.toml"
  $CP backtest --config "$OUT/config-$i.toml" --data-root data \
    --start "${S[$i]}T00:00:00Z" --end "${E[$i]}T00:00:00Z" \
    --initial-cash 100000 --slippage-multiple 2 --out "$OUT/fold-$i.json"
done
echo "done"
