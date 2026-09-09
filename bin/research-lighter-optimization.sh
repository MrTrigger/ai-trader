#!/usr/bin/env bash
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ ${LIGHTER_RESEARCH_SNAPSHOT:-0} != 1 ]]; then
  mkdir -p .audit
  SNAPSHOT=$(mktemp "$PWD/.audit/lighter-runner.XXXXXX.sh")
  cp "${BASH_SOURCE[0]}" "$SNAPSHOT"
  export LIGHTER_RESEARCH_SNAPSHOT=1
  exec bash "$SNAPSHOT" "$@"
fi
DECLARATION=${1:?usage: research-lighter-optimization.sh DECLARATION_JSON}
cargo build --release --manifest-path service/Cargo.toml -p crypto-portfolio
CP=$(mktemp "$PWD/.audit/lighter-portfolio.XXXXXXXX")
cp -p service/target/release/crypto-portfolio "$CP"
python3 - "$DECLARATION" <<'PY'
import json, pathlib, re, sys
path = pathlib.Path(sys.argv[1])
d = json.loads(path.read_text())
base_suite = json.loads(pathlib.Path(d['source_suite']).read_text())
if d.get('lighter_hourly'):
    base_suite['lighter_execution']['hourly'] = d['lighter_hourly']
for key in ('execution_hours', 'valuation_hours'):
    if key in d:
        base_suite['lighter_execution'][key] = d[key]
base = pathlib.Path(d['source_config']).read_text()
root = pathlib.Path(d['output'])
root.mkdir(parents=True, exist_ok=True)
def keep(path, value):
    if path.exists():
        if path.read_text() != value:
            raise SystemExit(f'refusing changed artifact: {path}')
    else:
        path.write_text(value)
scenarios = []
for candidate in d['candidates']:
    text = base
    for section, values in candidate.get('overrides', {}).items():
        for key, value in values.items():
            start = text.index(f'[{section}]')
            end = text.find('\n[', start + 1)
            if end < 0: end = len(text)
            fragment = text[start:end]
            fragment = re.sub(r'^' + re.escape(key) + r'\s*=.*\n?', '', fragment, flags=re.M)
            fragment += f'\n{key} = {json.dumps(value)}\n'
            text = text[:start] + fragment + text[end:]
    config = root / 'configs' / candidate['id']
    config.mkdir(parents=True, exist_ok=True)
    keep(config / 'config-1.toml', text)
    for cost in d['costs']:
        scenario = next(s for s in base_suite['scenarios'] if s['id'] == cost).copy()
        scenario.update(id=candidate['id']+'-'+cost, config_dir=str(config))
        scenario['execution'] = {**scenario['execution'], **candidate.get('execution', {})}
        scenarios.append(scenario)
base_suite.update(out_dir=str(root/'replay'), scenarios=scenarios)
keep(root/'suite.json', json.dumps(base_suite, indent=2)+'\n')
manifest = {'experiments': [dict(id=s['id'],family='lighter-optimization',status='development',trial_count=1,
    notes=['Declared candidate on reused development dates; native execution with disclosed liquidity proxies.'],
    fold_dir=str(root/'replay'/s['id']),expected_folds=1,initial_cash=s['initial_cash'],
    sources=[str(path),str(root/'suite.json'),str(root/'replay'/'inputs.json')]) for s in scenarios]}
keep(root/'experiments.json',json.dumps(manifest,indent=2)+'\n')
PY
OUT=$(jq -er .output "$DECLARATION")
"$CP" replay-suite --manifest "$OUT/suite.json"
"$CP" experiment-register --manifest "$OUT/experiments.json" --out "$OUT/register.json"
jq -r '.experiments[]|[.experiment.id,.pooled_daily_sharpe,.folds[0].total_return]|@tsv' "$OUT/register.json"
