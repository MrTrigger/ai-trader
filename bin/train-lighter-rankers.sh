#!/usr/bin/env bash
# Fit the two predeclared venue-aware rankers without changing live models.
set -euo pipefail

REPO=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
OUT=${1:?usage: bin/train-lighter-rankers.sh NEW_OUTPUT_DIR}
RAW=${LIGHTER_DAILY_ROOT:-$REPO/var/research/lighter-history-audit/daily-2026-09-08}
SIGNALS=${DATA_ROOT:-$REPO/data}
CONFIG=${LIGHTER_CONFIG:-$REPO/deploy/lighter-config.toml}
PYTHON=${PYTHON_BINARY:-$REPO/training/.venv/bin/python}
BUILDER=${MATRIX_BUILDER:-$REPO/service/target/release/examples/lighter-training-matrix}

[[ ! -e "$OUT" ]] || { echo "output already exists: $OUT" >&2; exit 1; }
[[ -d "$RAW" && -d "$SIGNALS" && -f "$CONFIG" ]] || { echo 'native history, signal data, or config is missing' >&2; exit 1; }
[[ -x "$PYTHON" ]] || { echo "training Python is not executable: $PYTHON" >&2; exit 1; }
if [[ ! -x "$BUILDER" ]]; then
  cargo build --release --manifest-path "$REPO/service/Cargo.toml" \
    -p crypto-portfolio --example lighter-training-matrix
fi

STAGE=${OUT}.partial.$$
trap 'rm -rf -- "$STAGE"' EXIT
mkdir -p "$STAGE/provenance"
cp "$BUILDER" "$STAGE/provenance/matrix-builder"
cp "$REPO/training/train.py" "$STAGE/provenance/train.py"
cp "${BASH_SOURCE[0]}" "$STAGE/provenance/train-lighter-rankers.sh"
cp "$CONFIG" "$STAGE/provenance/config.toml"

# This declaration exists before either matrix or model. Both candidates use
# one fixed recipe and differ only in their stated label-price source.
cat > "$STAGE/declaration.json" <<'JSON'
{
  "schema": "lighter-venue-ranker-experiment-1",
  "declared_before_fit": true,
  "training_decisions": {"start": "2025-01-18", "end": "2025-11-24"},
  "evaluation_starts": "2025-11-27",
  "cohort": "causal Lighter activity gate, then external-liquidity top 30",
  "quarantine": "omit every whole decision cross-section q-1 through q+30",
  "candidates": [
    {"id": "lighter-cohort-external-label", "label_prices": "external", "lag_hours": 1, "hold_hours": 24},
    {"id": "lighter-cohort-native-label", "label_prices": "native", "lag_hours": 0, "hold_hours": 24}
  ],
  "trainer": {"program": "training/train.py", "reward": "per_risk", "objective": "regression", "seeds": 1, "rank_transform": false, "feature_drop": []},
  "selection": "no hyperparameter or candidate search; evaluate both predeclared candidates"
}
JSON

for kind in external native; do
  "$STAGE/provenance/matrix-builder" "$kind" "$RAW" "$SIGNALS" "$CONFIG" \
    2025-01-18 2025-11-24 "$STAGE/$kind"
  env -u TRAIN_DROP TRAIN_OBJECTIVE=regression TRAIN_SEEDS=1 TRAIN_RANK=0 \
    "$PYTHON" "$STAGE/provenance/train.py" --matrix "$STAGE/$kind/matrix.jsonl" \
    --through 2025-11-24 --reward per_risk --out "$STAGE/$kind/model.json"
done

find "$RAW" -type f -print0 | sort -z | xargs -0 sha256sum > "$STAGE/native-inputs.sha256"
find "$SIGNALS/bars" "$SIGNALS/funding" "$SIGNALS/perp" -type f -print0 \
  | sort -z | xargs -0 sha256sum > "$STAGE/signal-inputs.sha256"
(
  cd -- "$STAGE"
  sha256sum provenance/* external/matrix.jsonl native/matrix.jsonl \
    external/cohort.json native/cohort.json declaration.json > inputs.sha256
  sha256sum external/model.json native/model.json > outputs.sha256
  sha256sum -c inputs.sha256
  sha256sum -c outputs.sha256
)
printf 'TRAIN_OBJECTIVE=regression\nTRAIN_SEEDS=1\nTRAIN_RANK=0\nTRAIN_DROP=<unset>\n' > "$STAGE/training-environment.txt"
mv -- "$STAGE" "$OUT"
trap - EXIT
printf 'Saved two predeclared venue-aware rankers in %s\n' "$OUT"
