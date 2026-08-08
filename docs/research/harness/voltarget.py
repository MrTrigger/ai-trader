"""Volatility targeting on the fast book, and the drawdown brake section 6 specifies.

Neither is a signal change. Both read only the book's OWN realised risk, never
its returns, so they are risk control rather than another turn of the parameter
search. That distinction is the only reason to run configuration ~100.

  vol target   scale gross by min(1, target / trailing_vol). CAP ONLY - 9.2
               holds gross at <= 1.0, so this can de-risk and never lever.
  dd brake     section 6's "max drawdown -> auto-pause", which the spec lists and
               nothing has ever implemented. Below the threshold, gross is cut
               until a new high.

Both use an 8-week trailing window and are strictly point-in-time.
"""
import json, math, statistics, sys
from pathlib import Path

R=Path("/home/magnus/dev/magnus/ai-trader/docs/research")
lean=json.load(open(R/"lean-recosted.json"))["state_x"]["8.0"]

def weekly(c): return [(c[i][0], c[i][1]/c[i-1][1]-1) for i in range(1,len(c))]
FR=weekly(lean["fresh"]["curve"]); OR=weekly(lean["orig"]["curve"])

def apply(rets, *, target=None, dd_cut=None, dd_floor=0.5, win=8):
    """Re-run an equity curve under a risk overlay. Exposure is decided from
    information available BEFORE each week's return."""
    eq, peak, curve, exps = 1.0, 1.0, [], []
    hist=[]
    for d, r in rets:
        e = 1.0
        if target is not None and len(hist) >= win:
            v = statistics.stdev(hist[-win:]) * math.sqrt(52)
            if v > 0: e = min(1.0, target / v)
        if dd_cut is not None and eq/peak - 1 <= -dd_cut:
            e = min(e, dd_floor)
        eq *= 1 + e * r
        peak = max(peak, eq)
        hist.append(r); curve.append(eq); exps.append(e)
    rr=[curve[i]/curve[i-1]-1 for i in range(1,len(curve))]
    m=sum(rr)/len(rr); sd=statistics.stdev(rr)
    pk, dd = curve[0], 0.0
    for v in curve: pk=max(pk,v); dd=min(dd, v/pk-1)
    return {"net":eq-1,"sharpe":(m*52)/(sd*math.sqrt(52)),"maxdd":dd,
            "vol":sd*math.sqrt(52),"mean_exposure":sum(exps)/len(exps)}

def show(title, rows):
    print(f"\n{title}")
    print(f"{'setting':>10}{'fresh Sh':>10}{'fresh DD':>10}{'orig Sh':>10}{'orig DD':>10}"
          f"{'combined':>11}{'min Sh':>8}{'mean exp':>10}")
    base=None
    for label, f, o in rows:
        ms=min(f["sharpe"],o["sharpe"]); comb=(1+f["net"])*(1+o["net"])-1
        if base is None: base=ms
        print(f"{label:>10}{f['sharpe']:>10.2f}{f['maxdd']*100:>9.1f}%{o['sharpe']:>10.2f}"
              f"{o['maxdd']*100:>9.1f}%{comb*100:>10.1f}%{ms:>8.2f}{o['mean_exposure']*100:>9.0f}%"
              f"{'  <- control' if label in ('off','none') else ('  better' if ms>base else '')}")

show("=== VOLATILITY TARGET (cap only; gross never exceeds 1.0) ===",
     [("off", apply(FR), apply(OR))] +
     [(f"{t*100:.0f}%", apply(FR,target=t), apply(OR,target=t))
      for t in (0.60,0.50,0.40,0.35,0.30,0.25)])

show("=== DRAWDOWN BRAKE (halve gross below the threshold, section 6) ===",
     [("none", apply(FR), apply(OR))] +
     [(f"{c*100:.0f}%", apply(FR,dd_cut=c), apply(OR,dd_cut=c))
      for c in (0.25,0.20,0.15,0.10)])

show("=== BOTH: vol target 40% + drawdown brake ===",
     [("neither", apply(FR), apply(OR))] +
     [(f"dd {c*100:.0f}%", apply(FR,target=0.40,dd_cut=c), apply(OR,target=0.40,dd_cut=c))
      for c in (0.25,0.20,0.15)])