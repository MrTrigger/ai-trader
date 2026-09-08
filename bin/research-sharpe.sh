#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DECLARATION=${SHARPE_DECLARATION:-docs/research/sharpe-target-r1.json}
PHASE=${1:-screen}
CANDIDATE=${2:-}
case "$PHASE" in
  screen|confirm) ;;
  *) echo 'usage: bin/research-sharpe.sh screen | confirm [CANDIDATE]' >&2; exit 2 ;;
esac
SOURCE=$(jq -er '.source_configs' "$DECLARATION")
CONFIGS=$(jq -er '.config_root' "$DECLARATION")
PREFIX=$(jq -er '.output_prefix' "$DECLARATION")
SOURCE_SUITE=$(jq -er '.source_suite' "$DECLARATION")
CP=service/target/release/crypto-portfolio
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio
SELECTION="$PREFIX-selection.json"
if [[ "$PHASE" == confirm ]]; then
  SCREEN="$PREFIX-screen"
  "$CP" experiment-register --manifest "$SCREEN/experiments.json" --out "$SCREEN/selection-register.json"
  cmp "$SCREEN/register.json" "$SCREEN/selection-register.json"
  jq -e -s '.[0] as $d | .[1] as $r |
    ([$d.hypotheses[].id]|sort)==([$r.experiments[].experiment.id]|sort) and
    all($r.experiments[]; .experiment.expected_folds==3 and (.folds|length)==3 and
      all(.folds[]; .provenance.status=="verified_suite_linkage"))' "$DECLARATION" "$SCREEN/register.json" >/dev/null
  WINNER=$(jq -er '[.experiments[]|select((.pooled_daily_sharpe|type)=="number" and all(.folds[]; (.total_return|tonumber)>0))] |
    sort_by(-.pooled_daily_sharpe,.experiment.id) | .[0].experiment.id // error("no eligible screen candidate")' "$SCREEN/register.json")
  [[ "$WINNER" != control ]] || { echo 'Control won the screen; no improvement selected' >&2; exit 1; }
  [[ -z "$CANDIDATE" || "$CANDIDATE" == "$WINNER" ]] || { echo "Declared selection requires $WINNER, not $CANDIDATE" >&2; exit 1; }
  CANDIDATE=$WINNER
  REGISTER_HASH=$(sha256sum "$SCREEN/register.json" | cut -d' ' -f1)
  DECLARATION_HASH=$(sha256sum "$DECLARATION" | cut -d' ' -f1)
  jq -n --arg candidate "$CANDIDATE" --arg register "$SCREEN/register.json" --arg register_sha256 "$REGISTER_HASH" \
    --arg declaration_sha256 "$DECLARATION_HASH" \
    '{candidate:$candidate,screen_register:$register,screen_register_sha256:$register_sha256,declaration_sha256:$declaration_sha256,
      rule:"Positive return in every screen fold, then highest pooled Sharpe; ID breaks ties. Dates already exposed."}' > "$SELECTION"
fi
OUT="$PREFIX-$PHASE${CANDIDATE:+-$CANDIDATE}"
MANIFEST="$OUT-suite.json"
mkdir -p "$CONFIGS"
while IFS= read -r id; do
  mkdir -p "$CONFIGS/$id"
  jq -r --arg id "$id" '.hypotheses[]|select(.id==$id)|["portfolio","costs"][] as $section | (.[$section]//{}|to_entries[]) | "\($section) \(.key) \(.value|tojson)"' "$DECLARATION" > "$CONFIGS/$id/overrides.txt"
  for original in 6 7 8 9; do
    awk 'FILENAME==ARGV[1] {s="[" $1 "]"; patch[s]=patch[s] $2 " = " $3 "\n"; keys[s SUBSEP $2]=1; next}
      /^\[/ {section=$0}
      (section SUBSEP $1) in keys {next}
      {print}
      /^\[/ {printf "%s", patch[section]}' \
      "$CONFIGS/$id/overrides.txt" "$SOURCE/config-$original.toml" > "$CONFIGS/$id/config-$((original-5)).toml"
  done
done < <(jq -r '.hypotheses[].id' "$DECLARATION")
jq -s --arg phase "$PHASE" --arg candidate "$CANDIDATE" --arg out "$OUT" '
  .[0] as $d | .[1] as $s |
  {source_dir:($d.config_root+"/control"),data_root:$s.data_root,out_dir:$out,
   folds:[$s.folds[]|select(.id>=6 and (.id<=8 or $phase=="confirm"))|.id-=5],
   scenarios: (if $phase=="screen" then
     [$d.hypotheses[]|{id:.id,config_dir:($d.config_root+"/"+.id),initial_cash:"100000",execution:($s.scenarios[]|select(.id=="impact")|.execution)}]
   else
     [["control",$candidate][] as $id | ["impact","stress","delay"][] as $cost |
       {id:($id+"-"+$cost),config_dir:($d.config_root+"/"+$id),initial_cash:"100000",execution:($s.scenarios[]|select(.id==$cost)|.execution)}]
   end)}' "$DECLARATION" "$SOURCE_SUITE" > "$MANIFEST"
"$CP" replay-suite --manifest "$MANIFEST"
mkdir -p "$OUT/monte-carlo"
jq --arg declaration "$DECLARATION" --arg manifest "$MANIFEST" --arg phase "$PHASE" --arg selection "$SELECTION" \
  '{experiments:[.scenarios[] as $s | {id:$s.id,family:"crypto-sharpe-target",status:"development",trial_count:1,notes:["Declared policy trial on exposed dates."],fold_dir:(.out_dir+"/"+$s.id),expected_folds:(.folds|length),initial_cash:$s.initial_cash,sources:([$declaration,$manifest,(.out_dir+"/inputs.json")]+if $phase=="confirm" then [$selection] else [] end)}]}' \
  "$MANIFEST" > "$OUT/experiments.json"
"$CP" experiment-register --manifest "$OUT/experiments.json" --out "$OUT/register.json"
while IFS= read -r id; do
  for block in 5 20 60; do
    "$CP" monte-carlo --folds "$OUT/$id" --out "$OUT/monte-carlo/$id-block$block.json" \
      --block "$block" --iterations 10000 --seed 42 --horizon-days 365
  done
done < <(jq -r '.scenarios[].id' "$MANIFEST")
jq -r '.experiments[]|[.experiment.id,.pooled_daily_sharpe,([.folds[].total_return|tonumber]|all(.>0))]|@tsv' "$OUT/register.json"
if [[ "$PHASE" == confirm ]]; then
  for cost in impact stress delay; do
    "$CP" monte-carlo --folds "$OUT/$CANDIDATE-$cost" --benchmark "$OUT/control-$cost" \
      --out "$OUT/monte-carlo/paired-$cost.json" --block 20 --iterations 10000 --seed 42 --horizon-days 365
  done
  jq --arg id "$CANDIDATE-impact" '.experiments[]|select(.experiment.id==$id)|
    {candidate:.experiment.id,pooled_sharpe:.pooled_daily_sharpe,latest_fold_return:(.folds[-1].total_return|tonumber),
      development_target_met:(.pooled_daily_sharpe>1.5 and (.folds[-1].total_return|tonumber)>0),
      independent_validation:false}' "$OUT/register.json" > "$OUT/verdict.json"
  cat "$OUT/verdict.json"
  jq -e '.development_target_met' "$OUT/verdict.json" >/dev/null
fi
