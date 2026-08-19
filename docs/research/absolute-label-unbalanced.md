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

## Results

*(to be filled from the runs; nothing below this line exists at the time of
pre-registration)*
