#!/usr/bin/env bash
# Reprice frozen recent folds with zero commission and a 2.2 bp half-spread.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${1:-}" in
  '') lighter_replay=true ;;
  --reports-only) lighter_replay=false ;;
  *) echo 'usage: bin/replay-lighter-quoted-cost.sh [--reports-only]' >&2; exit 2 ;;
esac
lighter_prefix=${LIGHTER_QUOTED_PREFIX:-var/research/lighter-quoted-cost}
lighter_configs="$lighter_prefix-configs"
lighter_out="$lighter_prefix-replay"
if [[ -n ${LIGHTER_QUOTED_PREFIX:-} ]]; then
  lighter_suite="$lighter_prefix-suite.json"
else
  lighter_suite=docs/research/lighter-quoted-spread-suite.json
fi
lighter_binary=service/target/release/crypto-portfolio
lighter_depth=var/research/sharpe-target-probes/lighter/depth-summary-2026-09-08.json
lighter_benchmarks=var/research/sharpe-target-r1-confirm-zero-fee
test -x "$lighter_binary"
test -f "$lighter_depth"
lighter_tmp=$(mktemp -d)
trap 'rm -rf "$lighter_tmp"' EXIT

# Preserve completed evidence when current inputs or reporting code have changed.
keep_file() {
  local candidate=$1 destination=$2
  if [[ -e "$destination" ]]; then
    if ! cmp -s "$candidate" "$destination"; then
      echo "$destination differs; choose a fresh LIGHTER_QUOTED_PREFIX" >&2
      exit 1
    fi
    rm "$candidate"
  else
    mv "$candidate" "$destination"
  fi
}

if "$lighter_replay"; then
  mkdir -p "$lighter_configs" "$(dirname "$lighter_suite")"
  for lighter_fold in 1 2 3 4; do
    lighter_original=$((lighter_fold + 5))
    lighter_source="var/research/wf-rank-2020/config-$lighter_original.toml"
    [[ $(rg -c '^commission_bps = ' "$lighter_source") == 1 ]]
    [[ $(rg -c '^spread_bps = ' "$lighter_source") == 1 ]]
    awk '/^commission_bps = / {next}
      /^spread_bps = / {$0="spread_bps = \"2.2\""}
      {print}
      /^\[costs\]$/ {print "commission_bps = \"0\""}' \
      "$lighter_source" > "$lighter_tmp/config.toml"
    keep_file "$lighter_tmp/config.toml" "$lighter_configs/config-$lighter_fold.toml"
  done
  jq --arg source "$lighter_configs" --arg out "$lighter_out" '
    {source_dir:$source,data_root:.data_root,out_dir:$out,
     folds:(.folds[5:9]|to_entries|map(.value + {id:(.key+1)})),
     scenarios:[.scenarios[]|select(.id=="impact")|.id="quoted-spread"]}
  ' docs/research/execution-suite.json > "$lighter_tmp/suite.json"
  if [[ -f "$lighter_suite" ]]; then
    jq -S . "$lighter_suite" > "$lighter_tmp/existing-suite.json"
    jq -S . "$lighter_tmp/suite.json" > "$lighter_tmp/generated-suite.json"
    cmp "$lighter_tmp/existing-suite.json" "$lighter_tmp/generated-suite.json"
  else
    mv "$lighter_tmp/suite.json" "$lighter_suite"
  fi
  # Use the existing binary. A new build requires a fresh output prefix.
  "$lighter_binary" replay-suite --manifest "$lighter_suite"
fi

mkdir -p "$lighter_out/monte-carlo"
jq -n --arg suite_path "$lighter_suite" --arg depth "$lighter_depth" --slurpfile suite "$lighter_suite" '
  {experiments:[{id:"lighter-quoted-spread-counterfactual",family:"crypto-ranker-execution",
   status:"development",trial_count:1,
   notes:["Zero commission with constant 2.2bp half-spread motivated by a current Lighter snapshot, on original historical universe/funding/impact assumptions. Exposed dates; not a venue replay."],
   fold_dir:($suite[0].out_dir+"/quoted-spread"),expected_folds:($suite[0].folds|length),
   initial_cash:"100000",sources:[$suite_path,($suite[0].out_dir+"/inputs.json"),$depth]}]}
' > "$lighter_tmp/experiments-manifest.json"
keep_file "$lighter_tmp/experiments-manifest.json" "$lighter_out/experiments-manifest.json"
"$lighter_binary" experiment-register --manifest "$lighter_out/experiments-manifest.json" --out "$lighter_tmp/register.json"
keep_file "$lighter_tmp/register.json" "$lighter_out/register.json"
for lighter_block in 5 20 60; do
  "$lighter_binary" monte-carlo --folds "$lighter_out/quoted-spread" \
    --iterations 10000 --seed 42 --block "$lighter_block" --horizon-days 365 \
    --out "$lighter_tmp/report.json"
  keep_file "$lighter_tmp/report.json" "$lighter_out/monte-carlo/block$lighter_block.json"
done
for lighter_benchmark in control-impact zero-fee-impact; do
  "$lighter_binary" monte-carlo --folds "$lighter_out/quoted-spread" \
    --benchmark "$lighter_benchmarks/$lighter_benchmark" \
    --iterations 10000 --seed 42 --block 20 --horizon-days 365 \
    --out "$lighter_tmp/report.json"
  keep_file "$lighter_tmp/report.json" "$lighter_out/monte-carlo/paired-$lighter_benchmark.json"
done
