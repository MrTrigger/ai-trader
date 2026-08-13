#!/usr/bin/env bash
# Shadow forward logging (Task 16): after each Stockholm close, build the
# day's live-contract feature rows, score them with the frozen lean model and
# the Task 14/15 overlay constructor, and append one JSON line to an
# append-only log that is never tuned against.
#
#   bin/stockholm-shadow.sh
#
# This is the only future evidence source for the Stockholm strategy that is
# not already exposed to whoever tunes it: everything else in var/stockholm
# is either training data or a backtest, both of which the strategy has been
# (or could be) shaped against. The shadow log alone accrues genuinely
# untouched forward observations, one Stockholm session at a time.
#
# Cron it, after the 17:30 Stockholm close (the global-risk feature window's
# own cutoff) with margin for the day's data collection to land:
#   0 18 * * 1-5  TZ=Europe/Stockholm cd /path/to/ai-trader && bin/stockholm-shadow.sh >> var/stockholm/shadow-log/logs/cron.log 2>&1
# or a systemd timer with OnCalendar=Mon..Fri 18:00 Europe/Stockholm firing a
# oneshot service that runs this script. Nothing here installs either --
# that is a separate, explicit operational decision.
#
# WHY IT WILL ONLY PRODUCE ROWS THAT ARE PLUMBING, FOR NOW
#
# Every frozen fold model was trained under a feature-set version older than
# this binary's current one (fs-rust-stockholm-1 versus the current
# fs-rust-stockholm-N -- see features_stockholm::BASELINE_FEATURE_SET_VERSION).
# `shadow-score` enforces the identical consistency gate `backtest` does: a
# model and the matrix it scores must declare the same feature-set version,
# feature order, and survivorship status, or the run is refused. A matrix
# freshly built by this script's own `training-matrix` step below therefore
# gets correctly REFUSED against every frozen model that exists today -- that
# is the gate working as designed, not a bug in this script. Real forward
# evidence starts accruing only once Phase 1 rebuilds the matrices and
# retrains under the current feature-set version. Until then this script is a
# deterministic plumbing check: it will run to the refusal and exit non-zero,
# which is the correct, loud outcome (see docs/stockholm-portfolio-status.md,
# "2026-08-13 Task 16: shadow forward logging"). It does NOT weaken the gate
# to force a row out.
#
# Exit codes are the point, same as bin/cycle.sh: anything non-zero and cron
# mails a human, rather than the job pressing on with a stale or inconsistent
# score.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Release preferred, debug as fallback, same as bin/cycle.sh -- neither is
# built here. A stale release binary from before your last change is
# preferred silently over a freshly built debug one (bitten by this once
# while testing this script: rebuild release after touching this crate).
CLI=./service/target/release/stockholm-portfolio
[ -x "$CLI" ] || CLI=./service/target/debug/stockholm-portfolio
[ -x "$CLI" ] || { echo "stockholm-shadow: no stockholm-portfolio binary — cargo build first" >&2; exit 2; }

# Root of the shadow log and its working files. The eventual home is the main
# checkout's var/stockholm/shadow-log; a worktree used for development points
# this elsewhere so it never writes into the main checkout's evidence trail.
SHADOW_LOG_ROOT="${SHADOW_LOG_ROOT:-var/stockholm/shadow-log}"

# The frozen lean fold this job scores with: the latest fold
# (baseline-membership-prenorm-v2's model-4.json), read-only. The log records
# which artifact scored each row (model_id, feature_set_version), so a future
# reader never has to trust this default out of band.
SHADOW_MODEL="${SHADOW_MODEL:-var/stockholm/baseline-membership-prenorm-v2/model-4.json}"

# The collected raw dataset `training-matrix` builds the day's feature rows
# from. This script does not collect fresh data itself -- it assumes a
# separate, already-scheduled collection step keeps this fresh (Phase 1).
SHADOW_DATA_ROOT="${SHADOW_DATA_ROOT:-var/stockholm/research-public}"
SHADOW_BENCHMARK="${SHADOW_BENCHMARK:-var/stockholm/research-public/benchmarks/OMXSGI.json}"
SHADOW_SKV_LISTING_HISTORY="${SHADOW_SKV_LISTING_HISTORY:-var/stockholm/authority-data/skatteverket-equity-history/latest-listing-history.json}"

# Overlay defaults per Task 14/15: index core plus self-funding overlay, the
# configuration the frozen lean model is meant to run under in Phase 2. Any of
# these can be overridden by exporting the same-named env var before running.
SHADOW_ALLOCATION_MODE="${SHADOW_ALLOCATION_MODE:-overlay}"

STOCKHOLM_DAY="$(TZ=Europe/Stockholm date +%Y-%m-%d)"
LOG_DIR="$SHADOW_LOG_ROOT/logs"
mkdir -p "$SHADOW_LOG_ROOT" "$LOG_DIR"
STDERR_LOG="$LOG_DIR/$STOCKHOLM_DAY.log"

say() { echo "[$(date -u +%H:%M:%S)] $*"; }

# Progress (say, and each CLI call's own stdout summary line) goes to this
# process's normal stdout, exactly as bin/cycle.sh does -- the caller (cron,
# a systemd unit, an interactive shell) decides where that goes. Only stderr
# is additionally captured here, to a dated file per session, so a failure
# overnight leaves a paper trail even if the caller's own stdout capture is
# rotated away before anyone looks.
{
    say "stockholm-shadow $STOCKHOLM_DAY  model=$SHADOW_MODEL  allocation-mode=$SHADOW_ALLOCATION_MODE"

    [ -f "$SHADOW_MODEL" ] || { echo "stockholm-shadow: model $SHADOW_MODEL not found" >&2; exit 2; }
    [ -d "$SHADOW_DATA_ROOT" ] || {
        echo "stockholm-shadow: data root $SHADOW_DATA_ROOT not found -- Phase 1's collection has not populated it here" >&2
        exit 2
    }

    # 1. Build/refresh today's live-contract feature rows. A narrow window is
    #    enough: `training-matrix` computes every rolling feature from each
    #    instrument's complete history regardless of --start, and only the
    #    matrix's own most recent date is ever scored below. The window is
    #    rolling (overwritten each run), not accumulated, so this step never
    #    grows unbounded disk use the way the append-only log deliberately
    #    does.
    say "matrix"
    MATRIX="$SHADOW_LOG_ROOT/matrix-latest.jsonl"
    MATRIX_ARGS=(
        training-matrix
        --data-root "$SHADOW_DATA_ROOT"
        --start "$(date -u -d "$STOCKHOLM_DAY -14 days" +%Y-%m-%d)"
        --end "$STOCKHOLM_DAY"
        --out "$MATRIX"
        --feature-set baseline
    )
    # `set -e` note: this must be an `if`, not `test && append` -- a bare
    # `[ -f ... ] &&` whose test is false would exit the whole script here.
    if [ -f "$SHADOW_SKV_LISTING_HISTORY" ]; then
        MATRIX_ARGS+=(--skv-listing-history "$SHADOW_SKV_LISTING_HISTORY")
    fi
    "$CLI" "${MATRIX_ARGS[@]}"

    # 2. Score the matrix's most recent date and append to the append-only
    #    log. No orders, no state mutation: `shadow-score` reads the matrix
    #    and the model, proposes an allocation from flat, and either appends
    #    one line or refuses -- it never touches any other file.
    say "score"
    "$CLI" shadow-score \
        --matrix "$MATRIX" \
        --model "$SHADOW_MODEL" \
        --benchmark "$SHADOW_BENCHMARK" \
        --allocation-mode "$SHADOW_ALLOCATION_MODE" \
        --out "$SHADOW_LOG_ROOT/scores.jsonl"

    say "done"
} 2>> "$STDERR_LOG"
