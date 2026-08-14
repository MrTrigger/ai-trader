#!/usr/bin/env bash
# Deep historical backfill driver for the Binance-venue gate run (Plan 3c
# Task 5, docs/superpowers/plans/2026-08-13-crypto-scalper-plan-3c-binance-venue.md)
# and the eligibility conditions Amendment 1 sets in docs/scalper-research.md:
# >=18 months of matrix span (§Amendment 1 condition 1) and every mapped,
# 90-day-eligible candidate present (§5.2 / Amendment 1 condition 2).
#
#   bin/scalper-backfill.sh [start] [end]
#
# Default start=2024-08-01 (>18 months before "now" through at least 2026),
# end=tomorrow (Binance's own [start,end) exclusive-window convention, same
# as bin/scalper-pull.sh - using "tomorrow" as the exclusive bound reaches
# through the whole of "today"). Pulls, for every mapped candidate
# (binance_um != null) in data/scalper-universe.json (refreshed first if the
# file is missing):
#
#   1. pull-binance-perp   --data-root data/perp                  (klines - cheap)
#   2. pull-binance-micro  --sources funding,metrics,book          (small)
#   3. pull-binance-micro  --sources flow                          (aggTrades - the bulk)
#      - smallest-cap coins first, BTC/ETH/SOL forced last, so most of the
#        universe has usable microstructure before the three priciest,
#        highest-volume downloads finish.
#      - chunked one calendar month at a time per coin so progress (and log
#        lines) are resumable at symbol-month granularity, per the plan.
#
# RESUMABLE. Safe to kill and re-run at any point:
#   - klines and the small micro sources (funding/metrics/book) are
#     idempotent to re-pull: store::write and write_jsonl always overwrite
#     the whole file for a given day/month from that day's/month's archive,
#     so re-running step 1 or 2 after a partial run just re-downloads a few
#     already-covered days - harmless, only redundant bandwidth (same
#     reasoning as scalper-pull.sh). This script does not try to skip them.
#   - flow (aggTrades) is the expensive one, so this script additionally
#     SKIPS a (coin, month) chunk outright, before touching the network,
#     when every expected day-file already exists under
#     data/binance-micro/flow/{SYMBOL}/ - a local file-existence check, no
#     network call. That's the only resume optimization here; steps 1-2
#     always just re-run.
#
# Logs one timestamped line per unit of work to var/live/scalper-backfill.log
# - the underlying scalper-data commands already print one line per
#   symbol-day or symbol-month (or a 404-skip note), so this script's
#   run_logged() timestamps each of those lines rather than re-deriving
#   units of work itself.
#
# Exits non-zero on any real failure (404s are handled and printed by the
# tools themselves, not errors).
#
# Run detached so it survives the launching session:
#   nohup bin/scalper-backfill.sh >> var/live/scalper-backfill.log 2>&1 &
#   disown
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SCALPER_DATA=./service/target/release/scalper-data
[ -x "$SCALPER_DATA" ] || SCALPER_DATA=./service/target/debug/scalper-data
[ -x "$SCALPER_DATA" ] || {
    echo "scalper-backfill: no scalper-data binary - cargo build -p scalper-data first" >&2
    exit 2
}

command -v jq >/dev/null || { echo "scalper-backfill: jq is required to read the universe file" >&2; exit 2; }

mkdir -p var/live

log() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1"
}

# Run a scalper-data subcommand, timestamping every line it prints. Exits
# with the subcommand's own exit code (not the timestamping pipe's), so
# set -e still catches a real failure.
run_logged() {
    "$@" 2>&1 | while IFS= read -r line; do
        printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$line"
    done
    return "${PIPESTATUS[0]}"
}

START="${1:-2024-08-01}"
END="${2:-$(date -u -d '+1 day' +%Y-%m-%d)}"

log "scalper-backfill: start=$START end=$END (exclusive)"

UNIVERSE=data/scalper-universe.json
if [ ! -f "$UNIVERSE" ]; then
    log "no $UNIVERSE - refreshing via scalper-data universe --top 25"
    run_logged "$SCALPER_DATA" universe --data-root data --top 25
fi
[ -f "$UNIVERSE" ] || { echo "scalper-backfill: universe refresh did not produce $UNIVERSE" >&2; exit 2; }

ASSETS="$(jq -r '.[] | select(.binance_um != null) | .coin' "$UNIVERSE" | paste -sd, -)"
[ -n "$ASSETS" ] || { echo "scalper-backfill: no mapped coins in $UNIVERSE" >&2; exit 2; }
N_COINS="$(echo "$ASSETS" | tr ',' '\n' | wc -l | tr -d ' ')"
log "mapped candidates: $N_COINS ($ASSETS)"

# --- Step 1: klines, full span, all mapped coins in one call - cheap. ---
log "step 1/3: perp klines $START..$END for all mapped coins"
run_logged "$SCALPER_DATA" pull-binance-perp \
    --data-root data/perp --assets "$ASSETS" --start "$START" --end "$END"

# --- Step 2: small micro sources, full span, all mapped coins in one call. ---
log "step 2/3: micro (funding,metrics,book) $START..$END for all mapped coins"
run_logged "$SCALPER_DATA" pull-binance-micro \
    --data-root data --assets "$ASSETS" --sources funding,metrics,book \
    --start "$START" --end "$END"

# --- Step 3: flow (aggTrades), smallest-cap first, majors last, chunked by
#     calendar month per coin so a resumed run can skip finished months. ---
MINORS="$(jq -r '
  [.[] | select(.binance_um != null) | select(.coin != "BTC" and .coin != "ETH" and .coin != "SOL")]
  | sort_by(.day_volume_usd) | .[].coin
' "$UNIVERSE")"
MAJORS=""
for m in BTC ETH SOL; do
    if jq -e --arg c "$m" '.[] | select(.coin == $c and .binance_um != null)' "$UNIVERSE" >/dev/null; then
        MAJORS="$MAJORS
$m"
    fi
done
FLOW_COINS="$(printf '%s%s\n' "$MINORS" "$MAJORS" | sed '/^$/d')"

log "step 3/3: flow (aggTrades), smallest-cap first, majors last: $(echo "$FLOW_COINS" | paste -sd, -)"

while IFS= read -r coin; do
    [ -n "$coin" ] || continue
    symbol="$(jq -r --arg c "$coin" '.[] | select(.coin == $c) | .binance_um' "$UNIVERSE")"
    [ -n "$symbol" ] && [ "$symbol" != "null" ] || continue

    cur="$(date -u -d "$START" +%Y-%m-01)"
    while [ "$(date -u -d "$cur" +%s)" -lt "$(date -u -d "$END" +%s)" ]; do
        next="$(date -u -d "$cur +1 month" +%Y-%m-01)"
        chunk_start="$cur"
        [ "$(date -u -d "$chunk_start" +%s)" -lt "$(date -u -d "$START" +%s)" ] && chunk_start="$START"
        chunk_end="$next"
        [ "$(date -u -d "$chunk_end" +%s)" -gt "$(date -u -d "$END" +%s)" ] && chunk_end="$END"

        # Skip rule: if every day-file this chunk would produce already
        # exists, don't re-download the month - a local existence check,
        # no network call.
        have_all=1
        d="$chunk_start"
        while [ "$(date -u -d "$d" +%s)" -lt "$(date -u -d "$chunk_end" +%s)" ]; do
            day_str="$(date -u -d "$d" +%Y-%m-%d)"
            if [ ! -f "data/binance-micro/flow/$symbol/$day_str.jsonl" ]; then
                have_all=0
                break
            fi
            d="$(date -u -d "$d +1 day" +%Y-%m-%d)"
        done

        if [ "$have_all" = 1 ]; then
            log "flow $symbol $chunk_start..$chunk_end: skipped (all day-files already present)"
        else
            run_logged "$SCALPER_DATA" pull-binance-micro \
                --data-root data --assets "$coin" --sources flow \
                --start "$chunk_start" --end "$chunk_end"
        fi

        cur="$next"
    done
done <<< "$FLOW_COINS"

log "scalper-backfill: done ($START..$END, $N_COINS mapped coins)"
