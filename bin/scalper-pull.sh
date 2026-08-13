#!/usr/bin/env bash
# Nightly Binance perp bar pull for the scalper research pipeline (Plan 3,
# docs/scalper-research.md §1).
#
#   bin/scalper-pull.sh [days]
#
# Default days=3. Re-pulls the last `days` days through now for every
# mapped candidate in data/scalper-universe.json (every coin whose
# binance_um field is non-null - unmapped coins can never contribute a
# matrix row). pull-binance-perp has no cache-skip, so re-pulling a few
# already-covered days nightly is simple and harmless, just redundant
# bandwidth - no incremental logic to get wrong.
#
# End is exclusive on bar timestamps; "tomorrow" just means "through now".
#
# Cron it:
#   20 0 * * *  cd /path/to/ai-trader && bin/scalper-pull.sh >> var/live/scalper-pull.log 2>&1
set -euo pipefail

DAYS="${1:-3}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SCALPER_DATA=./service/target/release/scalper-data
[ -x "$SCALPER_DATA" ] || SCALPER_DATA=./service/target/debug/scalper-data
[ -x "$SCALPER_DATA" ] || {
    echo "scalper-pull: no scalper-data binary - cargo build -p scalper-data first" >&2
    exit 2
}

command -v jq >/dev/null || { echo "scalper-pull: jq is required to read the universe file" >&2; exit 2; }

UNIVERSE=data/scalper-universe.json
[ -f "$UNIVERSE" ] || {
    echo "scalper-pull: no $UNIVERSE - run scalper-data universe --data-root data --top 25 first" >&2
    exit 2
}

ASSETS="$(jq -r '.[] | select(.binance_um != null) | .coin' "$UNIVERSE" | paste -sd, -)"
[ -n "$ASSETS" ] || { echo "scalper-pull: no mapped coins in $UNIVERSE" >&2; exit 2; }

START="$(date -u -d "-${DAYS} days" +%Y-%m-%d)"
END="$(date -u -d "+1 day" +%Y-%m-%d)"

"$SCALPER_DATA" pull-binance-perp \
    --data-root data/perp \
    --assets "$ASSETS" \
    --start "$START" --end "$END"
