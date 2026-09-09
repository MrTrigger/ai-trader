"""Draw the Rust-produced equity and drawdown series. No financial math here."""
import argparse, datetime, hashlib, json, pathlib
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from matplotlib.ticker import StrMethodFormatter, PercentFormatter

p = argparse.ArgumentParser()
p.add_argument('--series', required=True, type=pathlib.Path)
p.add_argument('--title', required=True)
p.add_argument('--out', required=True, type=pathlib.Path)
p.add_argument('--strategy', default='strategy', help='curve key prefix; <prefix>_central and <prefix>_stress or <prefix>-central/-stress')
a = p.parse_args()
a.out.mkdir(parents=True, exist_ok=False)
doc = json.loads(a.series.read_text())
dates = [datetime.datetime.fromisoformat(d.replace('Z', '+00:00')) for d in doc['dates']]
curves = doc['curves']
def key(suffix):
    for sep in ('_', '-'):
        k = f'{a.strategy}{sep}{suffix}'
        if k in curves:
            return k
    return f'{a.strategy}_{suffix}'
lines = [
    (key('central'), 'Strategy, central costs', '#1769aa', 1.8),
    (key('moderate'), 'Strategy, moderate costs (3x spread, 2x impact)', '#4d8fc4', 1.2),
    (key('stress'), 'Strategy, stressed costs (10x spread, 4x impact)', '#9cc3e0', 1.0),
    ('btc', 'BTC buy and hold', '#c78218', 1.5),
    ('eth', 'ETH buy and hold', '#d9b26a', 1.0),
]
plt.rcParams.update({'font.size': 10, 'axes.spines.top': False, 'axes.spines.right': False})
fig, (equity, dd) = plt.subplots(2, 1, figsize=(12, 8), sharex=True,
                                 gridspec_kw={'height_ratios': [2, 1]}, layout='constrained')
for key, label, color, width in lines:
    if key not in curves:
        continue
    c = curves[key]
    equity.plot(dates, [100000.0 * v for v in c['growth_of_one']],
                label=f"{label}  ({c['total_return']:+.1%}, max DD {c['max_drawdown']:.1%})",
                color=color, linewidth=width)
    dd.plot(dates, c['drawdown'], color=color, linewidth=width, label=label)
equity.axhline(100000.0, color='#888888', linewidth=.7, linestyle=':')
equity.set_yscale('log')
equity.set_ylabel('Account value, USD (log scale)')
equity.yaxis.set_major_formatter(StrMethodFormatter('${x:,.0f}'))
equity.set_title(a.title, loc='left', fontweight='bold')
equity.legend(loc='upper left', frameon=False)
dd.set_ylabel('Drawdown from peak')
dd.yaxis.set_major_formatter(PercentFormatter(1))
dd.axhline(0, color='#888888', linewidth=.7)
span_years = (dates[-1] - dates[0]).days / 365.25
dd.xaxis.set_major_locator(mdates.MonthLocator(interval=6 if span_years > 4 else 3))
dd.xaxis.set_major_formatter(mdates.DateFormatter('%b %Y'))
for ax in (equity, dd):
    ax.grid(axis='y', alpha=.15)
fig.supxlabel('Binance USD-M bars and funding with Lighter cost proxies; expanding walk-forward folds, '
              'model retrained before each. BTC and ETH are gross spot buy and hold on the same daily grid.',
              fontsize=9)
for suffix in ['svg', 'png', 'pdf']:
    fig.savefig(a.out / f'equity.{suffix}', dpi=160)
(a.out / 'inputs.json').write_text(json.dumps({
    'series': {'path': str(a.series), 'sha256': hashlib.sha256(a.series.read_bytes()).hexdigest()},
    'plotter_sha256': hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
    'matplotlib': matplotlib.__version__,
    'financial_math': 'None; draws Rust-chained NAV and drawdown values.',
}, indent=2) + '\n')
print('wrote', a.out)
