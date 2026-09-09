#!/usr/bin/env bash
set -euo pipefail
cd /home/magnus/dev/magnus/ai-trader
CFG=var/research/lighter-native-baseline-configs/config-1.toml
R44=docs/research/lighter-optimization-r44-turnover.json
for top in 80 120; do
  R=var/research/three-year-data-top$top
  until grep -q '^exit' $R/universe-rank.log; do sleep 15; done
  grep -q '^exit 0' $R/universe-rank.log || { echo "universe-rank top$top failed"; exit 1; }
  mkdir -p $R/matrix
  service/target/release/crypto-portfolio training-matrix --config $CFG --data-root $R \
    --start 2020-01-01 --end 2026-02-01 --lag-hours 1 --hold-hours 24 --market-context \
    --out $R/matrix/matrix.jsonl > $R/matrix/build.log 2>&1
  TS=$(date -u +%FT%T.%6NZ | sed 's/Z$/+00:00/')
  jq -n --arg ts "$TS" --arg top "$top" --arg r "$R" --slurpfile d $R44 '$d[0]
    | .declared_at=$ts | .output=("var/research/lighter-optimization-r47-breadth-top"+$top)
    | .data_root=$r | .matrix={path:($r+"/matrix/matrix.jsonl"), market_context:true, universe:("point-in-time Binance liquidity rank, top "+$top+", recorded into a research data root that symlinks the live bars/funding")}
    | .hypothesis=("The live universe snapshots expose only 24-40 names and the book holds 24 of them, so the ranker has almost no selection room and one-day sign noise dominates. Widen the point-in-time cross-section to the top "+$top+" by trailing liquidity, rebuild the matrix and refit per fold on that cohort, and compare the same 24-name book plus a 40-name book and a 40-name book with 1.5x hysteresis. Same folds, purge, recipe and costs. A wider cohort also changes feature rank-normalisation, so this is a cohort change, not only a selection change.")
    | .acceptance="Same rule as R44 against the R43 top-40 control (Sharpe 1.47, 137.5%, turnover 75%): central Sharpe not lower by more than 0.1, at least five of six folds positive, materially better moderate and stressed outcomes, and lower turnover."
    | .candidates=[
      {id:"sel24", reward:"per_risk", portfolio:{}, limits:{}},
      {id:"sel40", reward:"per_risk", portfolio:{ranker_selection_count:40}, limits:{}},
      {id:"sel40-hold15", reward:"per_risk", portfolio:{ranker_selection_count:40, ranker_hold_rank_multiple:"1.5"}, limits:{}}]' \
    > docs/research/lighter-optimization-r47-breadth-top$top.json
  printf '%s\thypothesis\tr47-breadth-top%s\tWiden point-in-time cohort to top %s; refit per fold; 24/40-name books and 40 with hysteresis1.5; separate research data root\tdocs/research/lighter-optimization-r47-breadth-top%s.json\tDECLARED\n' "$(date -u +%FT%TZ)" $top $top $top >> .audit/lighter-strategy-continuation.tsv
  bash bin/research-three-year.sh docs/research/lighter-optimization-r47-breadth-top$top.json > .audit/lighter-r47-top$top-run.log 2>&1
  echo "exit $?" >> .audit/lighter-r47-top$top-run.log
done
echo "chain done"
