#!/usr/bin/env bash
set -euo pipefail
cd /home/magnus/dev/magnus/ai-trader
OUT=var/research/lighter-ranker-r35-slow-features
[[ ! -e "$OUT" ]]
mkdir -p "$OUT/snapshot"
cp training/train.py "$OUT/snapshot/"
cp "${BASH_SOURCE[0]}" "$OUT/snapshot/"
cp docs/research/lighter-optimization-r35-slow-features.json "$OUT/declaration.json"
MATRIX=var/research/lighter-ranker-r31-native-hourly/matrix/matrix.jsonl
sha256sum "$OUT"/snapshot/* "$OUT/declaration.json" "$MATRIX" > "$OUT/source.sha256"
DROP=$(python3 - "$MATRIX" "$OUT/declaration.json" <<'PY'
import json,sys
with open(sys.argv[1]) as f: features=json.loads(next(f))['features']
keep=json.load(open(sys.argv[2]))['training']['features']
assert set(keep).issubset(features) and len(keep)==8
print(','.join(f for f in features if f not in keep))
PY
)
env TRAIN_DROP="$DROP" TRAIN_OBJECTIVE=regression TRAIN_SEEDS=5 TRAIN_RANK=0 training/.venv/bin/python "$OUT/snapshot/train.py" --matrix "$MATRIX" --through 2025-11-24 --reward per_risk_abs --out "$OUT/model.json"
python3 - "$OUT" <<'PY'
import json,sys,pathlib
root=pathlib.Path(sys.argv[1]);m=json.loads((root/'model.json').read_text());d=json.loads((root/'declaration.json').read_text())
assert m['features']==d['training']['features']
assert m['n_rows']==6486 and m['n_dates']==241 and len(m['tree_info'])==1500
PY
sha256sum "$OUT/model.json" > "$OUT/model.sha256"
