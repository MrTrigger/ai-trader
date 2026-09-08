#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
strategy=${1:-var/research/sharpe-target-r4-confirm-score-low-fee/score-low-fee-impact}
out=${2:-var/research/buy-hold-2026-09}
if [[ -e "$out" ]]; then
  echo 'Choose a fresh output directory to preserve saved evidence.' >&2
  exit 1
fi
cargo run --release --manifest-path service/Cargo.toml -p crypto-portfolio --example buy_hold_comparison -- data "$strategy" "$out"
for benchmark in btc eth btc-eth-50-50; do
  service/target/release/crypto-portfolio monte-carlo --folds "$strategy" --benchmark "$out/$benchmark" \
    --out "$out/paired-$benchmark.json" --block 20 --iterations 10000 --seed 42 --horizon-days 365
done
sha256sum service/crates/crypto-portfolio/examples/buy_hold_comparison.rs bin/compare-buy-hold.sh \
  service/target/release/examples/buy_hold_comparison service/target/release/crypto-portfolio "$out/inputs.json" > "$out/SHA256SUMS"
