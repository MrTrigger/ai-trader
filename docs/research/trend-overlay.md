# Trend overlay — pre-registered (2026-08-19, before any number exists)

## What this is

The ranker is a cross-sectional model with no timing skill (the label cannot
express direction; three labels that could were tested and failed —
`docs/research/absolute-label-unbalanced.md`). If the book is to lean with
the market, the lean must come from a component whose target *is* the
market, sitting in the construction layer: the ranker keeps choosing names
long and short; an overlay sets how much **net** the book runs. This note
fixes that overlay — inputs, rule, band, evaluation, decision — before it is
built or run. One rule, no scan, no learned model (≈2,000 daily rows is
where a learned timer overfits; the literature's surviving evidence at this
horizon is trend/vol rules — research round §4).

## The rule

All inputs are the planner's existing point-in-time daily features for the
eligible universe (newest feature date = decision date − 1; nothing new is
fetched, nothing is forward-looking).

- **Breadth trend.** For h ∈ {30, 90} days: `b_h = 2 · share(x_ret_h > 0) − 1`
  over eligible names (−1 = every name down over the window, +1 = every name
  up). `trend = (b_30 + b_90) / 2 ∈ [−1, 1]`. Two horizons, fixed a priori
  (7 is noise, 180 is a different strategy); equal weight; no smoothing.
- **Net target.** `N = band · trend`, **band = 0.30** of NAV (a risk-appetite
  number: at full tilt the book is 0.55 long / 0.25 short on 0.80 gross, or
  the mirror). No vol scaling (one part fewer to defend; vol enters only via
  the ranker's own edge/vol sizing).
- **Budget → sleeves.** `G = 0.80` unchanged. `long_sleeve = (G + N)/2`,
  `short_sleeve = (G − N)/2`. The ranker's selection is unchanged (top 24 by
  |E[r]| across both sides, cost floor, two-sided requirement); within each
  side, names are sized by edge/vol to the side's sleeve — i.e. `risk_adjusted`
  with asymmetric sleeves. Constructor name `risk_adjusted_tilted`. If the
  list has < 2 names on a side the two-sided rule still refuses (unchanged).
- **Guard.** For this test `max_net_exposure` is unset. If the overlay is
  ever adopted, the guard's quantity becomes `|net − N|` (deviation from the
  requested tilt), band 0.10, same release rule.

## What is run

`bin/replay-folds.sh` on the frozen fold models (`var/research/wf-rank-2020`,
the ranker is NOT retrained), constructor `risk_adjusted_tilted`, everything
else the frozen recipe; nine folds, funding charged, 1× slippage; baseline =
the same models under `risk_adjusted` (`wf-rank-2020-replay-base`, with the
settlement fix applied to both sides so they are binary-identical).

Reported: per-fold Sharpe / return / maxDD / turnover; realized net
p10/p50/p90 per fold; the overlay's own timing statistic —
`corr(N_t, EW-market return_{t+1})` and the share of days N < 0 inside
30-day drawdowns > 20% — so that if it helps we know *why*, and if it hurts
we know whether the rule timed nothing or timed badly; bootstrap 90%
interval (`crypto-portfolio compare`), 20-day blocks.

## Decision rule

Adopt the overlay only if it beats the flat book with a bootstrap 90%
interval excluding zero **and** the 2022 folds' max drawdown is not worse.
Indistinguishable → keep flat (simpler, has the paper track record). Worse →
closed; no band, horizon, weighting or smoothing is revisited against these
folds. One rule, one run, then the number is the number.

## Expectation, written now

Breadth trend at 30/90 days is a real but slow signal: it will be net short
through most of 2022 and net long through most of 2021 and 2024 — and late
at every turn (long into May 2021 and Nov 2021, short into the 2023 recovery).
Vol-targeted trend on liquid crypto is ~0.6–1.2 Sharpe stand-alone in the
better sources; bolted onto a Sharpe-2 flat book at 0.3 band it most likely
adds drawdown in the turns and a little return in the trends, netting near
zero. If it wins it wins on 2022.

## Results (2026-08-19, same day, run as registered)

Nine folds, frozen fold models, replay only; both sides on the same binary
(settlement fix applied).

| | flat (`risk_adjusted`) | overlay (`risk_adjusted_tilted`, band 0.30) |
|---|---:|---:|
| mean Sharpe | 2.11 | 1.74 |
| compounded | +1189% | +1309% |
| folds positive | 9/9 | 9/9 |
| mean max drawdown | −11.6% | −13.9% |
| worst fold drawdown | −17.6% | −19.7% |
| turnover / rebalance | 0.809 | 0.810 |
| realized net p5 / p50 / p95 | −0.02 / 0.00 / +0.06 | −0.22 / −0.01 / +0.26 |

Per-fold Sharpe flat → overlay: 5.57→5.28, 2.83→1.86, **1.98→1.75**,
1.56→0.58, 1.86→1.74, 0.28→0.60, 1.70→1.63, 2.41→1.70, 0.83→0.54. Per-fold
return flat → overlay: +112→+166, +53→+38, **+30→+41 (2022 H1)**, +17→+7,
+25→+27, +3→+8, +25→+29, +43→+33, +14→+8. 2022 H1 max drawdown −10.9% →
−12.1%.

`crypto-portfolio compare`: **observed Δ −0.29, 90% interval [−0.61, +0.02]**
— includes zero; the evidence does not separate them, and the point estimate
is against.

**The overlay's own timing (reported as promised).** corr(N_t, BTC_{t+1}) =
0.029. Inside 30-day drawdowns > 20% the book was net short on **92%** of
days (median −0.16); inside 30-day rallies > 20% net long (median +0.19).
Monthly, Nov-2021..Jul-2022: +0.16, −0.08, −0.07, −0.20, −0.16, −0.04,
−0.13, −0.23, −0.13 — short through the whole bear, one month late at the
top, exactly as the expectation paragraph said. It is the first directional
variant on this record whose tilt points the right way in the bear.

**Reading.** The rule does what it claims: it is short in bears and long in
bulls, it lifts 2022 H1 (+30% → +41%) and the compounded total (+1189% →
+1309%), and it keeps 9/9 folds positive. It pays for that at the turns and
in chop — folds 2 (into the Nov-2021 top), 4 (the 2022-23 bottom), 8 and 9 —
and the net effect on risk-adjusted return is negative by a third of a
Sharpe with an interval that touches zero. A slow trend signal bolted onto a
Sharpe-2 flat book adds a little return and more variance; per unit of risk
it is not an improvement on this record.

**Decision (per the rule).** Not adopted: it does not beat the flat book with
an interval excluding zero, and the 2022 drawdown is marginally worse, not
better. Indistinguishable-to-worse → keep flat. No band, horizon, weighting or
smoothing is revisited. `risk_adjusted_tilted` and `breadth_trend` stay in
the code as the measured alternative. If the overlay is ever reconsidered it
should be on *live* evidence of a sustained bear where the flat book's
kill-criteria are under pressure — the one regime where it measurably helped.

## Sweep with holdout — pre-registered (2026-08-19, before any run)

User direction: optimise the overlay for P&L and Sharpe. A sweep scored on
all nine folds would find an in-sample winner by arithmetic; this one is
scored on a **tuning set** and judged once on a **holdout**.

- Tuning set: folds 1–5 (2020-09-18..2023-12-16). Holdout: folds 6–9
  (2023-12-17..2026-07-30), not read until one config is selected.
- Grid, fixed now (24 configs): `band ∈ {0.15, 0.30, 0.45}` ×
  `horizons ∈ {(7,30), (30,90), (7,90), (90,180)}` days ×
  `vol_scale ∈ {off, on}` where *on* multiplies N by
  `min(1, 0.60 / median_30d_annualised_vol of the eligible universe)`.
  Breadth definition unchanged (`2·share(ret_h>0) − 1`, mean over the two
  horizons). Ranker, gross, costs, folds unchanged.
- Selection: highest mean Sharpe over folds 1–5. One look.
- Decision: adopt only if the selected config beats the flat book on folds
  6–9 with a bootstrap 90% interval excluding zero; otherwise the sweep is
  recorded and the overlay stays unadopted. 24 trials are reported as 24
  trials; the in-sample surface is reported in full so a plateau can be told
  from a spike.
- Expectation, written now: the in-sample winner will show +0.2–0.5 Sharpe
  over flat on folds 1–5 (selection effect on ~1,200 days); on the holdout
  it will most likely be within noise of flat, as the unswept overlay was.

All four horizons are features the planner already carries (`x_ret_7`,
`x_ret_30`, `x_ret_90`, `x_ret_180`), and the vol scalar uses `x_vol_30`
(annualised). Nothing new is computed. *(Corrected before any run: the first
draft listed 20/60/120-day horizons, which are not carried as features.)*

### Sweep results (2026-08-19, run as registered; 24 configs × folds 1–5, then one config × folds 6–9)

Flat book on the tuning folds: mean Sharpe 2.759, compounded +518%. **No
config beats it.** Ranked by tuning-set mean Sharpe (Δ vs flat):

| rank | config | mean Sharpe | Δ | compounded | worst DD |
|---:|---|---:|---:|---:|---:|
| 1 | b0.15 h7/30 vol-on | 2.750 | −0.009 | +553% | −15.7% |
| 2 | b0.15 h7/90 vol-on | 2.732 | −0.027 | +547% | −16.4% |
| 3 | b0.15 h30/90 vol-on | 2.703 | −0.055 | +533% | −16.0% |
| 4 | b0.30 h7/30 vol-on | 2.671 | −0.088 | +597% | −16.6% |
| … | | | | | |
| 12 | b0.45 h7/30 vol-on | 2.445 | −0.314 | +576% | −18.1% |
| 17 | b0.30 h30/90 vol-off (the registered base) | 2.241 | −0.517 | +600% | −19.7% |
| 24 | b0.45 h90/180 vol-off | 1.841 | −0.918 | +664% | −21.0% |

The surface is monotone and smooth, not spiky: Sharpe falls with the band
(0.15 → 0.30 → 0.45: −0.05 / −0.35 / −0.60 on average), vol-scaling helps
at every band (it shrinks the tilt), horizons barely matter. Compounded
return rises with the band (the beta), Sharpe falls with it, drawdowns
deepen with it. The optimum of this family on the tuning set is "as little
overlay as possible": the winner is within rounding of flat.

**Holdout, the one look** (selected `b0.15_h7-30_von`, folds 6–9): flat
0.28 / 1.70 / 2.41 / 0.83 → overlay 0.32 / 1.59 / 2.20 / 0.61; mean 1.305 →
1.179; bootstrap **Δ −0.13, 90% interval [−0.27, +0.02]**, variant ahead in
7% of resamples. Worse, not separable, on the folds it never saw.

**Decision.** Not adopted. The sweep found no configuration of a breadth-
trend overlay that improves the flat book's risk-adjusted return on this
record — in-sample or out — and the ones that raise total return do so by
carrying market beta, which the capital plan's kill criteria price at more
than it pays. The question "can a trend overlay improve P&L and Sharpe" is
answered on evidence for this rule family: P&L yes (with beta), Sharpe no.
24 trials are recorded above; nothing is adopted from them.

## Shadow measurement (2026-08-19, live)

Prompted by the live paper book sitting flat through the 2026-08-21 rally
(BTC +10%/24h; the book earned the spread, +$989 unrealized on the day,
after a −3.9% squeeze dip — both exactly the flat design). The overlay would
currently lean long; the backtest says that lean does not pay on average;
the honest arbiter is live data. Every plan now records a warning line —
`shadow overlay: breadth trend T (h30/h90) would target net N at band 0.30;
book targets net 0` — whatever constructor runs. The paper run's records
(Postgres, run history) therefore accrue the overlay's daily request
alongside realized returns, and in a few months "what would the lean have
earned, live" is a query, not a backtest. The book itself is unchanged.
