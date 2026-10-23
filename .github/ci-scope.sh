#!/usr/bin/env bash
# Repository boundaries for routine pull requests; unknown inputs require all checks.
set -euo pipefail
app=false
full=false
rust=false
training=false
all_checks() { app=true; full=true; rust=true; training=true; }
diff_file=$(mktemp)
trap 'rm -f "$diff_file"' EXIT
changed=()
event=${1:-}
action=${2:-}
base=${3:-}
head=${4:-HEAD}
draft=${5:-true}
if [[ "$event" != pull_request ]]; then
  all_checks
elif ! comparison=$(git merge-base "$base" "$head") || ! git diff --no-renames --name-only -z "$comparison" "$head" > "$diff_file"; then
  all_checks
else
  mapfile -d '' -t changed < "$diff_file"
  if [[ ${#changed[@]} -eq 0 ]]; then all_checks; fi
  for path in "${changed[@]}"; do
    if [[ "$path" != */* && "$path" == *.md ]]; then continue; fi
    case "$path" in
      */package.json|*/package-lock.json|*/bun.lock|*/bun.lockb|*/Cargo.toml|*/Cargo.lock|*/pyproject.toml|*/requirements*|*/uv.lock|*/poetry.lock|*/tsconfig*.json|*/vite.config.*|*/vitest.config.*|*/west.yml) all_checks ;;
      */README.md|*/CHANGELOG.md|LICENSE|docs/*.md) ;;
      training/*) app=true; training=true ;;
      *) all_checks ;;
    esac
  done
  if [[ "$app" == true && ( "$action" == ready_for_review || ( "$action" == opened && "$draft" == false ) ) ]]; then all_checks; fi
fi
printf 'app=%s\n' "$app"
printf 'full=%s\n' "$full"
printf 'rust=%s\n' "$rust"
printf 'training=%s\n' "$training"
