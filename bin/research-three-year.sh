#!/usr/bin/env bash
# Three-year expanding walk-forward on Binance proxies with Lighter cost assumptions.
#
#   bin/research-three-year.sh DECLARATION_JSON
#
# The declaration fixes the matrix, fold schedule, trainer recipe, cost
# scenarios and a list of candidates. Each candidate names a trainer reward and
# [portfolio]/[limits] overrides applied to the base config. Every fold gets its
# own model fitted only on rows dated on or before that fold's train_through.
# Outputs are never overwritten: an existing output directory aborts the run.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
D=${1:?usage: research-three-year.sh DECLARATION_JSON}
OUT=$(jq -er .output "$D")
[[ ! -e "$OUT" ]] || { echo "output already exists: $OUT" >&2; exit 1; }
PY=training/.venv/bin/python
CP=service/target/release/crypto-portfolio
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio \
  --bin crypto-portfolio --example buy_hold_comparison --example equity-series >/dev/null
mkdir -p "$OUT"
cp "$D" "$OUT/declaration.json"
cp "${BASH_SOURCE[0]}" "$OUT/runner.sh"
cp -p "$CP" "$OUT/portfolio"
MATRIX=$(jq -er .matrix.path "$D")
BASE=$(jq -er .base_config "$D")
sha256sum "$MATRIX" "$BASE" training/train.py "$OUT/portfolio" > "$OUT/inputs.sha256"
say() { echo "[$(date -u +%H:%M:%S)] $*"; }

override() { # text section key json-value -> stdout
  printf '%s\n' "$1" | awk -v sec="[$2]" -v key="$3" -v val="$4" '
    BEGIN{insec=0;done=0}
    /^\[/{ if(insec && !done){print key" = "val; done=1} insec=($0==sec) }
    { if(insec && $0 ~ "^"key"[ \t]*=") next; print }
    END{ if(insec && !done) print key" = "val }'
}

NFOLDS=$(jq -r '.folds|length' "$D")
scenarios='[]'
for cid in $(jq -r '.candidates[].id' "$D"); do
  reward=$(jq -er --arg c "$cid" '.candidates[]|select(.id==$c)|.reward' "$D")
  seeds=$(jq -er .trainer.seeds "$D")
  objective=$(jq -er .trainer.objective "$D")
  mkdir -p "$OUT/models/$cid" "$OUT/configs/$cid"
  # A candidate may name its own matrix (for example a longer label horizon)
  # and its own purge: `train_through_offset_days` replaces the fold schedule's
  # cutoffs with fold start minus that many days, so a longer label window
  # never overlaps its test block.
  cmatrix=$(jq -r --arg c "$cid" '.candidates[]|select(.id==$c)|.matrix // empty' "$D")
  MATRIX_C=${cmatrix:-$MATRIX}
  [[ -z "$cmatrix" ]] || sha256sum "$cmatrix" >> "$OUT/inputs.sha256"
  offset=$(jq -r --arg c "$cid" '.candidates[]|select(.id==$c)|.train_through_offset_days // empty' "$D")
  for n in $(seq 1 "$NFOLDS"); do
    if [[ -n "$offset" ]]; then
      through=$(date -ud "$(jq -er ".folds[$((n-1))].start" "$D" | cut -c1-10) - $offset days" +%F)
    else
      through=$(jq -er ".folds[$((n-1))].train_through" "$D")
    fi
    say "$cid fold $n: fit $reward through $through on $(basename "$MATRIX_C")"
    env -u TRAIN_DROP TRAIN_OBJECTIVE="$objective" TRAIN_SEEDS="$seeds" TRAIN_RANK=0 \
      "$PY" training/train.py --matrix "$MATRIX_C" --through "$through" \
      --reward "$reward" --out "$OUT/models/$cid/model-$n.json" > "$OUT/models/$cid/fit-$n.log" 2>&1
    text=$(cat "$BASE")
    text=$(override "$text" portfolio model_path "\"$OUT/models/$cid/model-$n.json\"")
    for section in portfolio limits; do
      for key in $(jq -r --arg c "$cid" --arg s "$section" '.candidates[]|select(.id==$c)|(.[$s] // {})|keys[]' "$D"); do
        value=$(jq -c --arg c "$cid" --arg s "$section" --arg k "$key" '.candidates[]|select(.id==$c)|.[$s][$k]' "$D")
        text=$(override "$text" "$section" "$key" "$value")
      done
    done
    printf '%s\n' "$text" > "$OUT/configs/$cid/config-$n.toml"
  done
  for cost in $(jq -r '.costs|keys[]' "$D"); do
    scenarios=$(jq -c --arg id "$cid-$cost" --arg dir "$OUT/configs/$cid" --argjson ex "$(jq -c --arg k "$cost" '.costs[$k]' "$D")" \
      '. + [{id:$id, config_dir:$dir, initial_cash:"100000", execution:$ex}]' <<<"$scenarios")
  done
done
first=$(jq -r '.candidates[0].id' "$D")
DROOT=$(jq -r '.data_root // "data"' "$D")
jq -n --arg out "$OUT" --arg dir "$OUT/configs/$first" --arg droot "$DROOT" --argjson sc "$scenarios" --slurpfile d "$D" \
  '{source_dir:$dir, data_root:$droot, lighter_execution:null, out_dir:($out+"/replay"), folds:($d[0].folds|map({id,start,end})), scenarios:$sc}' \
  > "$OUT/suite.json"
say "replay $(jq -r '.scenarios|length' "$OUT/suite.json") scenarios x $NFOLDS folds"
"$OUT/portfolio" replay-suite --manifest "$OUT/suite.json"
jq -n --arg out "$OUT" --argjson sc "$scenarios" --argjson n "$NFOLDS" '{experiments: [$sc[] | {id, family:"lighter-optimization-three-year", status:"development", trial_count:1,
  notes:["Three-year Binance proxy walk-forward with Lighter cost assumptions; expanding folds retrained per fold; not a Lighter execution estimate."],
  fold_dir:($out+"/replay/"+.id), expected_folds:$n, initial_cash:"100000", sources:[$out+"/declaration.json",$out+"/suite.json",$out+"/replay/inputs.json"]}]}' > "$OUT/experiments.json"
"$OUT/portfolio" experiment-register --manifest "$OUT/experiments.json" --out "$OUT/register.json"
firstsc=$(jq -r '.scenarios[0].id' "$OUT/suite.json")
service/target/release/examples/buy_hold_comparison "$DROOT" "$OUT/replay/$firstsc" "$OUT/buy-hold"
args=()
for id in $(jq -r '.scenarios[].id' "$OUT/suite.json"); do args+=("$id=$OUT/replay/$id"); done
service/target/release/examples/equity-series "$OUT/equity-series.json" "${args[@]}" btc="$OUT/buy-hold/btc" eth="$OUT/buy-hold/eth"
days=$(jq -r '.dates|length' "$OUT/equity-series.json")
for id in $(jq -r '.scenarios[].id' "$OUT/suite.json"); do
  "$OUT/portfolio" monte-carlo --folds "$OUT/replay/$id" --benchmark "$OUT/buy-hold/btc" --out "$OUT/paired-$id.json" \
    --iterations 10000 --seed 42 --block 20 --horizon-days "$days" --last-folds "$NFOLDS" > /dev/null
done
say "summary"
printf 'scenario\tsharpe\treturn\tmaxdd\tsharpe_p025\tsharpe_p975\n'
for id in $(jq -r '.scenarios[].id' "$OUT/suite.json"); do
  jq -r --arg id "$id" '[$id, (.original_pooled_sharpe*1000|round/1000), (.original_cumulative_return*10000|round/100), (.original_max_drawdown*10000|round/100), (.whole_sample_sharpe_bootstrap.percentile_interval.p025*1000|round/1000), (.whole_sample_sharpe_bootstrap.percentile_interval.p975*1000|round/1000)]|@tsv' "$OUT/paired-$id.json"
done
jq -r '"btc\t\t\(.curves.btc.total_return*10000|round/100)\t\(.curves.btc.max_drawdown*10000|round/100)"' "$OUT/equity-series.json"
