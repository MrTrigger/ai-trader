# crypto-directional — a separate, directional portfolio bot (design + protocol)

Pre-registered 2026-08-21, before any signal code or number exists. User
decision: the frozen cross-sectional bot (`crypto-portfolio`) is not touched;
direction gets its own bot, its own capital sleeve, and a real optimization
effort under holdout discipline.

## 1. Why this is not contradicted by the 2026-08-19 experiments

Those experiments rejected (a) direction inside the ranker's label (D/F: the
label imports the bull drift, corr with next-day market ≈ 0.01) and (b) a
trend overlay on the flat book's gross (−0.29 Sharpe: it dilutes a Sharpe-2
book). Neither tested a standalone trend book with its own vol budget. The
outside evidence for that object is the strongest of any strategy class for
a $50–500k account (`docs/scalper-research-round-2026-08.md` §4): time-series
trend on 10–20 liquid coins, daily bars, vol-targeted, net Sharpe 0.6–1.2,
long flat spells, shines in sustained regimes — 2022 included, because TSMOM
is short a bear. Expectation is written there and binds here: **this is a
Sharpe ~1 project, not a Sharpe 2 project.** Its value is the return stream's
low correlation to the spread book, judged at the portfolio level.

## 2. The bot

Same architecture as `crypto-portfolio` (same crate, new `bot_id`
`crypto-directional`, own config, own paper book): daily cycle, same data
pull, same point-in-time universe, same cost model, funding charged, same
plan/risk/dashboard machinery. What differs is signal and construction:

- **Universe:** top 15 by rank-day liquidity among perp-listed names (Man:
  risk-adjusted return peaks at 10–15 coins; beyond that costs win).
- **Signal (`tsmom_ensemble`), per coin, deterministic — no ML:**
  `s_i = mean over h ∈ {30, 90, 180} of sign(ret_h)` ∈ {−1, −⅓, +⅓, +1}.
  Direction = sign(s_i), zero s_i = flat. Three canonical horizons, equal
  weight; no tuning in round 1.
- **Sizing (vol parity, ex-ante):** `w_i ∝ s_i / σ_i` (`σ_i` = the existing
  30-day annualised `x_vol_30`), normalised so `Σ|w_i|·σ_i = 0.25` — an
  ex-ante book-vol proxy under full correlation, i.e. deliberately
  conservative; realised book vol will land below 25%. Gross capped at 1.0,
  `max_position = 0.15`, whole-book scale when a cap binds. *(Amended before
  any run: the first draft targeted realised trailing book vol, which needs
  state the stateless daily planner does not carry; trailing-vol targeting
  is a later, pre-registered axis, not a round-1 feature.)*
- **Both directions**, no balance constraint — the whole book may be short
  (2022) or long (now). This is the user's directional instinct in the
  object where it belongs.
- **Cadence:** daily. Turnover is naturally low (signs flip rarely); the
  existing cost deadband applies.

## 3. Protocol (binds every round)

- Walk-forward on the nine fold windows of `wf-rank-2020` (2020-09-18..
  2026-07-30) — deterministic signal, so "training" is nothing and the folds
  are pure out-of-sample pricing; funding charged, 1× slippage, settled
  backtest.
- **Benchmarks, all three reported every round:** (i) vol-targeted (20%)
  BTC buy-and-hold, (ii) the flat book's settled record (2.11), (iii) a
  50/50 capital split flat + directional, monthly rebalanced.
- **Gate to reach the cluster paper book:** net Sharpe of the directional
  book ≥ 1.0 over the nine folds with a bootstrap 90% interval vs the
  vol-targeted-BTC benchmark excluding zero, **and** 2022 (folds 3–4)
  profitable — a trend book that lost the bear is not a trend book — **and**
  the 50/50 portfolio's Sharpe ≥ the flat book's alone (else it adds risk,
  not diversification).
- **Optimization ("really try"):** permitted, unlimited rounds, but every
  round is (grid pre-registered → scored on folds 1–5 → one selection → one
  look at folds 6–9), exactly like the overlay sweep. Trials are counted and
  reported. The holdout is never used for selection. Axes expected: horizon
  set, vol target, universe size, cadence, entry/exit asymmetry, breakout vs
  MA vs sign ensembles. One axis per round.
- Kill criteria for the paper phase, written now: DD > 30% (trend runs
  deeper than the spread book), 6 months with realized Sharpe < 0, or costs
  > 1.5× model.

## 4. What would make this fail honestly

Chop: 2023-24 sideways stretches will bleed; the 2022 short leg and 2020-21/
2024-25 longs must pay for them. If round 1 lands under Sharpe ~0.5 on the
nine folds, the literature numbers were flattered by their samples and this
project stops at one round rather than sweeping its way to a mirage.

## Round 1 results (2026-08-21, run as registered)

Nine fold windows, deterministic signal, funding charged, 1× slippage:

| fold | window | Sharpe | return | maxDD | ann vol | net p10/p50/p90 |
|---:|---|---:|---:|---:|---:|---|
| 1 | 2020-09..2021-05 | **3.09** | +45.1% | −5.6% | 19% | +0.15/+0.18/+0.32 |
| 2 | 2021-05..2022-01 | −0.01 | −0.7% | −9.1% | 14% | −0.02/+0.09/+0.24 |
| 3 | 2022-01..2022-08 | −0.22 | −3.4% | −13.0% | 17% | −0.24/−0.18/+0.04 |
| 4 | 2022-08..2023-04 | **−1.62** | **−15.6%** | −17.0% | 16% | −0.38/−0.08/+0.31 |
| 5 | 2023-04..2023-12 | 0.24 | +1.3% | −9.5% | 11% | −0.32/−0.01/+0.27 |
| 6 | 2023-12..2024-08 | 0.73 | +6.8% | −7.9% | 16% | −0.15/+0.19/+0.35 |
| 7 | 2024-08..2025-04 | −0.62 | −6.4% | −13.5% | 15% | −0.20/+0.16/+0.26 |
| 8 | 2025-04..2025-11 | −0.20 | −3.0% | −11.2% | 17% | −0.22/+0.23/+0.45 |
| 9 | 2025-11..2026-07 | 0.40 | +3.5% | −8.4% | 16% | −0.32/−0.22/+0.06 |

**Mean Sharpe 0.198, compounded +19.3%, 4/9 positive.** Benchmark
vol-targeted-20% BTC over the same window: +125%, Sharpe 0.74. The gate
(≥1.0, beat the benchmark, 2022 profitable) fails on every criterion.
Mechanics: one clean trend (fold 1, the 2020-21 run, Sharpe 3.1), then
whipsaw. The book *did* go short in 2022 (fold 3 net median −0.18) and
still lost slightly, because it turned short late and paid every relief
rally; fold 4 was short into the Jan-2023 V-recovery for −15.6%. Turnover
0.05–0.11/rebalance — cost drag is negligible; the failure is the signal,
not the execution. This replicates the *honest* end of the literature
(simple daily TSMOM ≈ 0.4 gross, negative 2022–23) rather than the
marketed end (ensemble numbers on flattered samples).

## Stop (per §4, applied as written)

Round 1 landed under Sharpe 0.5. **The project stops at one round.** The
whipsaw losses are structural to daily trend on this asset class in
2021–2026 — no axis in the §3 sweep list (horizons, vol target, breadth of
universe, cadence, rule family) removes reversal risk; a sweep winner would
be selected precisely against the 2022–23 whipsaws the base rule failed on,
which is the mirage §4 names. The standing directional experiments are the
live ones: the shadow-overlay line accruing daily in the paper run, and the
flat book's own conditional record (+19/+21 bp on big days both ways).
Reopening requires either live shadow evidence that a lean pays, or a
directional signal from outside this data (not price/breadth of the same
bars). The frozen cross-sectional bot is unaffected; `tsmom_ensemble` /
`tsmom_vol_parity` / `config/directional.toml` remain in the repo as the
measured round-1 record.


## Round 2 authorized (2026-08-21, user override of §4)

User decision, recorded verbatim in intent: round 1 "went way too fast";
the §4 stop is overridden. Round 2 scope, set by the user: (1) research
state-of-the-art thinking for directional crypto across horizons; (2) train
model(s) tailored to this bot — i.e. a market-direction target of its own,
not the ranker's label and not a three-line rule. Round 1 (Sharpe 0.198)
stands as the baseline round 2 must beat. Discipline unchanged: research →
pre-registered design (features, label horizon, model family, folds 1–5
tuning / 6–9 holdout, gate) → build → one holdout look per round, trials
counted. The research round's outputs will be recorded in
docs/research/directional-round-2-research.md before any design is fixed.

**Scope addition (user, same day): the horizon is an axis.** Round 2 tests
multiple label horizons / holding periods rather than assuming daily. The
testable range is hours → weeks: sub-hour holds are excluded by the scalper
record's cost arithmetic (gate runs 4–6: predictable component a few bp vs
11–25 bp taker round trip, maker adversely selected — the bound binds any
signal at that holding period, directional included) unless the horizon
research overturns it with costed evidence. Candidate label horizons for the
design, to be fixed after the research lands: {12h, 1d, 3d, 7d, 14d, 30d};
sub-daily variants use the existing hourly features and the backtest's
cadence-hours grid. Each tested version follows the same tuning/holdout
discipline; versions are counted as trials.

## Round 2 pre-registration (2026-08-21, after the research, before any number)

Research basis: `docs/research/directional-round-2-research.md`. Round 1
(0.198) is the baseline. Realistic target from the evidence: 0.5–1.0; the
§3 adoption gate (≥1.0 net, beat vol-targeted BTC with interval, 2022
profitable, 50/50 with the flat book ≥ flat alone) is unchanged — a round-2
winner below the gate is recorded, not deployed.

**Horizon decision (from §1 of the synthesis):** versions live at
days-to-weeks effective holds, expressed through signal speed and exit
style. Sub-12h versions are excluded — zero published net-positive results
at VIP0 costs, and our own sub-hour record — unless a later round brings
maker-verified sub-5bp execution evidence.

**Versions (each a counted trial; tuning = folds 1–5, one holdout look =
folds 6–9; selection by tuning mean Sharpe; the holdout is never used to
select):**

- **V1 — Donchian long-flat ensemble (the anchor, deterministic).**
  Per coin: 9 sub-strategies, lookbacks n ∈ {5,10,20,30,60,90,150,250,360}d.
  Entry: close = n-day max close → long. Exit: close ≤ trailing stop;
  TrailingStop ratchets up at the Donchian channel midpoint. Position =
  mean of the 9 states ∈ [0,1] (graded by ensemble agreement), NO shorts.
  Sizing: weight_i = pos_i · min(0.25/σ90_i, 2)/N over the top-15 by
  rank-day liquidity; gross ≤ 1.0; rebalance drift band 20% (signal trades
  immediate). This is the Zarattini configuration as published, on our
  window, our costs, our universe machinery.
- **V2 — V1 with symmetric shorts** (Donchian low entry, ratcheting stop
  above): measures the long-flat-vs-long-short ablation on OUR record.
- **V3 — vol-managed slow TSMOM with continuous response:** round-1's
  30/90/180 horizons but response = clipped z of trend (±2), σ90-scaled,
  book vol-targeted, weekly decision grid. Tests Kang–Ryu/Man's "slow +
  risk-managed" claim against V1's "fast + asymmetric exits".
- **V4 — trained market-direction model (the user's ask), two variants:**
  (a) market-level LightGBM, one row per day, labels = vol-scaled forward
  market return at h ∈ {7d, 14d, 30d} (three label versions, counted),
  features from verified point-in-time sources: multi-horizon breadth and
  trend, realized vol and vol-of-vol, funding aggregates (level, dispersion,
  extremes — in-repo since 2019), OI/positioning changes (Binance metrics
  backfilled to 2020-09), DVOL level/premium (2021+, missing-before
  handled as None), stablecoin supply growth, DXY/rates trend; output →
  net tilt via clipped z into the V3 machinery. (b) per-coin LightGBM with
  a 7d label on the top-15 (per-coin trend/vol/funding + the market
  features), signed vol-parity book. Walk-forward per fold; purged like
  the frozen recipe; TRAIN_* hygiene identical.
- **V5 — best-of composition:** the tuning-set winner among V1–V3 combined
  with V4a as a de-risking condition only (model says strongly against →
  halve the tilt), if and only if both parts individually beat round 1 on
  the tuning set. No other combinations.

**Build order:** Donchian/EWMAC state columns in `features-crypto`
(computed causally from bars, like every feature); V1–V3 run on the
existing replay rig; the V4 data pullers (metrics backfill, DVOL,
stablecoins, FRED) land as `data-pull` extensions with their own manifests;
V4 training in Python per the house rule. Results, trial counts, and the
holdout look are recorded here; the gate decides deployment, not
enthusiasm.
