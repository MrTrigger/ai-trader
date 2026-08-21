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
- **Sizing (vol-targeted risk parity):** per-coin weight
  `w_i = s_i · (σ_target / σ_i) / N`, `σ_i` = 30-day annualised vol (the
  existing `x_vol_30`), `σ_target = 0.40` per-coin budget; book scaled so
  **realized book vol targets 20% annualised** (estimated from the trailing
  60-day book return; scale capped so gross ≤ 1.0). `max_position = 0.15`.
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
