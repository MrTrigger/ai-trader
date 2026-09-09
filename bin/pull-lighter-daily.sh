#!/usr/bin/env bash
# Public raw daily history, including inactive contracts. No price transformations.
set -euo pipefail
# Example: bin/pull-lighter-daily.sh markets.json var/native-daily 2026-09-08
SNAPSHOT=${1:?usage: pull-lighter-daily.sh MARKET_SNAPSHOT_JSON OUTPUT_DIR EXCLUSIVE_END_DAY}
OUT=${2:?output directory required}
END_DAY=${3:?exclusive UTC end day required}
[[ "$END_DAY" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || { echo 'cutoff must be YYYY-MM-DD' >&2; exit 1; }
END=$(date -u --date="$END_DAY" +%s)
jq -e '.code == 200 and (.order_books|type) == "array" and
  ([.order_books[]|select(.market_type=="perp")] | length > 0 and
   all(.[]; (.market_id|type)=="number" and .market_id>=0 and (.market_id|floor)==.market_id and
       (.created_at|tonumber)>=0 and (.symbol|type)=="string") and
   ([.[].market_id]|length)==([.[].market_id]|unique|length))' "$SNAPSHOT" >/dev/null
mkdir -p "$OUT/attempts"
OUT=$(realpath -- "$OUT")
exec 8>"$OUT/download.lock"
flock -n 8 || { echo 'this daily output is already being collected' >&2; exit 1; }
if [[ -f "$OUT/exclusive-end-day.txt" ]]; then
  [[ "$(cat "$OUT/exclusive-end-day.txt")" == "$END_DAY" ]] || { echo 'exclusive cutoff changed' >&2; exit 1; }
fi
SNAP_HASH=$(sha256sum "$SNAPSHOT" | cut -d ' ' -f1)
if [[ -f "$OUT/market-snapshot.sha256" ]]; then
  [[ "$(cat "$OUT/market-snapshot.sha256")" == "$SNAP_HASH" ]] || { echo 'market snapshot changed' >&2; exit 1; }
else
  printf '%s\n' "$SNAP_HASH" > "$OUT/market-snapshot.sha256"
  cp "$SNAPSHOT" "$OUT/market-snapshot.json"
fi
rm -f "$OUT/COMPLETE.json"
printf '%s\n' "$END_DAY" > "$OUT/exclusive-end-day.txt"
: > "$OUT/requested-pages.tsv"
while IFS=$'\t' read -r id created symbol status; do
  start=$((created / 1000 / 86400 * 86400))
  while ((start < END)); do
    end=$((start + 480 * 86400))
    ((end <= END)) || end=$END
    name="market-$id-$start-$end"
    target="$OUT/$name.json"
    printf '%s\t%s\t%s\n' "$id" "$start" "$end" >> "$OUT/requested-pages.tsv"
    if [[ -f "$target" && -f "$target.sha256" ]] && sha256sum -c "$target.sha256" >/dev/null 2>&1 && jq -e '.code == 200 and (.c|type)=="array"' "$target" >/dev/null; then
      start=$end
      continue
    fi
    url="https://mainnet.zklighter.elliot.ai/api/v1/candles?market_id=$id&resolution=1d&start_timestamp=$start&end_timestamp=$end&count_back=500"
    at=$(date -u +%FT%TZ)
    raw="$OUT/attempts/$name-$(date +%s%N)"
    code=$(curl --silent --show-error --connect-timeout 5 --max-time 25 -D "$raw.headers" "$url" -o "$raw.json" -w '%{http_code}') || code=000
    if [[ -f "$raw.json" ]]; then cp "$raw.json" "$target"; fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$at" "$id" "$symbol" "$status" "$code" "$url" >> "$OUT/requests.tsv"
    if [[ "$code" == 200 ]] && jq -e '.code == 200 and (.c|type)=="array"' "$target" >/dev/null 2>&1; then
      sha256sum "$target" > "$target.sha256"
    else
      printf 'failed market %s (%s), HTTP %s\n' "$id" "$symbol" "$code" >&2
    fi
    sleep 1
    start=$end
  done
  printf 'market %s %s complete\n' "$id" "$symbol"
done < <(jq -r '.order_books[]|select(.market_type=="perp")|[.market_id,(.created_at|tonumber),.symbol,.status]|@tsv' "$SNAPSHOT" | sort -n)

while IFS=$'\t' read -r id start end; do
  target="$OUT/market-$id-$start-$end.json"
  sha256sum -c "$target.sha256" >/dev/null
  jq -e '.code==200 and (.c|type)=="array"' "$target" >/dev/null
done < "$OUT/requested-pages.tsv"
cp "${BASH_SOURCE[0]}" "$OUT/downloader.sh"
sha256sum "$OUT/market-snapshot.json" "$OUT/exclusive-end-day.txt" "$OUT/requested-pages.tsv" "$OUT/downloader.sh" > "$OUT/inputs.sha256"
jq -n --arg at "$(date -u +%FT%TZ)" --arg snapshot "$SNAP_HASH" --arg end "$END_DAY" \
  --argjson markets "$(jq '[.order_books[]|select(.market_type=="perp")]|length' "$SNAPSHOT")" \
  --argjson pages "$(wc -l < "$OUT/requested-pages.tsv")" \
  '{schema:"lighter-native-daily-collection-1",completed_at:$at,snapshot_sha256:$snapshot,market_count:$markets,pages:$pages,end_exclusive:$end,notes:["Raw pages can overlap and include zero-volume padding.","Completion verifies requests and hashes, not valid OHLC or historical tradability."]}' > "$OUT/COMPLETE.json"
