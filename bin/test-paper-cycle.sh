#!/usr/bin/env bash
# Exercise cycle orchestration without network access, credentials, or orders.
set -euo pipefail
REPO=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/paper-cycle-test.XXXXXX")
trap 'rm -rf "$TEST_ROOT"' EXIT
mkdir -p "$TEST_ROOT/bin"

cat > "$TEST_ROOT/bin/stub" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
role=${0##*/}
printf '%s' "$role" >> "$CALLS"
printf '\t%s' "$@" >> "$CALLS"
printf '\n' >> "$CALLS"
if [[ "$role" == bot ]]; then
  [[ "$1" == --config && "$2" == "$EXPECTED_BOT_CONFIG" ]]
  shift 2
fi
stage=$1
shift
[[ "$stage" != "${FAIL_STAGE:-}" ]] || exit 42
arg() {
  local wanted=$1
  shift
  while (($#)); do
    if [[ "$1" == "$wanted" ]]; then printf '%s' "$2"; return; fi
    shift
  done
  return 1
}
case "$role:$stage" in
  bot:settle|bot:book-capture) ;;
  bot:markets) printf 'venue-markets-fixture\n' ;;
  bot:book) printf 'live-book-fixture\n' ;;
  bot:run)
    [[ "$(cat "$(arg --plan "$@")")" == plan-fixture ]]
    ;;
  planner:data-pull)
    [[ "$(arg --config "$@")" == "$EXPECTED_CORE_CONFIG" ]]
    [[ "$(arg --data-root "$@")" == "$EXPECTED_SHARED" ]]
    ;;
  planner:universe-rank)
    [[ "$(arg --data-root "$@")" == "$EXPECTED_DATA" ]]
    [[ "$(cat "$(arg --tradeable "$@")")" == venue-markets-fixture ]]
    mkdir -p "$EXPECTED_DATA/universe"
    printf 'venue-universe\n' > "$EXPECTED_DATA/universe/today.json"
    ;;
  planner:plan)
    [[ "$(arg --data-root "$@")" == "$EXPECTED_DATA" ]]
    [[ "$(arg --config "$@")" == "$EXPECTED_CORE_CONFIG" ]]
    [[ "$(cat "$(arg --book "$@")")" == live-book-fixture ]]
    [[ "$(arg --out "$@")" == "$EXPECTED_DATA/state/plan.json" ]]
    printf 'plan-fixture\n' > "$(arg --out "$@")"
    ;;
  *) printf 'unexpected invocation: %s %s\n' "$role" "$stage" >&2; exit 99 ;;
esac
STUB
chmod +x "$TEST_ROOT/bin/stub"
ln -s stub "$TEST_ROOT/bin/bot"
ln -s stub "$TEST_ROOT/bin/planner"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
contains() { rg -q -- "$2" "$1" || fail "missing $2 in $1"; }
absent() { if rg -q -- "$2" "$1"; then fail "unexpected $2 in $1"; fi; }

run_case() {
  local name=$1 separate=$2 fail_stage=$3 bootstrap=$4
  local case_root="$TEST_ROOT/$name"
  local shared="$case_root/data"
  local data="$shared"
  [[ "$separate" == 0 ]] || data="$shared/lighter"
  mkdir -p "$shared/bars" "$shared/funding" "$data" "$case_root/config"
  local calls="$case_root/calls.tsv"
  : > "$calls"
  local status=0
  env -u BOOTSTRAP \
    BOT_BINARY="$TEST_ROOT/bin/bot" PLANNER_BINARY="$TEST_ROOT/bin/planner" \
    BOT_CONFIG="$case_root/config/bot.json" PORTFOLIO_CONFIG="$case_root/config/core.toml" \
    DATA_ROOT="$data" MARKET_DATA_ROOT="$shared" \
    CALLS="$calls" FAIL_STAGE="$fail_stage" \
    EXPECTED_BOT_CONFIG="$case_root/config/bot.json" EXPECTED_CORE_CONFIG="$case_root/config/core.toml" \
    EXPECTED_DATA="$data" EXPECTED_SHARED="$shared" \
    ${bootstrap:+BOOTSTRAP=$bootstrap} \
    bash "$REPO/deploy/cycle.sh" > "$case_root/output.log" 2>&1 || status=$?

  contains "$calls" $'^planner\tdata-pull\t'
  if [[ "$separate" == 1 ]]; then
    [[ -L "$data/bars" && "$(readlink "$data/bars")" == "$shared/bars" ]] || fail "$name bars link"
    [[ -L "$data/funding" && "$(readlink "$data/funding")" == "$shared/funding" ]] || fail "$name funding link"
    [[ ! -e "$shared/state" && ! -e "$shared/universe" ]] || fail "$name leaked venue state/universe into shared root"
  fi
  if [[ -n "$fail_stage" ]]; then
    [[ "$status" == 42 ]] || { cat "$case_root/output.log" >&2; fail "$name exit $status, expected 42"; }
    absent "$calls" $'^planner\tplan\t'
    absent "$calls" $'\trun\t'
    absent "$calls" $'\tbook-capture\t'
    [[ ! -e "$data/state/plan.json" ]] || fail "$name generated a plan after failure"
    if [[ "$fail_stage" == markets ]]; then absent "$calls" $'^planner\tuniverse-rank\t'; fi
  else
    [[ "$status" == 0 ]] || { cat "$case_root/output.log" >&2; fail "$name exit $status"; }
    [[ -f "$data/state/plan.json" && -f "$data/universe/today.json" ]] || fail "$name missing output"
    contains "$calls" $'^planner\tplan\t'
    contains "$calls" $'\trun\t--plan\t'
    [[ "$(rg -c $'\tsettle$' "$calls")" == 2 ]] || fail "$name needs pre/post settlement"
    if [[ "$bootstrap" == 1 ]]; then
      contains "$calls" $'\trun\t.*\t--accept-decision-lag$'
    else
      absent "$calls" --accept-decision-lag
    fi
  fi
  [[ -z "$(find "$data/state" -maxdepth 1 -type f \( -name 'markets.*' -o -name 'book.*' \) -print -quit)" ]] || fail "$name left scratch snapshots"
  printf 'PASS: %s\n' "$name"
}

run_case hyperliquid-normal 0 '' ''
run_case lighter-normal 1 '' ''
run_case lighter-markets-failure 1 markets ''
run_case lighter-book-failure 1 book ''
run_case lighter-bootstrap 1 '' 1
run_case lighter-bootstrap-disabled 1 '' 0
run_case lighter-bootstrap-other-value 1 '' true
