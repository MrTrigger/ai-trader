import hashlib, json, pathlib
root = pathlib.Path('var/research/lighter-ranker-r31-native-hourly')
reference = pathlib.Path('var/research/lighter-ranker-r24-context/matrix/matrix.jsonl')
expected = '7908f995a691b199cf50ccd958871b88672b0917a7ffda5f97e171212073a1c0'
assert hashlib.sha256(reference.read_bytes()).hexdigest() == expected
old = [json.loads(line) for line in reference.read_text().splitlines()]
path = root / 'matrix/matrix.jsonl'
new = [json.loads(line) for line in path.read_text().splitlines()]
assert old[0]['features'] == new[0]['features']
assert new[0]['label_prices'] == 'native-hourly'
old, new = old[1:], new[1:]
assert len(old) == len(new) == 6486
assert len({r['date'] for r in new}) == 241
assert len({r['asset'] for r in new}) == 45
keys = ['date', 'slot', 'asset', 'vol', 'features']
for a,b in zip(old,new):
    assert {k:a[k] for k in keys} == {k:b[k] for k in keys}
changed = sum(a['raw_ret'] != b['raw_ret'] for a,b in zip(old,new))
assert changed > 0
result = {'reference_sha256':expected, 'native_matrix_sha256':hashlib.sha256(path.read_bytes()).hexdigest(), 'rows':6486,'dates':241,'assets':45,'all_identity_features_vol_exact':True,'changed_raw_return_labels':changed,'gate':'passed before first replay; stricter audit added after initial model fit'}
(root / 'strict-matrix-parity.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result))
