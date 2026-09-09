#!/usr/bin/env bash
set -euo pipefail
cd /home/magnus/dev/magnus/ai-trader
OUT=${1:?usage: train-lighter-market-context.sh NEW_OUTPUT_DIR}
[[ ! -e "$OUT" ]] || { echo "output already exists: $OUT" >&2; exit 1; }
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio --example lighter-training-matrix
mkdir -p "$OUT/snapshot"
cp service/target/release/examples/lighter-training-matrix "$OUT/snapshot/"
cp training/train.py "$OUT/snapshot/"
cp "${BASH_SOURCE[0]}" "$OUT/snapshot/"
cp docs/research/lighter-optimization-r31-native-labels.json "$OUT/declaration.json"
cp var/research/lighter-native-baseline-configs/config-1.toml "$OUT/snapshot/"
sha256sum service/crates/crypto-portfolio/src/{lib,model,training,market_context,lighter_hourly}.rs \
  service/crates/crypto-portfolio/examples/lighter-training-matrix.rs \
  service/Cargo.lock "$OUT"/snapshot/* > "$OUT/source.sha256"
rg --files --hidden --no-ignore -0 data/bars data/funding data/perp \
  | sort -z | xargs -0 sha256sum > "$OUT/signal-inputs.sha256"
"$OUT/snapshot/lighter-training-matrix" native-hourly \
  var/research/lighter-history-audit/daily-2026-09-08 data \
  "$OUT/snapshot/config-1.toml" 2025-01-18 2025-11-24 "$OUT/matrix" 24 --market-context --hourly-root var/research/lighter-training-hourly/hourly
python3 - "$OUT" <<'PY'
import hashlib, json, pathlib, sys
root = pathlib.Path(sys.argv[1])
def read(path):
    with path.open() as f:
        manifest = json.loads(next(f))
        return manifest, [json.loads(line) for line in f]
reference = pathlib.Path('var/research/lighter-ranker-r24-context/matrix/matrix.jsonl')
reference_sha256 = '7908f995a691b199cf50ccd958871b88672b0917a7ffda5f97e171212073a1c0'
assert hashlib.sha256(reference.read_bytes()).hexdigest() == reference_sha256
old_manifest, old = read(reference)
manifest, new = read(root / 'matrix/matrix.jsonl')
context = ['mkt_btc_ret_7d', 'mkt_btc_ret_30d', 'mkt_btc_ret_90d', 'mkt_btc_vol_30d']
assert manifest['features'] == old_manifest['features']
assert manifest['label_prices'] == 'native-hourly'
assert manifest['label_horizon_hours'] == 24 and manifest['label_lag_hours'] == 1
assert manifest['market_context'] == 'btc-daily-raw-v1'
assert len(old) == len(new) == 6486
assert len({r['date'] for r in new}) == 241
assert len({r['asset'] for r in new}) == 45
assert any(a['raw_ret'] != b['raw_ret'] for a,b in zip(old,new))
for previous, current in zip(old, new):
    keys = ['date', 'slot', 'asset', 'vol', 'features']
    assert {k: previous[k] for k in keys} == {k: current[k] for k in keys}, (previous['date'], previous['asset'])
(root / 'matrix-parity.json').write_text(json.dumps({
    'reference_sha256': reference_sha256, 'original_rows': len(old), 'all_original_features_vol_and_identity_exact': True,
    'feature_count': len(manifest['features']),
    'dates': len({r['date'] for r in new}), 'assets': len({r['asset'] for r in new}),
    'changed_raw_return_labels': sum(a['raw_ret'] != b['raw_ret'] for a,b in zip(old,new)),
}, indent=2) + '\n')
PY
env -u TRAIN_DROP TRAIN_OBJECTIVE=regression TRAIN_SEEDS=1 TRAIN_RANK=0 \
  training/.venv/bin/python "$OUT/snapshot/train.py" \
  --matrix "$OUT/matrix/matrix.jsonl" --through 2025-11-24 \
  --reward per_risk_abs --out "$OUT/model.json"
sha256sum "$OUT/model.json" "$OUT/matrix/matrix.jsonl" "$OUT/declaration.json" \
  "$OUT/matrix-parity.json" "$OUT/matrix/native-hourly-inputs.json" > "$OUT/artifacts.sha256"
