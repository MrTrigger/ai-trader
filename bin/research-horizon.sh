#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
D=${HORIZON_DECLARATION:-docs/research/sharpe-target-horizon-final.json}
PHASE=${1:-screen}
[[ "$PHASE" == screen || "$PHASE" == confirm ]] || { echo 'usage: research-horizon.sh screen|confirm' >&2; exit 2; }
# Build the intended Rust source before running; its executable hash is pinned below.
CP=service/target/release/crypto-portfolio
P=$(jq -er .output_prefix "$D")
SOURCE=$(jq -er .source_configs "$D")
BASE=$(jq -er .source_suite "$D")
mkdir -p "$P"
uv pip freeze --python training/.venv/bin/python > "$P/current-python-environment.txt"
if [[ -e "$P/python-environment.txt" ]]; then
  cmp "$P/current-python-environment.txt" "$P/python-environment.txt"
else
  mv "$P/current-python-environment.txt" "$P/python-environment.txt"
fi
# Refuse mixed recipes or data when reusing trained models.
{
  sha256sum "$D" "$BASE" "$CP" training/train.py training/uv.lock bin/research-horizon.sh "$P/python-environment.txt"
  find data/bars data/funding data/universe -type f -print0 | sort -z | xargs -0 sha256sum
  sha256sum "$SOURCE"/config-{6,7,8,9}.toml
} > "$P/current-inputs.sha256"
if [[ -e "$P/training-inputs.sha256" ]]; then
  cmp "$P/current-inputs.sha256" "$P/training-inputs.sha256"
else
  mv "$P/current-inputs.sha256" "$P/training-inputs.sha256"
fi
LAST=3
if [[ "$PHASE" == confirm ]]; then
  "$CP" experiment-register --manifest "$P-screen/experiments.json" --out "$P-screen/selection-register.json"
  cmp "$P-screen/register.json" "$P-screen/selection-register.json"
  jq -e '.experiments | sort_by(.experiment.id) | length==2 and
    .[0].experiment.id=="h24" and .[1].experiment.id=="h72" and
    all(.[]; .experiment.expected_folds==3 and (.folds|length)==3 and all(.folds[]; .provenance.status=="verified_suite_linkage")) and
    all(.[1].folds[]; (.total_return|tonumber)>0) and
    .[1].pooled_daily_sharpe>.[0].pooled_daily_sharpe and .[1].pooled_daily_sharpe>1.3893034737' "$P-screen/register.json" >/dev/null || {
      echo '72h failed the declared selection rule; no confirmation' >&2; exit 1;
    }
  LAST=4
  sha256sum "$P-screen/register.json" "$D" > "$P/selection.sha256"
fi
for H in 24 72; do
  MATRIX="$P/matrix-$H.jsonl"
  if [[ -e "$MATRIX.sha256" ]]; then
    sha256sum -c "$MATRIX.sha256"
  else
    "$CP" training-matrix --config "$SOURCE/config-6.toml" --data-root data \
      --start "$(jq -r '.matrix_start[:10]' "$D")" --end "$(jq -r '.matrix_end[:10]' "$D")" \
      --lag-hours 1 --hold-hours "$H" --step-hours 24 --out "$MATRIX.partial"
    mv "$MATRIX.partial" "$MATRIX"
    sha256sum "$MATRIX" > "$MATRIX.sha256"
  fi
  mkdir -p "$P/h$H"
  while IFS=$'\t' read -r ORIGINAL ID THROUGH; do
    MODEL="$P/h$H/model-$ID.json"
    if [[ -e "$MODEL.sha256" ]]; then
      sha256sum -c "$MODEL.sha256"
    else
      env TRAIN_DROP= TRAIN_RANK=0 TRAIN_OBJECTIVE=regression TRAIN_SEEDS=1 \
        training/.venv/bin/python training/train.py --matrix "$MATRIX" --through "$THROUGH" \
        --reward per_risk --out "$MODEL.partial"
      mv "$MODEL.partial" "$MODEL"
      sha256sum "$MODEL" > "$MODEL.sha256"
    fi
    jq -e --arg through "$THROUGH" --argjson h "$H" '.trained_through==$through and .label_horizon_hours==$h and
      .label_lag_hours==1 and .target_contract=="rust-crypto-targets-1" and .reward=="per_risk" and (.features|length)==67' "$MODEL" >/dev/null
    awk -v model="$MODEL" '/^model_path =/ {$0="model_path = \"" model "\""} {print}' \
      "$SOURCE/config-$ORIGINAL.toml" > "$P/h$H/config-$ID.toml"
  done < <(jq -r --argjson last "$LAST" '.folds[]|select(.id<=$last)|[.original,.id,.through]|@tsv' "$D")
done
OUT="$P-$PHASE"
SUITE="$OUT-suite.json"
jq --arg p "$P" --arg phase "$PHASE" --arg out "$OUT" --argjson last "$LAST" '
  {source_dir:($p+"/h24"),data_root:.data_root,out_dir:$out,
   folds:[.folds[]|select(.id>=6 and .id<=(5+$last))|.id-=5],
   scenarios:[(["h24","h72"][]) as $id | .scenarios[]|select(if $phase=="screen" then .id=="impact" else (.id=="impact" or .id=="stress" or .id=="delay") end) |
     {id:(if $phase=="screen" then $id else ($id+"-"+.id) end),config_dir:($p+"/"+$id),initial_cash:"100000",execution:.execution}]}' "$BASE" > "$SUITE"
"$CP" replay-suite --manifest "$SUITE"
mkdir -p "$OUT/monte-carlo"
jq --arg d "$D" --arg suite "$SUITE" --arg p "$P" --arg phase "$PHASE" \
  '{experiments:[.scenarios[] as $s | {id:$s.id,family:"crypto-horizon",status:"development",trial_count:1,
    notes:["Matched fresh24h and72h fits; four-day purge; exposed evaluation dates."],fold_dir:(.out_dir+"/"+$s.id),expected_folds:(.folds|length),initial_cash:$s.initial_cash,
    sources:([$d,$suite,(.out_dir+"/inputs.json"),($p+"/training-inputs.sha256"),($p+"/python-environment.txt"),($p+"/matrix-24.jsonl.sha256"),($p+"/matrix-72.jsonl.sha256")]+if $phase=="confirm" then [($p+"/selection.sha256")] else [] end)}]}' "$SUITE" > "$OUT/experiments.json"
"$CP" experiment-register --manifest "$OUT/experiments.json" --out "$OUT/register.json"
while IFS= read -r ID; do
  for BLOCK in 5 20 60; do
    "$CP" monte-carlo --folds "$OUT/$ID" --block "$BLOCK" --iterations 10000 --seed 42 --horizon-days 365 \
      --out "$OUT/monte-carlo/$ID-block$BLOCK.json"
  done
done < <(jq -r '.scenarios[].id' "$SUITE")
if [[ "$PHASE" == screen ]]; then COSTS=(''); else COSTS=(-impact -stress -delay); fi
for COST in "${COSTS[@]}"; do
  "$CP" monte-carlo --folds "$OUT/h72$COST" --benchmark "$OUT/h24$COST" --block 20 --iterations 10000 --seed 42 --horizon-days 365 \
    --out "$OUT/monte-carlo/paired$COST.json"
done
jq -r '.experiments[]|[.experiment.id,.pooled_daily_sharpe,([.folds[].total_return|tonumber]|all(.>0))]|@tsv' "$OUT/register.json"
if [[ "$PHASE" == confirm ]]; then
  jq '.experiments[]|select(.experiment.id=="h72-impact")|
    {pooled_sharpe:.pooled_daily_sharpe,latest_fold_return:(.folds[-1].total_return|tonumber),
    development_target_met:(.pooled_daily_sharpe>1.5 and (.folds[-1].total_return|tonumber)>0),independent_validation:false}' \
    "$OUT/register.json" > "$OUT/verdict.json"
  cat "$OUT/verdict.json"
  jq -e .development_target_met "$OUT/verdict.json" >/dev/null
fi
