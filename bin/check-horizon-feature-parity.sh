#!/usr/bin/env bash
set -euo pipefail
p=var/research/sharpe-horizon-final
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
for h in 24 72; do
  jq -r 'select(.asset)|[([.date,.slot,.asset]|map(tostring)|join(":")),(.features|tojson)]|@tsv' "$p/matrix-$h.jsonl" |
    LC_ALL=C sort -t $'\t' -k1,1 > "$scratch/$h.tsv"
done
LC_ALL=C join -t $'\t' "$scratch/24.tsv" "$scratch/72.tsv" |
  awk -F '\t' '{n++; if ($2!=$3) bad++} END {print "common_rows=" n, "different_feature_vectors=" bad+0; exit(bad>0)}'
