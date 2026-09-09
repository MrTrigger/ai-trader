#!/usr/bin/env bash
# One declared timing profile, four expanding folds. Never replaces prior fits.
set -euo pipefail
REPO=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
DATA=${DATA_ROOT:-$REPO/data}
CP=${PORTFOLIO_BINARY:-$REPO/service/target/release/crypto-portfolio}
PYTHON=${PYTHON_BINARY:-$REPO/training/.venv/bin/python}
OUT=${1:?usage: bin/train-selection-timer.sh NEW_OUTPUT_DIR}
[[ ! -e "$OUT" ]] || { echo "output already exists: $OUT" >&2; exit 1; }
[[ -x "$CP" && -x "$PYTHON" ]] || { echo 'planner binary and training Python must be executable' >&2; exit 1; }
mkdir -p "$OUT"
OUT=$(cd -- "$OUT" && pwd)
DATA=$(cd -- "$DATA" && pwd)
printf '%s\n' "$DATA" > "$OUT/data-root.txt"
cp "$CP" "$OUT/matrix-builder"
cp "$REPO/training/train_directional.py" "$OUT/trainer.py"
cp "${BASH_SOURCE[0]}" "$OUT/train-selection-timer.sh"
"$OUT/matrix-builder" directional-matrix --data-root "$DATA" --out "$OUT/market-matrix.jsonl"
sha256sum "$OUT/matrix-builder" "$OUT/trainer.py" "$OUT/train-selection-timer.sh" "$OUT/market-matrix.jsonl" > "$OUT/inputs.sha256"
printf 'fold\tevaluation_start\ttrained_through\tprofile\n' > "$OUT/declaration.tsv"
starts=(2023-12-17 2024-08-10 2025-04-04 2025-11-27)
for offset in 0 1 2 3; do
  fold=$((offset+6))
  start=${starts[$offset]}
  through=$(date -u --date="$start -2 days" +%F)
  printf '%s\t%s\t%s\tselection-timer-7d-v1\n' "$fold" "$start" "$through" >> "$OUT/declaration.tsv"
  OMP_NUM_THREADS=${OMP_NUM_THREADS:-4} "$PYTHON" "$OUT/trainer.py" \
    --matrix "$OUT/market-matrix.jsonl" --label fwd_z_7 --through "$through" \
    --profile selection-timer-7d-v1 --out "$OUT/model-$fold.json"
done
sha256sum -c "$OUT/inputs.sha256"
sha256sum "$OUT"/model-*.json "$OUT/declaration.tsv" > "$OUT/outputs.sha256"
printf 'Saved four purged timing models in %s\n' "$OUT"
