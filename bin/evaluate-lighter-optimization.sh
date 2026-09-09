#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DECLARATION=${1:?usage: evaluate-lighter-optimization.sh DECLARATION_JSON}
OUT=$(jq -er .output "$DECLARATION")
[[ ! -e "$OUT/evaluation" ]] || { echo "evaluation already exists: $OUT" >&2; exit 1; }
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio --bin crypto-portfolio --example lighter-evaluation
mkdir -p "$OUT/evaluation"
cp service/target/release/examples/lighter-evaluation "$OUT/evaluation/evaluator"
cp service/target/release/crypto-portfolio "$OUT/evaluation/portfolio"
cp "${BASH_SOURCE[0]}" "$OUT/evaluation/runner.sh"
sha256sum "$OUT/evaluation/evaluator" "$OUT/evaluation/portfolio" "$OUT/evaluation/runner.sh" \
  service/crates/crypto-portfolio/examples/lighter-evaluation.rs \
  "$OUT/suite.json" "$DECLARATION" > "$OUT/evaluation/source.sha256"
RAW=$(jq -er .lighter_execution.daily "$OUT/suite.json")
mapfile -t HOURLY_ROOTS < <(jq -r '.lighter_execution | .hourly, (.hourly_extensions // [])[]' "$OUT/suite.json")
while IFS= read -r ID; do
  "$OUT/evaluation/evaluator" "$RAW" "$OUT/replay/$ID/fold-1.json" \
    "$OUT/evaluation/$ID" "${HOURLY_ROOTS[@]}" > "$OUT/evaluation/$ID.log" 2>&1
  "$OUT/evaluation/portfolio" monte-carlo \
    --folds "$OUT/evaluation/$ID/strategy/full" \
    --benchmark "$OUT/evaluation/$ID/btc-price/full" \
    --out "$OUT/evaluation/$ID/paired-btc.json" \
    --iterations 10000 --seed 42 --block 20 \
    --horizon-days "$(jq -er '.steps | length' "$OUT/replay/$ID/fold-1.json")" \
    > "$OUT/evaluation/$ID-paired.log" 2>&1
  jq -r --arg id "$ID" '.acceptance.comparisons[] | select(.part == "full" or .part == "second-half") | [$id,.part,.strategy_sharpe,.strategy_return,.btc_return,.joint_pass] | @tsv' \
    "$OUT/evaluation/$ID/summary.json"
done < <(jq -r '.scenarios[].id' "$OUT/suite.json")
