#!/usr/bin/env bash
# Apply the same selected policy and control to a static venue membership.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE=${1:?usage: bin/research-selection-venue.sh CONFIRMATION_SUITE DATA_ROOT OUTPUT_DIR}
DATA=${2:?missing prepared data root}
OUT=${3:?missing output directory}
CP=service/target/release/crypto-portfolio
[[ -s "$DATA/manifest.json" ]] || { echo 'missing membership provenance' >&2; exit 1; }
jq -e '.schema_version=="static-lighter-membership-1"' "$DATA/manifest.json" >/dev/null
jq -e '[.scenarios[]|select(.id|endswith("-impact"))] as $arms |
  ($arms|length)==2 and any($arms[];.id=="control-impact") and (.folds|length)==4' "$SOURCE" >/dev/null
jq --arg data "$DATA" --arg out "$OUT" '
  .data_root=$data | .out_dir=$out | .scenarios|=map(select(.id|endswith("-impact")))' \
  "$SOURCE" > "$OUT-suite.json"
"$CP" replay-suite --manifest "$OUT-suite.json"
jq --arg source "$SOURCE" --arg membership "$DATA/manifest.json" --arg manifest "$OUT-suite.json" '
  {experiments:[.scenarios[] as $s | {id:$s.id,family:"selection-trend-venue-sensitivity",status:"development",
    trial_count:1,notes:["Static current membership; original price and funding proxies; no historical venue claim."],
    fold_dir:(.out_dir+"/"+$s.id),expected_folds:4,initial_cash:$s.initial_cash,
    sources:[$source,$membership,$manifest,(.out_dir+"/inputs.json")]}]}' \
  "$OUT-suite.json" > "$OUT/experiments.json"
"$CP" experiment-register --manifest "$OUT/experiments.json" --out "$OUT/register.json"
mkdir -p "$OUT/monte-carlo"
while IFS= read -r id; do
  "$CP" monte-carlo --folds "$OUT/$id" --out "$OUT/monte-carlo/$id.json" \
    --block 20 --iterations 10000 --seed 42 --horizon-days 365
done < <(jq -r '.scenarios[].id' "$OUT-suite.json")
CANDIDATE=$(jq -er '.scenarios[]|select(.id!="control-impact")|.id' "$OUT-suite.json")
"$CP" monte-carlo --folds "$OUT/$CANDIDATE" --benchmark "$OUT/control-impact" \
  --out "$OUT/monte-carlo/paired.json" --block 20 --iterations 10000 --seed 42 --horizon-days 365
jq -r '.experiments[]|[.experiment.id,.pooled_daily_sharpe,.folds[-1].total_return]|@tsv' "$OUT/register.json"
