"""Chain the two windows into one record carrying BOTH equity conventions."""
import json, statistics
from pathlib import Path
from datetime import date

src=json.loads(Path("docs/research/position-cap.json").read_text())
key=[k for k in src if k.startswith("capped")][0]
fr,og=src[key]["fresh"],src[key]["orig"]

comp=[list(p) for p in fr["curve"]]
tail=comp[-1][1]
comp+=[[d,v*tail] for d,v in og["curve"]]
fix=[list(p) for p in fr["simple_curve"]]
off=fix[-1][1]-1.0
fix+=[[d,v+off] for d,v in og["simple_curve"]]

def dd(c):
    pk,out=c[0][1],[]
    for d,v in c: pk=max(pk,v); out.append([d,v/pk-1])
    return out
def worst(c):
    pk,lo,at=c[0][1],0.0,None
    for d,v in c:
        pk=max(pk,v)
        if v/pk-1<lo: lo,at=v/pk-1,d
    return lo,at

# On a constant stake a drawdown is measured in units of that stake, so the
# 2021 and 2025 episodes become directly comparable - which they are not on a
# compounding curve, where a later loss is arithmetically larger for no reason
# other than the account having grown.
def span(c,a,b):
    seg=[p for p in c if a<=p[0]<b]
    return (seg[-1][1]-seg[0][1]) if len(seg)>1 else 0.0

print(f"fixed-budget P&L, in units of the constant stake:")
for y in range(2019,2027):
    s=span(fix,f"{y}-01-01",f"{y+1}-01-01")
    if s: print(f"   {y}   {s*100:+7.1f}%  of stake")

print(f"\ndrawdown, the two episodes you asked about:")
for lab,a,b in (("2021-09..2021-12","2021-09-01","2021-12-31"),
                ("2025-07..2026-08","2025-07-01","2026-08-31")):
    fc=[p for p in fix if a<=p[0]<=b]; cc=[p for p in comp if a<=p[0]<=b]
    fw,_=worst(fc); cw,_=worst(cc)
    print(f"   {lab}   fixed-budget {fw*100:6.1f}% of stake"
          f"    compounded {cw*100:6.1f}% of NAV")

rec={"window":[comp[0][0],comp[-1][0]],"split":og["curve"][0][0],
     "compounded":comp,"fixed":fix,
     "compounded_dd":dd(comp),"fixed_dd":dd(fix),
     "compounded_final":comp[-1][1]-1,"fixed_final":fix[-1][1]-1,
     "compounded_maxdd":worst(comp)[0],"fixed_maxdd":worst(fix)[0],
     "years":[{"year":y,"pnl":span(fix,f"{y}-01-01",f"{y+1}-01-01")}
              for y in range(2019,2027) if span(fix,f"{y}-01-01",f"{y+1}-01-01")]}
Path("docs/research/fixed-budget.json").write_bytes((json.dumps(rec,indent=2)+"\n").encode())
print(f"\ncompounded {rec['compounded_final']*100:.1f}%   "
      f"fixed-budget {rec['fixed_final']*100:.1f}%   over {len(comp)} weeks")