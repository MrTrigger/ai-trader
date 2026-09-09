#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT=${1:?usage: train-lighter-market-context.sh NEW_OUTPUT_DIR}
[[ ! -e "$OUT" ]] || { echo "output already exists: $OUT" >&2; exit 1; }
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio --example lighter-training-matrix
mkdir -p "$OUT/snapshot"
cp service/target/release/examples/lighter-training-matrix "$OUT/snapshot/"
cp training/train.py "$OUT/snapshot/"
cp "${BASH_SOURCE[0]}" "$OUT/snapshot/"
cp docs/research/lighter-optimization-r24-context.json "$OUT/declaration.json"
cp var/research/lighter-native-baseline-configs/config-1.toml "$OUT/snapshot/"
sha256sum service/crates/crypto-portfolio/src/{lib,model,training,market_context}.rs \
  service/crates/crypto-portfolio/examples/lighter-training-matrix.rs \
  service/Cargo.lock "$OUT"/snapshot/* > "$OUT/source.sha256"
rg --files --hidden --no-ignore -0 data/bars data/funding data/perp \
  | sort -z | xargs -0 sha256sum > "$OUT/signal-inputs.sha256"
"$OUT/snapshot/lighter-training-matrix" external \
  var/research/lighter-history-audit/daily-2026-09-08 data \
  "$OUT/snapshot/config-1.toml" 2025-01-18 2025-11-24 "$OUT/matrix" 24 --market-context
python3 - "$OUT" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
def read(path):
    with path.open() as f:
        manifest = json.loads(next(f))
        return manifest, [json.loads(line) for line in f]
old_manifest, old = read(pathlib.Path('var/research/lighter-ranker-r1-2026-09-08/external/matrix.jsonl'))
manifest, new = read(root / 'matrix/matrix.jsonl')
context = ['mkt_btc_ret_7d', 'mkt_btc_ret_30d', 'mkt_btc_ret_90d', 'mkt_btc_vol_30d']
assert manifest['features'] == old_manifest['features'] + context
assert manifest['market_context'] == 'btc-daily-raw-v1'
assert len(old) == len(new)
for previous, current in zip(old, new):
    stripped = dict(current, features={k: v for k, v in current['features'].items() if k not in context})
    assert previous == stripped, (previous['date'], previous['asset'])
(root / 'matrix-parity.json').write_text(json.dumps({
    'original_rows': len(old), 'all_original_features_and_targets_exact': True,
    'appended_features': context,
}, indent=2) + '\n')
PY
env -u TRAIN_DROP TRAIN_OBJECTIVE=regression TRAIN_SEEDS=1 TRAIN_RANK=0 \
  training/.venv/bin/python "$OUT/snapshot/train.py" \
  --matrix "$OUT/matrix/matrix.jsonl" --through 2025-11-24 \
  --reward per_risk --out "$OUT/model.json"
sha256sum "$OUT/model.json" "$OUT/matrix/matrix.jsonl" "$OUT/declaration.json" \
  "$OUT/matrix-parity.json" > "$OUT/artifacts.sha256"
