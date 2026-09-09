#!/usr/bin/env bash
# Raw hourly funding for the fixed Nov 27, 2025–Jul 30, 2026 baseline.
# Usage: bin/pull-lighter-funding.sh SELECTED_MARKETS_JSON OUTPUT_DIR
# Include one day before first eligibility (no earlier than Nov 26), and keep
# fetching through Jul 31 00:00 for every selected name, even after eligibility
# ends: an incompletely exited position can still owe funding. Two workers.
set -euo pipefail
SCRIPT=$(realpath -- "${BASH_SOURCE[0]}")
if [[ "${1:-}" == --page ]]; then
  OUT=$2
  IFS=$'\t' read -r id start end symbol <<< "$3"
  stem="market-$id-$start-$end"
  target="$OUT/$stem.json"
  if [[ -f "$target.sha256" ]] && sha256sum -c "$target.sha256" >/dev/null 2>&1 && jq -e '.code==200 and .resolution=="1h" and (.fundings|type)=="array"' "$target" >/dev/null 2>&1; then exit 0; fi
  count=$(((end-start)/3600+2))
  url="https://mainnet.zklighter.elliot.ai/api/v1/fundings?market_id=$id&resolution=1h&start_timestamp=$((start-3600))&end_timestamp=$((end+3600))&count_back=$count"
  for attempt in 1 2 3 4; do
    # Reserve request starts globally, independent of individual HTTP latency.
    (
      flock 9
      now=$(date +%s%3N)
      last=0
      [[ ! -f "$OUT/request-clock" ]] || read -r last < "$OUT/request-clock"
      remaining=$((last+600-now))
      if ((remaining>0)); then sleep "$(printf '%d.%03d' "$((remaining/1000))" "$((remaining%1000))")"; fi
      date +%s%3N > "$OUT/request-clock"
    ) 9> "$OUT/request-rate.lock"
    raw="$OUT/attempts/$stem-$attempt-$(date +%s%N)"
    at=$(date -u +%FT%TZ)
    code=$(curl --silent --show-error --connect-timeout 5 --max-time 30 -D "$raw.headers" "$url" -o "$raw.json" -w '%{http_code}') || code=000
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$at" "$id" "$symbol" "$attempt" "$code" "$url" >> "$OUT/requests.tsv"
    if [[ "$code" == 200 ]] && jq -e '.code==200 and .resolution=="1h" and (.fundings|type)=="array"' "$raw.json" >/dev/null 2>&1; then
      cp "$raw.json" "$target"
      sha256sum "$target" > "$target.sha256"
      printf 'funding market %s %s %s..%s saved\n' "$id" "$symbol" "$start" "$end"
      exit 0
    fi
    sleep "$((2**attempt))"
  done
  printf 'funding page failed: %s\n' "$stem" >&2
  exit 1
fi

SELECTED=${1:?usage: pull-lighter-funding.sh SELECTED_MARKETS_JSON OUTPUT_DIR}
OUT=${2:?output directory required}
jq -e 'type=="array" and length>0 and
  all(.[]; (.market_id|type)=="number" and .market_id>=0 and (.market_id|floor)==.market_id and
      (.symbol|type)=="string" and (.first_eligible_decision|type)=="string") and
  ([.[].market_id]|length)==([.[].market_id]|unique|length)' "$SELECTED" >/dev/null
mkdir -p "$OUT/attempts"
OUT=$(realpath -- "$OUT")
exec 8>"$OUT/download.lock"
flock -n 8 || { echo 'this funding output is already being collected' >&2; exit 1; }
HASH=$(sha256sum "$SELECTED" | cut -d ' ' -f1)
if [[ -f "$OUT/selected-markets.sha256" ]]; then
  [[ "$(cat "$OUT/selected-markets.sha256")" == "$HASH" ]] || { echo 'selected markets changed' >&2; exit 1; }
else
  cp "$SELECTED" "$OUT/selected-markets.json"
  printf '%s\n' "$HASH" > "$OUT/selected-markets.sha256"
fi
rm -f "$OUT/COMPLETE.json"
cp "$SCRIPT" "$OUT/downloader.sh"
GLOBAL_START=$(date -u --date=2025-11-26 +%s)
GLOBAL_END=$(date -u --date=2026-07-31 +%s)
: > "$OUT/jobs.tsv"
while IFS=$'\t' read -r id symbol first last; do
  start=$(date -u --date="$first" +%s)
  ((start % 86400 == 0 && start < GLOBAL_END)) || { echo "invalid first decision for market $id" >&2; exit 1; }
  start=$((start-86400))
  ((start>=GLOBAL_START)) || start=$GLOBAL_START
  finish=$GLOBAL_END
  while ((start<finish)); do
    end=$((start+700*3600))
    ((end<=finish)) || end=$finish
    printf '%s\t%s\t%s\t%s\n' "$id" "$start" "$end" "$symbol" >> "$OUT/jobs.tsv"
    start=$end
  done
done < <(jq -er '.[]|[.market_id,.symbol,.first_eligible_decision,.last_eligible_decision]|@tsv' "$SELECTED")
[[ -s "$OUT/jobs.tsv" ]] || { echo 'no declared funding jobs' >&2; exit 1; }
sha256sum "$OUT/selected-markets.json" "$OUT/jobs.tsv" "$OUT/downloader.sh" > "$OUT/inputs.sha256"
xargs -d '\n' -n 1 -P 2 bash "$SCRIPT" --page "$OUT" < "$OUT/jobs.tsv"
while IFS=$'\t' read -r id start end symbol; do
  sha256sum -c "$OUT/market-$id-$start-$end.json.sha256" >/dev/null
  jq -e '.code==200 and .resolution=="1h" and (.fundings|type)=="array"' "$OUT/market-$id-$start-$end.json" >/dev/null
done < "$OUT/jobs.tsv"
sha256sum -c "$OUT/inputs.sha256" >/dev/null
jq -n --arg at "$(date -u +%FT%TZ)" --arg selected_sha256 "$HASH" --argjson pages "$(wc -l < "$OUT/jobs.tsv")" \
  '{schema:"lighter-native-hourly-funding-collection-1",completed_at:$at,selected_sha256:$selected_sha256,pages:$pages,resolution:"1h",notes:["Every declared request succeeded and its raw response hash verified.","Requests pad one hour on each side and may overlap; filter and deduplicate timestamps.","Empty responses are preserved. Completion does not assert every possible hour has a funding event; replay must verify its held hours."]}' > "$OUT/COMPLETE.json"
printf 'Verified all %s hourly funding pages\n' "$(wc -l < "$OUT/jobs.tsv")"
