#!/usr/bin/env bash
# Static current-membership sensitivity, not historical venue eligibility.
set -euo pipefail

usage() {
  echo "usage: $0 [source-data-root [output-data-root [2026-09-08-membership-json]]]" >&2
}
(( $# <= 3 )) || { usage; exit 2; }
for tool in jq sha256sum realpath mktemp cmp; do
  command -v "$tool" >/dev/null || { echo "missing command: $tool" >&2; exit 1; }
done
source_root=$(realpath -e "${1:-data}")
output_root=$(realpath -m "${2:-var/research/selection-trend-lighter-data}")
membership_source=$(realpath -e "${3:-var/research/sharpe-target-probes/lighter/orderBookDetails-2026-09-08.json}")
[[ -d "$source_root/bars" && -d "$source_root/universe" && -s "$source_root/funding/binance_um.parquet" ]] || {
  echo "source requires bars/, universe/, and funding/binance_um.parquet" >&2; exit 1;
}
[[ "$output_root" != "$source_root" && "$output_root" != "$source_root/"* ]] || {
  echo "output must be outside the source data root" >&2; exit 1;
}
shopt -s nullglob globstar
bars=("$source_root"/bars/**/*.parquet)
snapshots=("$source_root"/universe/*.json)
(( ${#bars[@]} > 0 && ${#snapshots[@]} > 0 )) || { echo "missing bars or universe snapshots" >&2; exit 1; }
for file in "${snapshots[@]}"; do
  [[ "${file##*/}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}\.json$ ]] || {
    echo "unexpected universe filename: $file" >&2; exit 1;
  }
done
mkdir -p "$(dirname "$output_root")"
stage=$(mktemp -d "${output_root}.prepare.XXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/universe"
jq -e '
  if .code != 200 or (.order_book_details | type) != "array" then error("invalid membership response") else . end
  | [.order_book_details[] | select(.status == "active" and .market_type == "perp") | .symbol]
  | if length == 0 or any(.[]; type != "string" or length == 0) then error("missing native symbols")
    elif (unique | length) != length then error("duplicate native symbols") else sort end
' "$membership_source" > "$stage/membership.json"
(cd "$source_root/universe" && sha256sum -- *.json) > "$stage/input-universe.sha256"
membership_sha=$(sha256sum "$membership_source"); membership_sha=${membership_sha%% *}
universe_sha=$(sha256sum "$stage/input-universe.sha256"); universe_sha=${universe_sha%% *}

# Emit NUL-delimited filenames and JSON in one jq process; preserve all fields
# except the members removed by the exact native-symbol intersection.
jq -jr --slurpfile allowed "$stage/membership.json" '
  if (.as_of | type) != "string" or (.members | type) != "array"
     or any(.members[]; (.asset | type) != "string" or (.eligible | type) != "boolean")
  then error("invalid universe snapshot: " + input_filename) else . end
  | (input_filename | split("/")[-1]), "\u0000",
    (.members |= map(select(.asset as $asset | $allowed[0] | index($asset))) | tojson), "\u0000"
' "${snapshots[@]}" > "$stage/filtered.stream"
while IFS= read -r -d '' name && IFS= read -r -d '' snapshot; do
  printf '%s\n' "$snapshot" > "$stage/universe/$name"
done < "$stage/filtered.stream"
rm "$stage/filtered.stream"
(cd "$source_root/universe" && sha256sum --check --status "$stage/input-universe.sha256")
[[ "$(sha256sum "$membership_source")" == "$membership_sha  $membership_source" ]] || {
  echo "membership changed during preparation" >&2; exit 1;
}
(cd "$stage/universe" && sha256sum -- *.json) > "$stage/output-universe.sha256"
ln -s "$source_root/bars" "$stage/bars"
ln -s "$source_root/funding" "$stage/funding"
jq -n --arg source_root "$source_root" --arg membership_source "$membership_source" \
  --arg membership_sha256 "$membership_sha" --arg input_universe_sha256 "$universe_sha" \
  --argjson snapshots "${#snapshots[@]}" --slurpfile symbols "$stage/membership.json" '{
  schema_version: "static-lighter-membership-1",
  interpretation: "Static 2026-09-08 current-venue-membership sensitivity; not historical Lighter eligibility. Native symbol intersection only; no aliases or price rescaling.",
  membership_as_of: "2026-09-08", source_root: $source_root,
  membership_source: $membership_source, membership_sha256: $membership_sha256,
  input_universe_sha256: $input_universe_sha256,
  input_hashes: "input-universe.sha256", output_hashes: "output-universe.sha256",
  snapshot_count: $snapshots, active_native_perpetual_count: ($symbols[0] | length),
  shared_data: ["bars", "funding"],
  shared_data_note: "Symlinks share the original price and funding proxies. Freeze/hash those inputs separately for each replay; this preparation does not claim Lighter historical prices or funding."
}' > "$stage/manifest.json"
if [[ -e "$output_root" || -L "$output_root" ]]; then
  for file in manifest.json membership.json input-universe.sha256 output-universe.sha256; do
    cmp -s "$stage/$file" "$output_root/$file" || { echo "existing output differs: $output_root/$file" >&2; exit 1; }
  done
  [[ -L "$output_root/bars" && -L "$output_root/funding" &&
     "$(readlink "$output_root/bars")" == "$source_root/bars" &&
     "$(readlink "$output_root/funding")" == "$source_root/funding" ]] || {
    echo "existing output has different data links" >&2; exit 1;
  }
  (cd "$output_root/universe" && sha256sum --check --status "$output_root/output-universe.sha256")
  existing=("$output_root"/universe/*.json)
  (( ${#existing[@]} == ${#snapshots[@]} )) || { echo "existing output has extra snapshots" >&2; exit 1; }
  echo "verified unchanged: $output_root (${#snapshots[@]} snapshots)"
else
  mv -T "$stage" "$output_root"
  echo "prepared: $output_root (${#snapshots[@]} snapshots)"
fi
