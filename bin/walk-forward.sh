#!/usr/bin/env bash
# Expanding walk-forward: retrain at every fold boundary, score only forward.
#
#   bin/walk-forward.sh [start] [end] [folds] [outdir]
#
# WHY THIS EXISTS
#
# A single model has a single `trained_through` date, and the runtime refuses to
# score on or before it — "training contains the answer". That refusal is the
# only thing separating a backtest from a model reciting outcomes it memorised,
# so it is never to be relaxed. But it does mean one model cannot legitimately
# score a multi-year window: point a 2025-cutoff model at 2022 and 150 of 202
# decision dates are correctly refused.
#
# The answer is not a weaker guard, it is more models. Each fold trains on
# everything strictly before its test block and scores only inside it, so every
# date is priced by a model that never saw it. That is what `docs/research/
# backtest.json` describes as "6 expanding walk-forward folds", and what the
# Rust migration dropped: `validate::walk_forward` slices one finished backtest
# into folds and reports each one's metrics, which measures the stability of a
# single fit rather than performing walk-forward at all.
#
# COST
#
# Each fold re-prepares the feature store, which dominates: budget ~30 minutes
# per fold. The training matrix is built once and reused by every fit (~15s).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

START="${1:-2022-09-18}"
END="${2:-2026-07-30}"
FOLDS="${3:-6}"
OUT="${4:-var/research/walk-forward}"

CP=service/target/debug/crypto-portfolio
[ -x "$CP" ] || CP=service/target/release/crypto-portfolio
[ -x "$CP" ] || { echo "walk-forward: build crypto-portfolio first" >&2; exit 2; }
PY=training/.venv/bin/python
[ -x "$PY" ] || { echo "walk-forward: training/.venv missing — see training/README.md" >&2; exit 2; }

MATRIX=data/models/training.jsonl
[ -f "$MATRIX" ] || { echo "walk-forward: $MATRIX missing — run training-matrix first" >&2; exit 2; }

mkdir -p "$OUT"
say() { echo "[$(date -u +%H:%M:%S)] $*"; }

# Fold boundaries. Equal test blocks across the window; the model for each is
# trained on everything before it, so fold 1 has the least history and fold N
# the most. Expanding, not rolling: discarding old data to keep the window a
# fixed width would be a different experiment, and not the one documented.
readarray -t BOUNDS < <(python3 - "$START" "$END" "$FOLDS" <<'PY'
import sys
from datetime import date, timedelta
start, end, folds = date.fromisoformat(sys.argv[1]), date.fromisoformat(sys.argv[2]), int(sys.argv[3])
span = (end - start).days
block = span // folds
for i in range(folds):
    a = start + timedelta(days=i * block)
    b = end if i == folds - 1 else start + timedelta(days=(i + 1) * block - 1)
    # One-day purge: the model may not see the day immediately before its test
    # block either, because that day's label overlaps the first test decision.
    print(f"{(a - timedelta(days=1)).isoformat()} {a.isoformat()} {b.isoformat()}")
PY
)

say "walk-forward $START..$END in $FOLDS expanding folds -> $OUT"
for i in "${!BOUNDS[@]}"; do
  read -r CUTOFF TSTART TEND <<<"${BOUNDS[$i]}"
  n=$((i + 1))
  MODEL="$OUT/model-$n.json"
  CFG="$OUT/config-$n.toml"
  FOLDOUT="$OUT/fold-$n.json"

  say "fold $n/$FOLDS  train<=$CUTOFF  test $TSTART..$TEND"

  "$PY" training/train.py --matrix "$MATRIX" --through "$CUTOFF" --out "$MODEL"

  # A per-fold config, because `backtest` takes its model from the config and
  # the whole point is that each fold uses a different one.
  python3 - "$CFG" "$MODEL" <<'PY'
import re, sys, pathlib
cfg, model = sys.argv[1], sys.argv[2]
text = pathlib.Path("config/default.toml").read_text()
text = re.sub(r'^model_path\s*=.*$', f'model_path = "{model}"', text, flags=re.M)
pathlib.Path(cfg).write_text(text)
PY

  "$CP" backtest --config "$CFG" --data-root data \
    --start "$TSTART" --end "$TEND" --initial-cash 100000 --out "$FOLDOUT"
done

say "stitching"
python3 - "$OUT" "$FOLDS" <<'PY'
import json, pathlib, sys
out, folds = pathlib.Path(sys.argv[1]), int(sys.argv[2])
compounded, rows, refused_total, steps_total = 1.0, [], 0, 0
for n in range(1, folds + 1):
    d = json.loads((out / f"fold-{n}.json").read_text())
    m, steps = d["metrics"], d["steps"]
    refused = sum(1 for x in d.get("disclosures", []) if "gate failed" in str(x))
    r = float(m["total_return"])
    compounded *= (1.0 + r)
    refused_total += refused
    steps_total += len(steps)
    rows.append({"fold": n,
                 "test": [steps[0]["as_of"][:10], steps[-1]["as_of"][:10]] if steps else None,
                 "steps": len(steps), "refused": refused,
                 "total_return": r, "sharpe": float(m.get("sharpe", 0)),
                 "max_drawdown": float(m.get("max_drawdown", 0))})

summary = {"folds": rows,
           "compounded_return": compounded - 1.0,
           "positive_folds": sum(1 for r in rows if r["total_return"] > 0),
           "steps": steps_total, "refused": refused_total}
(out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
print(f"\n{'fold':<5}{'test window':<26}{'steps':>6}{'refused':>9}{'return':>10}{'sharpe':>9}{'maxDD':>9}")
for r in rows:
    w = f"{r['test'][0]}..{r['test'][1]}" if r["test"] else "-"
    print(f"{r['fold']:<5}{w:<26}{r['steps']:>6}{r['refused']:>9}"
          f"{r['total_return']:>9.1%}{r['sharpe']:>9.2f}{r['max_drawdown']:>9.1%}")
print(f"\ncompounded {summary['compounded_return']:.1%} across {folds} folds, "
      f"{summary['positive_folds']}/{folds} positive")
if refused_total:
    print(f"WARNING: {refused_total} decision dates were still refused — a fold is "
          f"scoring dates its own model saw. The numbers above are not clean.")
PY
