import hashlib,json,pathlib
root=pathlib.Path('var/research/lighter-optimization-r32-ensembles')
checks=[]
for cost in ['central','stress']:
    checks.extend([
        (f'var/research/lighter-optimization-r26-absolute/replay/absolute-directional-{cost}/fold-1.json',f'var/research/lighter-optimization-r31-native-labels/replay/external-label-control-{cost}/fold-1.json'),
        (f'var/research/lighter-optimization-r26-absolute/replay/absolute-directional-{cost}/fold-1.json',str(root/f'replay/external-single-{cost}/fold-1.json')),
        (f'var/research/lighter-optimization-r31-native-labels/replay/native-hourly-absolute-{cost}/fold-1.json',str(root/f'replay/native-single-{cost}/fold-1.json')),
    ])
reports=[]
for a,b in checks:
    pa,pb=pathlib.Path(a),pathlib.Path(b)
    x,y=json.loads(pa.read_text()),json.loads(pb.read_text())
    assert len(x['steps'])==len(y['steps'])==285
    keys=set(x['steps'][0])-{'plan_id','warnings'}
    assert keys=={'as_of','attribution','cash','fills','gross_exposure','nav','net_exposure','status'}
    assert all({k:r[k] for k in keys}=={k:s[k] for k in keys} for r,s in zip(x['steps'],y['steps']))
    assert x['metrics']==y['metrics'] and x['execution']==y['execution']
    reports.append({'reference':a,'candidate':b,'reference_sha256':hashlib.sha256(pa.read_bytes()).hexdigest(),'candidate_sha256':hashlib.sha256(pb.read_bytes()).hexdigest(),'financial_steps_exact':285,'metrics_exact':True,'execution_exact':True})
(root/'control-parity.json').write_text(json.dumps(reports,indent=2)+'\n')
print('All six controls match 285 daily financial steps, metrics and execution exactly.')
