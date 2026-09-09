#!/usr/bin/env bash
set -euo pipefail
for f in 1 2 3 4; do
  old=var/research/lighter-quoted-cost-replay/quoted-spread/fold-$f.json
  current=var/research/sharpe-target-r4-confirm-score-low-fee/control-impact/fold-$f.json
  for file in "$old" "$current"; do
    jq -e '(.steps|length)>200 and all(.steps[]; (.nav|type)=="string" and (.fills|type)=="array" and (.attribution|type)=="object")' "$file" >/dev/null
  done
  cmp <(jq '[.steps[]|{as_of,nav,cash,fills,attribution,gross_exposure,net_exposure}]' "$old") \
      <(jq '[.steps[]|{as_of,nav,cash,fills,attribution,gross_exposure,net_exposure}]' "$current")
done
echo 'All four repeated quoted-cost control NAVs, cash, fills, attribution and exposure paths match prior replay.'
