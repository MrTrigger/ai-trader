#!/usr/bin/env bash
# Fixed model and portfolio rules, with venue-first historical selection.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHASE=${1:-native}
case "$PHASE" in native|proxy) ;; *) echo 'usage: replay-lighter-native.sh [native|proxy] [OUTPUT_PREFIX]' >&2; exit 2 ;; esac
PREFIX=${2:-var/research/lighter-native-baseline}
DATA=var/research/lighter-native-universe
DAILY=var/research/lighter-history-audit/daily-2026-09-08
FUNDING=var/research/lighter-history-audit/funding-selected-2025nov-2026jul
CP=service/target/release/crypto-portfolio
[[ -x "$CP" && -s "$DATA/coverage.json" ]] || { echo 'build the planner and historical universes first' >&2; exit 1; }
jq -e '.start=="2025-11-27T00:00:00Z" and .end=="2026-07-30T00:00:00Z" and .top_n==30 and (.decisions|length)==246' "$DATA/coverage.json" >/dev/null
if [[ "$PHASE" == native ]]; then
  [[ -s "$FUNDING/COMPLETE.json" ]] || { echo 'native hourly funding collection is incomplete' >&2; exit 1; }
fi
for name in bars funding; do
  SOURCE=$(realpath "data/$name")
  if [[ -e "$DATA/$name" || -L "$DATA/$name" ]]; then
    [[ -L "$DATA/$name" && "$(readlink "$DATA/$name")" == "$SOURCE" ]] || { echo "unexpected signal-data link: $DATA/$name" >&2; exit 1; }
  else
    ln -s "$SOURCE" "$DATA/$name"
  fi
done
CONFIGS="$PREFIX-configs"
OUT="$PREFIX-$PHASE"
mkdir -p "$CONFIGS" "$(dirname "$OUT")"
TMP=$(mktemp -d)
trap 'rm -rf -- "$TMP"' EXIT
keep() {
  if [[ -f "$2" ]]; then cmp "$1" "$2"; else cp "$1" "$2"; fi
}
awk '/^model_path = / {$0="model_path = \"var/research/wf-rank-2020/model-9.json\""} {print}' \
  deploy/lighter-config.toml > "$TMP/config.toml"
keep "$TMP/config.toml" "$CONFIGS/config-1.toml"
jq -n --arg data "$DATA" --arg configs "$CONFIGS" --arg out "$OUT" --arg phase "$PHASE" \
  --arg daily "$DAILY" --arg funding "$FUNDING" '{
    source_dir:$configs,data_root:$data,out_dir:$out,
    folds:[{id:1,start:"2025-11-27T00:00:00Z",end:"2026-07-30T00:00:00Z"}],
    scenarios:[{id:"central",initial_cash:"100000",execution:{model:"impact",delay_hours:0,participation:0.01,impact_coefficient:0.1,spread_multiple:1.0,impact_multiple:1.0,half_spread_bps:{}}},
      {id:"stress",initial_cash:"100000",execution:{model:"impact",delay_hours:0,participation:0.01,impact_coefficient:0.1,spread_multiple:10.0,impact_multiple:4.0,half_spread_bps:{}}}]
  } + if $phase=="native" then {lighter_execution:{daily:$daily,funding:$funding}} else {} end' > "$TMP/suite.json"
keep "$TMP/suite.json" "$OUT-suite.json"
"$CP" replay-suite --manifest "$OUT-suite.json"
jq --arg phase "$PHASE" --arg manifest "$OUT-suite.json" --arg coverage "$DATA/coverage.json" '
  {experiments:[.scenarios[] as $s | {id:($phase+"-"+$s.id),family:"lighter-historical-baseline",status:"development",trial_count:1,
    notes:["Fixed fold-9 ranker; deployed Lighter limits; historical venue activity before top30 ranking.",
      (if $phase=="native" then "Native daily opens and hourly funding; assumed spread/impact and native trailing liquidity proxies." else "External execution prices and funding proxies; not a native venue performance claim." end)],
    fold_dir:(.out_dir+"/"+$s.id),expected_folds:1,initial_cash:$s.initial_cash,
    sources:[$manifest,$coverage,(.out_dir+"/inputs.json")]}]}' "$OUT-suite.json" > "$TMP/experiments.json"
keep "$TMP/experiments.json" "$OUT/experiments.json"
"$CP" experiment-register --manifest "$OUT/experiments.json" --out "$OUT/register.json"
mkdir -p "$OUT/monte-carlo"
for id in central stress; do
  "$CP" monte-carlo --folds "$OUT/$id" --out "$OUT/monte-carlo/$id.json" \
    --block 20 --iterations 10000 --seed 42 --horizon-days 365
done
jq -r '.experiments[]|[.experiment.id,.pooled_daily_sharpe,.folds[0].total_return]|@tsv' "$OUT/register.json"
