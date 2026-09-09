#!/usr/bin/env bash
# Raw native hourly candles for the fixed Nov 27, 2025–Jul 30, 2026 baseline.
# Usage: bin/pull-lighter-hourly.sh SELECTED_MARKETS_JSON OUTPUT_DIR
# Collect every declared market from Nov 26 00:00 through Jul 31 00:00 exclusive.
# Retain pre-listing gaps and inactive padding for consumers to inspect.
# Two workers share request starts at least 600 ms apart. Run separately from
# the daily and funding collectors to preserve that combined request limit.
# A rate-limit response pauses both workers and increases spacing to 1200 ms.
set -euo pipefail
SCRIPT=$(realpath -- "${BASH_SOURCE[0]}")
if [[ "${1:-}" == --page ]]; then
  OUT=$2
  IFS=$'\t' read -r id start end symbol <<< "$3"
  stem="market-$id-$start-$end"
  target="$OUT/$stem.json"
  if [[ -f "$target.sha256" ]] && sha256sum -c "$target.sha256" >/dev/null 2>&1 && jq -e '.code==200 and (.c|type)=="array"' "$target" >/dev/null 2>&1; then exit 0; fi
  count=$(((end-start)/3600))
  url="https://mainnet.zklighter.elliot.ai/api/v1/candles?market_id=$id&resolution=1h&start_timestamp=$start&end_timestamp=$((end-1))&count_back=$count&set_timestamp_to_end=false"
  for attempt in 1 2 3 4; do
    # Reserve request starts globally, independent of individual HTTP latency.
    (
      flock 9
      now=$(date +%s%3N)
      last=0
      [[ ! -f "$OUT/request-clock" ]] || read -r last < "$OUT/request-clock"
      spacing=600
      [[ ! -f "$OUT/request-spacing-ms" ]] || read -r spacing < "$OUT/request-spacing-ms"
      remaining=$((last+spacing-now))
      if ((remaining>0)); then sleep "$(printf '%d.%03d' "$((remaining/1000))" "$((remaining%1000))")"; fi
      date +%s%3N > "$OUT/request-clock"
    ) 9> "$OUT/request-rate.lock"
    raw="$OUT/attempts/$stem-$attempt-$(date +%s%N)"
    at=$(date -u +%FT%TZ)
    code=$(curl --silent --show-error --connect-timeout 5 --max-time 30 -D "$raw.headers" "$url" -o "$raw.json" -w '%{http_code}') || code=000
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$at" "$id" "$symbol" "$attempt" "$code" "$url" >> "$OUT/requests.tsv"
    if [[ "$code" == 200 ]] && jq -e '.code==200 and (.c|type)=="array"' "$raw.json" >/dev/null 2>&1; then
      cp "$raw.json" "$target"
      sha256sum "$target" > "$target.sha256"
      printf 'hourly market %s %s %s..%s saved\n' "$id" "$symbol" "$start" "$end"
      exit 0
    fi
    if [[ "$code" == 429 ]]; then
      cooldown=30
      retry_after=$(awk 'tolower($1)=="retry-after:" {sub(/^[^:]*:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit}' "$raw.headers")
      if [[ "$retry_after" =~ ^[0-9]+$ ]]; then
        ((retry_after<=cooldown)) || cooldown=$retry_after
      elif [[ -n "$retry_after" ]]; then
        retry_epoch=$(date -u --date="$retry_after" +%s 2>/dev/null) || retry_epoch=0
        requested_wait=$((retry_epoch-$(date +%s)))
        ((requested_wait<=cooldown)) || cooldown=$requested_wait
      fi
      (
        flock 9
        resume=$(( $(date +%s%3N) + cooldown*1000 ))
        previous=0
        [[ ! -f "$OUT/request-clock" ]] || read -r previous < "$OUT/request-clock"
        ((resume>=previous)) || resume=$previous
        printf '%s\n' "$resume" > "$OUT/request-clock"
        printf '1200\n' > "$OUT/request-spacing-ms"
      ) 9> "$OUT/request-rate.lock"
    fi
    sleep "$((2**attempt))"
  done
  printf 'hourly page failed: %s\n' "$stem" >&2
  exit 1
fi

SELECTED=${1:?usage: pull-lighter-hourly.sh SELECTED_MARKETS_JSON OUTPUT_DIR}
OUT=${2:?output directory required}
jq -e 'type=="array" and length>0 and
  all(.[]; (.market_id|type)=="number" and .market_id>=0 and (.market_id|floor)==.market_id and
      (.symbol|type)=="string") and
  ([.[].market_id]|length)==([.[].market_id]|unique|length)' "$SELECTED" >/dev/null
mkdir -p "$OUT/attempts"
OUT=$(realpath -- "$OUT")
exec 8>"$OUT/download.lock"
flock -n 8 || { echo 'this hourly output is already being collected' >&2; exit 1; }
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
while IFS=$'\t' read -r id symbol; do
  start=$GLOBAL_START
  finish=$GLOBAL_END
  while ((start<finish)); do
    end=$((start+480*3600))
    ((end<=finish)) || end=$finish
    printf '%s\t%s\t%s\t%s\n' "$id" "$start" "$end" "$symbol" >> "$OUT/jobs.tsv"
    start=$end
  done
done < <(jq -er '.[]|[.market_id,.symbol]|@tsv' "$SELECTED")
[[ -s "$OUT/jobs.tsv" ]] || { echo 'no declared hourly jobs' >&2; exit 1; }
sha256sum "$OUT/selected-markets.json" "$OUT/jobs.tsv" "$OUT/downloader.sh" > "$OUT/inputs.sha256"
xargs -d '\n' -n 1 -P 2 bash "$SCRIPT" --page "$OUT" < "$OUT/jobs.tsv"
while IFS=$'\t' read -r id start end symbol; do
  sha256sum -c "$OUT/market-$id-$start-$end.json.sha256" >/dev/null
  jq -e '.code==200 and (.c|type)=="array"' "$OUT/market-$id-$start-$end.json" >/dev/null
done < "$OUT/jobs.tsv"
sha256sum -c "$OUT/inputs.sha256" >/dev/null
jq -n --arg at "$(date -u +%FT%TZ)" --arg selected_sha256 "$HASH" --argjson pages "$(wc -l < "$OUT/jobs.tsv")" \
  '{schema:"lighter-native-hourly-candle-collection-1",completed_at:$at,selected_sha256:$selected_sha256,pages:$pages,resolution:"1h",start_inclusive:"2025-11-26T00:00:00Z",end_exclusive:"2026-07-31T00:00:00Z",timestamp_semantics:"UTC bar start, milliseconds",notes:["Every declared request succeeded and its raw response hash verified.","Raw responses can extend outside requested windows; filter and deduplicate timestamps.","Empty responses and zero-volume padding are preserved. Completion does not assert valid OHLC, continuous history or tradability."]}' > "$OUT/COMPLETE.json"
printf 'Verified all %s hourly candle pages\n' "$(wc -l < "$OUT/jobs.tsv")"
