#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 ]]; then
  echo 'usage: run.sh FOLD_JSON NEW_AUDIT_JSON' >&2
  exit 2
fi
cargo build --release --manifest-path var/research/lighter-window-audit-r10/Cargo.toml
var/research/lighter-window-audit-r10/target/release/lighter-window-audit-r10 "$PWD" "$1" "$2"
