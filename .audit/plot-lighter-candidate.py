"""Plot saved Rust NAV/exposure values without recalculating performance."""
import argparse, datetime, hashlib, json, pathlib
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from matplotlib.ticker import StrMethodFormatter, PercentFormatter
p=argparse.ArgumentParser()
p.add_argument('--central',required=True,type=pathlib.Path)
p.add_argument('--stress',required=True,type=pathlib.Path)
p.add_argument('--btc',required=True,type=pathlib.Path)
p.add_argument('--out',required=True,type=pathlib.Path)
a=p.parse_args();a.out.mkdir(parents=True,exist_ok=False)
paths=[a.central,a.stress,a.btc]
series=[json.loads(path.read_text()) for path in paths]
assert all(len(x['steps'])==285 for x in series)
dates=[[datetime.datetime.fromisoformat(r['as_of'].replace('Z','+00:00')) for r in x['steps']] for x in series]
assert dates[0]==dates[1]==dates[2]
plt.rcParams.update({'font.size':10,'axes.spines.top':False,'axes.spines.right':False})
fig,(equity,exposure)=plt.subplots(2,1,figsize=(11,7),sharex=True,gridspec_kw={'height_ratios':[2,1]},layout='constrained')
for x,t,label,color in zip(series,dates,['Strategy, central costs','Strategy, stressed costs','BTC buy and hold'],['#1769aa','#68a5cc','#c78218']):
    equity.plot(t,[float(r['nav']) for r in x['steps']],label=label,color=color,linewidth=1.7)
equity.axhline(float(series[0]['initial_cash']),color='#888888',linewidth=.7,linestyle=':')
equity.set_ylabel('Account value, USD');equity.yaxis.set_major_formatter(StrMethodFormatter('${x:,.0f}'))
equity.set_title('Lighter strategy versus BTC | $100,000 starting capital',loc='left',fontweight='bold')
equity.legend(loc='upper left',frameon=False)
for key,label,color in [('gross_exposure','Gross exposure','#1769aa'),('net_exposure','Net exposure','#b24c63')]:
    exposure.plot(dates[0],[float(r[key]) for r in series[0]['steps']],label=label,color=color,linewidth=1.3)
exposure.axhline(0,color='#888888',linewidth=.7);exposure.set_ylabel('Exposure / NAV')
exposure.yaxis.set_major_formatter(PercentFormatter(1));exposure.legend(loc='upper left',frameon=False,ncol=2)
exposure.xaxis.set_major_locator(mdates.MonthLocator(interval=2));exposure.xaxis.set_major_formatter(mdates.DateFormatter('%b %Y'))
for ax in (equity,exposure):ax.grid(axis='y',alpha=.15)
fig.supxlabel('Reused development dates; daily native 01:00 marks. BTC is gross price-only buy and hold.',fontsize=9)
for suffix in ['svg','png','pdf']:fig.savefig(a.out/f'comparison.{suffix}',dpi=160)
(a.out/'inputs.json').write_text(json.dumps({'inputs':[{'path':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()} for path in paths],'plotter_sha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),'matplotlib':matplotlib.__version__,'financial_math':'None; plots saved Rust NAV and exposure values.'},indent=2)+'\n')
