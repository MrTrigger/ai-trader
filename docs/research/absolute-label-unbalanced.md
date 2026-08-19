# An absolute label for an unbalanced book — pre-registered, then measured

## Pre-registration (2026-08-19, before any model is trained)

**Question.** `docs/research/unbalanced-constructor.md` showed that letting the
ranked list float (`risk_adjusted_unbalanced`) cannot express market direction
with the frozen model, because that model is trained on a **within-date rank**
of return/vol: roughly half the universe ranks above the median every day by
construction, so the list comes out near-neutral (median net +0.02) whatever
the market does. The user's intent — be short-heavy when the market falls, long
when it rises, by buying the best of the ranked list — needs a model whose
sign carries market direction. That has never been trained here: both existing
rewards (`return` = demean(ret), `per_risk` = demean(ret/vol)) are demeaned
within date. This is the fair test the user asked for.

**What is trained.** A new trainer reward `per_risk_abs`: label = 24h
return / vol, **not demeaned** within date, otherwise identical to `per_risk`
(same matrix `data/models/training.jsonl`, `fs-rust-crypto-4`, 67 features;
same LightGBM parameters, objective, seeds, `TRAIN_RANK` unset). The artefact
records `reward: "per_risk_abs"`; Rust inference treats it exactly as
`per_risk` (expected return = |score| × vol, direction = sign of score). No
feature, parameter, cadence or fold change.

**What is run.** `bin/walk-forward.sh 2020-09-18 2026-07-30 9`, the nine
expanding folds of `wf-rank-2020`, two-day purge, funding charged,
1× slippage, from the current matrix — **twice**: once with
`TRAIN_RANK=1 --reward per_risk` (the frozen recipe, re-trained from the
current matrix so every cell shares one matrix), once with
`--reward per_risk_abs`. Each set of fold models is then priced under both
constructors (`risk_adjusted`, `risk_adjusted_unbalanced`) via
`bin/replay-folds.sh`, with `max_net_exposure` unset so the unbalanced book
is seen unconstrained. That is a 2×2:

| | balanced | unbalanced |
|---|---|---|
| rank label (frozen recipe) | A — should reproduce the frozen record | B — already measured: −0.45 [−0.80, −0.14] vs A |
| absolute label | C — control: does the label alone help or hurt a balanced book | **D — the question** |

**Reported, every cell:** per-fold Sharpe, return, max drawdown, turnover,
realized net p10/p50/p90; `crypto-portfolio compare` block-bootstrap delta
and 90% interval for D vs A, C vs A, D vs C.

**Decision rule, written now.** The frozen config changes to the absolute
label and/or the unbalanced constructor **only if D beats A with a bootstrap
90% interval that excludes zero**, and its fold-level behaviour in 2022 (the
one bear market in the sample) is not worse in drawdown than A's. A D that is
indistinguishable from A keeps the frozen config (a ranker hedged flat is the
simpler object and already has the live-paper track record). A D that is
worse closes the question the same way the unbalanced-constructor note did.
No re-tuning of either model against these folds follows from this note;
one matrix, two trainings, four pricings, then the number is the number.

**Expectation, written now.** Absolute 24h direction in crypto is close to
unpredictable at daily horizon in the literature (`docs/scalper-research-round-2026-08.md`
§4: time-series momentum on majors ≈ 0.4–0.6 gross); adding it to the label
most likely adds noise to the ranking and costs Sharpe in C, and D inherits
that plus beta. If D wins, it wins because the model finds a timing signal
the rank label had been discarding — which would be worth knowing and worth
believing only with the interval.

## Results (2026-08-19, same day, run exactly as registered)

Nine folds 2020-09-18..2026-07-30, two-day purge, funding charged, 1×
slippage, `max_net_exposure` unset everywhere. One matrix
(`data/models/training.jsonl`, fs-rust-crypto-4, 67 features), two
trainings (`TRAIN_RANK=1 --reward per_risk`; `--reward per_risk_abs`), four
pricings. The absolute walk-forward's first attempt died on fold 4 with a
`SIGILL` in `String::clone` inside feature preparation — not reproducible,
no panic message, the third unrelated process crash on this new machine in
two days (ghostty ×2, 1Password); it re-ran clean and is recorded here as a
hardware suspicion, not a software finding.

| cell | mean Sharpe | compounded | folds + | mean maxDD | turnover | realized net p5 / p50 / p95 |
|---|---:|---:|---:|---:|---:|---|
| **A** rank × balanced (re-trained) | 2.06 | +1019% | 9/9 | −0.114 | 0.809 | −0.02 / 0.00 / +0.05 |
| **B** rank × unbalanced | 1.55 | +647% | 8/9 | −0.138 | 0.805 | −0.17 / +0.03 / +0.24 |
| **C** absolute × balanced | 1.35 | +493% | 8/9 | −0.134 | 0.757 | −0.24 / 0.00 / +0.18 |
| **D** absolute × unbalanced | **1.03** | +407% | **6/9** | **−0.276** | 0.808 | **−0.69 / +0.23 / +0.77** |
| frozen record (rank × balanced, 9 Aug models) | 2.21 | +1450% | 9/9 | −0.115 | 0.802 | −0.02 / 0.00 / +0.06 |

Per-fold Sharpe:

| fold | window | A | B | C | D | frozen |
|---:|---|---:|---:|---:|---:|---:|
| 1 | 2020-09..2021-05 | 2.89 | 3.00 | 3.64 | 4.55 | 5.64 |
| 2 | 2021-05..2022-01 | 3.67 | 2.12 | 2.89 | −0.09 | 2.83 |
| 3 | 2022-01..2022-08 | 2.46 | 1.39 | 1.99 | −0.77 | 2.02 |
| 4 | 2022-08..2023-04 | 1.01 | 0.71 | 0.47 | 0.66 | 1.80 |
| 5 | 2023-04..2023-12 | 1.36 | 1.46 | 1.24 | 2.42 | 1.86 |
| 6 | 2023-12..2024-08 | 0.59 | −0.26 | 1.37 | 1.30 | 0.28 |
| 7 | 2024-08..2025-04 | 3.19 | 2.35 | −1.21 | 0.85 | 2.17 |
| 8 | 2025-04..2025-11 | 2.57 | 2.35 | 1.30 | 0.68 | 2.41 |
| 9 | 2025-11..2026-07 | 0.82 | 0.83 | 0.48 | −0.30 | 0.91 |

Max drawdown per fold, A vs D: −10/−11%, −10/**−56%**, −13/−34%, −14/−29%,
−14/−15%, −12/**−42%**, −6/−20%, −9/−20%, −16/−21%.

`crypto-portfolio compare` (block bootstrap, 2,133 steps, 20-day blocks):

| comparison | observed Δ | 90% interval | variant ahead |
|---|---:|---|---:|
| **D vs A** (the question) | **−1.16** | **[−2.03, −0.36]** — excludes zero | 1% |
| C vs A (label alone, balanced) | −0.64 | [−1.37, +0.16] | 9% |
| D vs C (construction alone, absolute model) | −0.56 | [−1.44, +0.37] | 16% |
| B vs A (construction alone, rank model) | −0.45 | [−0.76, −0.13] — excludes zero | 1% |
| A vs frozen record (re-train drift) | −0.11 | [−0.56, +0.28] | 33% |

**Reading.**

- The re-trained rank/balanced book (A) reproduces the frozen record within
  noise (−0.11, interval spans zero), so every cell shares a comparable
  baseline; the residual gap is matrix revision since 9 August and nine-fold
  sampling, not a change in the recipe.
- **The absolute label did what it was supposed to do** — the model's sign
  now carries market direction, and cell D's book really moves with it:
  realized net median +0.23, p10/p90 −0.69/+0.77, against the rank book's
  ±0.05. This was a fair test of "buy the best of the ranked list, short-heavy
  in a falling market, long-heavy in a rising one".
- **And it is worse, decisively.** D vs A: −1.16 Sharpe, interval
  [−2.03, −0.36]; 6 of 9 folds positive; mean max drawdown 2.4× A's. The
  timing the model learned is long-biased (bull-market training) and wrong
  when it matters: fold 2 (2021-05..2022-01) ran net +0.62 median into the
  top and drew down 56%; fold 3 (2022 H1) net +0.39 median through the bear,
  Sharpe −0.77. Where D wins (folds 1, 5, 6) it is long in rallies. That is
  market beta with a noisy timer, which is what the literature says 24-hour
  crypto direction is (`docs/scalper-research-round-2026-08.md` §4).
- **The label hurts even hedged** (C vs A −0.64; fold 7 −1.21): removing the
  demeaning teaches the trees the market component at the expense of the
  cross-sectional one, and the balanced constructor then throws the market
  component away — the worst of both.
- The construction effect alone (B vs A, −0.45, excludes zero) is the result
  already recorded in `unbalanced-constructor.md`, confirmed on fresh models.

**Decision (per the rule written above).** D does not beat A; it loses on
both criteria — Sharpe interval and 2022 drawdown. **The frozen config
stands: rank label, `risk_adjusted` (balanced), daily cadence, with
`max_net_exposure = 0.10` as the drift guard around a book that is, by
construction and by evidence, meant to be flat.** The label axis the freeze
note listed as untested is now tested: an absolute label is worse, with and
without the balance. `per_risk_abs` and `risk_adjusted_unbalanced` stay in
the code as named, measured alternatives; no re-tuning of either follows from
this note, and the question is closed unless live evidence reopens it.

**What this does say about the user's intent.** The intuition — don't hedge
a strong view flat — is sound in a world where the model's view of market
direction is worth having. On this record, with these features at a 24-hour
horizon, it isn't: the model's cross-sectional ranking is the asset, and the
balance is what protects it from the model's timing.

## Addendum (2026-08-19, pre-registered before training): signed magnitude rank — cells E / F

**Why.** The user's intent is directional *selection*, not a directional
*drift*: on a day when 20 names fall and 10 rise, the list should be 20
short / 10 long, with magnitudes comparable across sides. The median-centred
rank (A/B) cannot express that (half the list is "long" by construction);
the absolute label (C/D) expresses it but carries the bull-market drift and
learned it. A label in between: **rank |ret/vol| jointly across all names of
the day onto (0, 1], then re-attach the real sign.** The biggest move of the
day is ±1, a mid-sized move ≈ ±0.5, a flat name ≈ 0; ordering within a side
is preserved; long/short count follows the day's real signs; no raw drift
magnitude survives (the only market information is the sign mix, whose
half-year mean ranges 44–53% up in this matrix).

**What is run.** `TRAIN_RANK=signed --reward per_risk_abs` (sign = actual
direction), otherwise the frozen recipe; the same nine folds from the same
matrix; priced balanced (**E**) and unbalanced (**F**). Same reports, same
bootstrap comparisons (E vs A, F vs A, F vs E), same decision rule: the
frozen config changes only if F beats A with a 90% interval excluding zero
and no worse 2022 drawdown. Expectation written now: F's net will move with
the market, far less violently than D's; its Sharpe most likely lands
between B and A.

### E / F results (same day, run as registered)

| cell | mean Sharpe | compounded | folds + | mean maxDD | worst fold DD | realized net p5 / p50 / p95 |
|---|---:|---:|---:|---:|---:|---|
| E signed-rank × balanced | 1.49 | +501% | 8/9 | −16.9% | −47.2% | −0.25 / 0.00 / +0.07 |
| F signed-rank × unbalanced | 1.18 | +1106% | 7/9 | −26.7% | −52.7% | −0.54 / +0.36 / +0.77 |

Per-fold Sharpe E: `1.66 1.44 2.58 1.56 1.22 1.85 2.27 1.81 −0.94`;
F: `3.99 1.31 −1.33 1.71 1.51 0.99 2.39 −0.69 0.72`. F per-fold return vs A:
+290/+56, +51/+58, **−43/+39 (2022 H1)**, +43/+11, +24/+18, +23/+7,
+81/+46, **−21/+42**, +14/+13. F realized net median by fold: +0.49, +0.57,
+0.56, +0.30, +0.31, +0.31, +0.27, +0.17, +0.10 — long-biased throughout,
most in 2020–22.

Bootstrap: **F vs A −0.86 [−1.68, −0.07]** (excludes zero); E vs A −0.83
[−1.58, +0.04]; F vs E −0.04 [−1.14, +0.92].

**Reading.** The signed-magnitude label did what it was designed to do — the
list is genuinely directional and less violent than D's — and it is still
long-biased, because the drift enters through the sign mix and the larger
magnitudes of up-moves in the training windows; fold 3 ran net +0.56 into
the 2022 bear and lost 43%. F's compounded return is the highest in the
table and that is the trap: it is a long-crypto tilt on top of the ranker,
earned in rallies, with 2.3× the drawdowns and a −53% fold — it would breach
the capital plan's live kill criterion (DD > 25%) in its first bear market.
The label also costs the ranker skill even when hedged flat (E vs A −0.83).

**Decision (per the rule).** F does not beat A; it loses on both criteria.
The frozen config stands. Directional selection has now been tested three
ways on the same folds — float the ranked list (B), absolute label (D),
signed-magnitude label (F) — and the balanced ranker wins each time on
risk-adjusted terms: every route to direction imports the market's drift,
and the ranker's edge is the cross-section. Closed on evidence; reopened
only by a pre-registered market-direction component with its own target,
not by another label for the ranker.

### Does F's tilt anticipate anything? (measured, same day)

Correlation of F's realized net exposure on day *t* with BTC's return on
*t+1*: **0.013** over 2,142 rebalances (per fold −0.03 … +0.04). Before
next-day BTC moves < −3% (n=224) the book's net median was +0.41, net-short
18% of the time; before moves > +3% (n=259): +0.46 / 17% — indistinguishable.
Inside regimes where BTC was already down > 20% over 30 days (n=158): net
median **+0.47** (rallies: +0.37). Monthly, Nov-2021..Jun-2022, F's median
net against BTC's month: +0.58/−6.5%, +0.61/−19%, +0.62/−19%, +0.46/+12%,
+0.48/+3%, +0.61/−19%, +0.54/−17%, +0.51/−33%. The tilt is the training
base rate plus noise; it never sees the bear coming and does not react once
it is there. The ranker's cross-sectional skill is real; its timing skill is
nil under any of the three labels. The balanced book is right not because
direction is undesirable but because nothing in this model can see it.

### What F and E were getting wrong (decomposition, same day)

Daily return = net × market + residual. A: residual sd 114 bp, Sharpe 1.98,
worst day −8.6%. F: net×market part sd 159 bp, residual sd 298 bp (Sharpe
0.59), worst days −22% (2021-05-19, net ≈ +0.7, long HOT/UNI/ETC/TRX at
5–6% each into a −13% BTC day), −12%/−11% (2022-05-09/11, Terra week, long
LTC/BNB/AVAX/ROSE then BTC/XMR/BNB/LTC), −11% (2022-01-21, long YFI/OMG/
ETH/XRP). Regression beta to BTC ≈ 0 over the whole sample only because the
tilt is persistently long regardless of direction: it takes every crash and
earns no premium on ordinary days. E (balanced, same label): worst day
−38.8% on 2026-07-24 — a DEXE short at the 0.25 `max_position` cap,
squeezed; when a label thins one side, the balanced constructor puts 0.40 of
gross into it and a single name reaches a quarter of NAV. Two causes, then:
timing (long into every major sell-off in the sample) and concentration
(a one-sided list plus a 25% per-name cap). A has neither because the
median-centred rank keeps both sides ~12/12. Flag for the frozen book:
`max_position = 0.25` is generous if a side ever thins; it does not bind in
A's replay but belongs on the open-items list next to collapsed-asset
shorts.


### The spec, literally: top-30 by |score|, weights ∝ score (same day)

`top30_by_score` on the rank models (B-style) and on the signed models
(F-style): rank → Sharpe 1.12, 7/9, worst DD −43%, vs A −1.02 [−1.47, −0.51];
signed → 1.27, 8/9, compounded +1907% (the highest of all), worst DD −49%,
net > +0.2 on 55% of days, 2022 H1 fold −0.60. The literal spec reproduces
the two findings it was run to check: with the rank score the list is not
one-sided (|net| > 0.4 on 0.9% of days); with the signed score it is
one-sided and long — the same long tilt into every sell-off, plus heavier
concentration from sizing by raw score. Nothing about 24-vs-30 or ÷vol
changed the conclusion.
