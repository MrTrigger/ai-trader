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
#   2. pull-binance-micro  --sources book, then metrics             (small, skip-resumable)
#      then --sources funding                                       (small, always re-pulled)
#   3. pull-binance-micro  --sources flow                          (aggTrades - the bulk, skip-resumable)
#      - smallest-cap coins first, BTC/ETH/SOL forced last, so most of the
#        universe has usable microstructure before the three priciest,
#        highest-volume downloads finish.
#      - chunked one calendar month at a time per coin so progress (and log
#        lines) are resumable at symbol-month granularity, per the plan.
#
# RESUMABLE. Safe to kill (or lose to an OOM/WSL event) and re-run at any
# point:
#   - klines and funding are idempotent to re-pull: store::write and
#     write_jsonl always overwrite the whole file for a given day/month from
#     that day's/month's archive, so re-running them after a partial run
#     just re-downloads a few already-covered units - harmless, only
#     redundant bandwidth (same reasoning as scalper-pull.sh). funding is
#     one small merged-by-ts file per symbol and cheap regardless, so it is
#     always fully re-pulled - no skip rule for it.
#   - book, metrics, and flow (aggTrades) are the sources worth skipping:
#     this script SKIPS a (source, coin, month) chunk outright, before
#     touching the network, when every expected day-file already exists
#     under data/binance-micro/{source}/{SYMBOL}/ - a local file-existence
#     check, no network call. Day files are written atomically (tmp file +
#     rename, commit 2912ce0), so a day-file existing on disk means that
#     day's pull fully completed, never a partial write half-visible mid-
#     download - "exists" really does mean "done", making the skip test
#     safe. Partial months (any missing day) are re-pulled in full, which is
#     fine and idempotent.
#
# Logs one timestamped line per unit of work to var/live/scalper-backfill.log
# - the underlying scalper-data commands already print one line per
#   symbol-day or symbol-month (or a 404-skip note), so this script's
#   run_logged() timestamps each of those lines rather than re-deriving
#   units of work itself. An EXIT trap additionally logs the script's own
#   exit status on every exit (normal or error) so a future death leaves a
#   trace in the log even if it's the last line written - though it cannot
#   catch a SIGKILL (e.g. a hard OOM kill), since no shell trap can.
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

# Leaves a trace on every exit - normal completion, a `set -e` abort, or any
# trappable signal - so a future silent death still has a last log line to
# diagnose from. Single EXIT trap (bash runs it for error exits too, since
# set -e turns an error into a plain exit) keeps this to one line per run.
# Cannot catch SIGKILL (a hard OOM kill) - no shell trap can - but covers
# everything else, including the more common soft-OOM SIGTERM.
on_exit() {
    local status=$?
    log "exit status=$status at $(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
trap on_exit EXIT

START="${1:-2024-08-01}"
END="${2:-$(date -u -d '+1 day' +%Y-%m-%d)}"

log "scalper-backfill: start=$START end=$END (exclusive)"
log "ulimits (first 3): $(ulimit -a | head -3 | paste -sd '; ' -)"

UNIVERSE=data/scalper-universe.json
if [ ! -f "$UNIVERSE" ]; then
    log "no $UNIVERSE - refreshing via scalper-data universe --top 25"
    run_logged "$SCALPER_DATA" universe --data-root data --top 25
fi
[ -f "$UNIVERSE" ] || { echo "scalper-backfill: universe refresh did not produce $UNIVERSE" >&2; exit 2; }

ASSET_COINS="$(jq -r '.[] | select(.binance_um != null) | .coin' "$UNIVERSE")"
ASSETS="$(echo "$ASSET_COINS" | paste -sd, -)"
[ -n "$ASSETS" ] || { echo "scalper-backfill: no mapped coins in $UNIVERSE" >&2; exit 2; }
N_COINS="$(echo "$ASSETS" | tr ',' '\n' | wc -l | tr -d ' ')"
log "mapped candidates: $N_COINS ($ASSETS)"

# Pull one micro `source` (book|metrics|flow - anything that lands under
# data/binance-micro/{source}/{SYMBOL}/{day}.jsonl) for every coin in
# `coin_list` (newline-separated, caller's chosen order), chunked one
# calendar month at a time per coin. Skips a chunk outright, before any
# network call, when every day-file it would produce already exists (see
# the header comment on why "exists" safely means "done").
pull_chunked() {
    local source="$1" coin_list="$2" coin symbol
    while IFS= read -r coin; do
        [ -n "$coin" ] || continue
        symbol="$(jq -r --arg c "$coin" '.[] | select(.coin == $c) | .binance_um' "$UNIVERSE")"
        [ -n "$symbol" ] && [ "$symbol" != "null" ] || continue

        local cur next chunk_start chunk_end have_all d day_str
        cur="$(date -u -d "$START" +%Y-%m-01)"
        while [ "$(date -u -d "$cur" +%s)" -lt "$(date -u -d "$END" +%s)" ]; do
            next="$(date -u -d "$cur +1 month" +%Y-%m-01)"
            chunk_start="$cur"
            [ "$(date -u -d "$chunk_start" +%s)" -lt "$(date -u -d "$START" +%s)" ] && chunk_start="$START"
            chunk_end="$next"
            [ "$(date -u -d "$chunk_end" +%s)" -gt "$(date -u -d "$END" +%s)" ] && chunk_end="$END"

            have_all=1
            d="$chunk_start"
            while [ "$(date -u -d "$d" +%s)" -lt "$(date -u -d "$chunk_end" +%s)" ]; do
                day_str="$(date -u -d "$d" +%Y-%m-%d)"
                if [ ! -f "data/binance-micro/$source/$symbol/$day_str.jsonl" ]; then
                    have_all=0
                    break
                fi
                d="$(date -u -d "$d +1 day" +%Y-%m-%d)"
            done

            if [ "$have_all" = 1 ]; then
                log "$source $symbol $chunk_start..$chunk_end: skipped (all day-files already present)"
            else
                run_logged "$SCALPER_DATA" pull-binance-micro \
                    --data-root data --assets "$coin" --sources "$source" \
                    --start "$chunk_start" --end "$chunk_end"
            fi

            cur="$next"
        done
    done <<< "$coin_list"
}

# --- Step 1: klines, full span, all mapped coins in one call - cheap. ---
log "step 1/3: perp klines $START..$END for all mapped coins"
run_logged "$SCALPER_DATA" pull-binance-perp \
    --data-root data/perp --assets "$ASSETS" --start "$START" --end "$END"

# --- Step 2: small micro sources. book and metrics are skip-resumable
#     (per-coin, per-month, via pull_chunked); funding is always re-pulled
#     in one bulk call - it's cheap and merged-by-ts, not worth chunking. ---
log "step 2/3: micro book $START..$END, skip-resumable per symbol-month"
pull_chunked book "$ASSET_COINS"

log "step 2/3: micro metrics $START..$END, skip-resumable per symbol-month"
pull_chunked metrics "$ASSET_COINS"

log "step 2/3: micro funding $START..$END for all mapped coins (no skip rule)"
run_logged "$SCALPER_DATA" pull-binance-micro \
    --data-root data --assets "$ASSETS" --sources funding \
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
pull_chunked flow "$FLOW_COINS"

log "scalper-backfill: done ($START..$END, $N_COINS mapped coins)"
