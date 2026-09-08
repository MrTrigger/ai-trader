#!/usr/bin/env bash
# Rebuild declared execution scenarios and their uncertainty reports.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUITE="${STRATEGY_TEST_SUITE:-docs/research/execution-suite.json}"
OUT=$(jq -er '.out_dir' "$SUITE")
CP=service/target/release/crypto-portfolio
case "${1:-}" in
  '')
    cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio
    "$CP" replay-suite --manifest "$SUITE"
    ;;
  --reports-only) test -x "$CP" ;;
  *) echo 'usage: bin/test-strategies.sh [--reports-only]' >&2; exit 2 ;;
esac
mkdir -p "$OUT/monte-carlo"
jq -s --arg suite "$SUITE" '
  .[0] as $history | .[1] as $run |
  $history | .experiments += [ $run.scenarios[] | {
    id: ("crypto-execution-" + .id), family: "crypto-ranker-execution",
    status: "development", trial_count: 1,
    notes: ["Predeclared execution sensitivity on frozen models and exposed dates. Not an alpha selection or calibrated venue forecast."],
    corrections: ["First-day costs, realized impact and spread costs, and reconciled daily attribution."],
    fold_dir: ($run.out_dir + "/" + .id), expected_folds: ($run.folds | length),
    initial_cash: .initial_cash,
    sources: [$suite, ($run.out_dir + "/inputs.json")]
  } ]' docs/research/experiments.json "$SUITE" > "$OUT/experiments-manifest.json"
"$CP" experiment-register --manifest "$OUT/experiments-manifest.json" --out "$OUT/register.json"
while IFS= read -r scenario; do
  for block in 5 20 60; do
    "$CP" monte-carlo --folds "$OUT/$scenario" --out "$OUT/monte-carlo/$scenario-block$block.json" \
      --iterations 10000 --seed 42 --block "$block" --horizon-days 365
  done
  "$CP" monte-carlo --folds "$OUT/$scenario" --out "$OUT/monte-carlo/$scenario-recent.json" \
    --iterations 10000 --seed 42 --block 20 --horizon-days 365 --last-folds 4
  for block in 5 60; do
    "$CP" monte-carlo --folds "$OUT/$scenario" --out "$OUT/monte-carlo/$scenario-recent-block$block.json" \
      --iterations 10000 --seed 42 --block "$block" --horizon-days 365 --last-folds 4
  done
  if [[ "$scenario" != flat ]]; then
    "$CP" monte-carlo --folds "$OUT/$scenario" --benchmark "$OUT/flat" \
      --out "$OUT/monte-carlo/$scenario-vs-flat.json" \
      --iterations 10000 --seed 42 --block 20 --horizon-days 365
  fi
done < <(jq -r '.scenarios[].id' "$SUITE")
# Compare the frozen directional candidate without selecting blend weights.
"$CP" monte-carlo --folds "$OUT/impact" --benchmark var/research/directional-r2-v6 \
  --initial-cash 100000 --out "$OUT/monte-carlo/ranker-vs-v6-legacy.json" \
  --iterations 10000 --seed 42 --block 20 --horizon-days 365
# Repeat one report exactly; provenance and random draws must be reproducible.
"$CP" monte-carlo --folds "$OUT/impact" --out "$OUT/monte-carlo/repeat.json" \
  --iterations 10000 --seed 42 --block 20 --horizon-days 365
cmp "$OUT/monte-carlo/impact-block20.json" "$OUT/monte-carlo/repeat.json"
rm "$OUT/monte-carlo/repeat.json"
NQ_OUT=var/research/nq-testing-2026-09
ROLL_MANIFEST=../trading-journal/backtest/data/bars/_manifests/20260818T160848Z-MNQ-300.json
if [[ -f var/futures/bars.jsonl && -f "$ROLL_MANIFEST" ]]; then
  cargo build --release --manifest-path service/Cargo.toml -p futures-bot
  mkdir -p "$NQ_OUT"
  env -u DATABASE_URL service/target/release/futures-bot research-replay \
    --bars var/futures/bars.jsonl --start 2026-06-01 --end 2026-08-03 \
    --research-out "$NQ_OUT/parity.json" --initial-cash 100000 \
    --fixture bots/futures/parity-fixture.json --roll-manifest "$ROLL_MANIFEST"
  env -u DATABASE_URL service/target/release/futures-bot research-replay \
    --bars var/futures/bars.jsonl --start 2026-03-02 --end 2026-07-29 \
    --research-out "$NQ_OUT/fold-1.json" --initial-cash 100000 --roll-manifest "$ROLL_MANIFEST"
  sha256sum var/futures/bars.jsonl "$ROLL_MANIFEST" service/target/release/futures-bot > "$NQ_OUT/inputs.sha256"
  "$CP" monte-carlo --folds "$OUT/impact" --benchmark "$NQ_OUT" \
    --start 2026-03-03 --end 2026-07-30 --out "$OUT/monte-carlo/ranker-vs-nq.json" \
    --iterations 10000 --seed 42 --block 20 --horizon-days 365
else
  echo 'NQ comparison unavailable: local bars or documented roll manifest missing' >&2
fi
printf 'Verified register, scenarios and deterministic Monte Carlo: %s\n' "$OUT"
