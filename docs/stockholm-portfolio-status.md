# Stockholm portfolio bot: research and implementation status

> **Last updated:** 2026-08-13  
> **Decision:** **FAILED / NOT PROMOTABLE**  
> **Required net Sharpe:** 2.0 after realistic costs (superseded as the
> promotion gate by the active-t-stat/Sharpe-floor rule below, pending the
> user's Decision Point 1 ruling)  
> **Best corrected development Sharpe:** 1.16 ± 0.60 SE, excess of a 2%
> risk-free rate (was 1.27 non-excess, was 1.52 before the 2026-08-13
> phase-combination fix)  
> **Latest 2025-10–2026-07 diagnostic Sharpe:** -0.47 ± 1.22 SE, excess of a 2%
> risk-free rate (was -0.35 non-excess, was -0.89)  
> **Latest comparable OMXSGI Sharpe:** 0.95 ± 0.60 SE, excess, measured daily
> on the bot's own session grid (was 0.79 ± 0.58 on holding-period frequency,
> was 1.25 non-excess, was 2.38)  
> **Latest active-return t-stat vs OMXSGI:** **1.08** on the lean development
> aggregate, **-1.93** on the recent diagnostic — the promotion gate needs 2.0  
> **Direction-model results:** **all void** — every one was measured with a
> label that credited the untradable overnight gap (see the Task 4 section)  
> **Direction forecasting:** **retired** pending fundamentally new information
> — structurally unlearnable at ~250 independent 20-session outcomes, and a
> score-normalization defect independently suppressed any signal it might
> have had (see the Task 8 section); removed from every promotable
> configuration, research-only behind an explicit diagnostic flag  
> **Data status:** `SURVIVORSHIP_CONTAMINATED`  
> **Execution status:** no Stockholm paper or live orders; no deployment

This is the consolidated status of the Stockholm portfolio project. It records
the material datasets, feature contracts, responses, model families, horizons,
portfolio constructors, direction models, cost models, and implementation work
tested so far. Exact experiment history and fold tables remain in
[`stockholm-reward-loss-study.md`](stockholm-reward-loss-study.md); the intended
production contract is in
[`stockholm-portfolio-design.md`](stockholm-portfolio-design.md).

The short version is uncomfortable but unambiguous: the available data contains
some cross-sectional stock-selection signal, but it has not remained stable
through the latest regime and no tested daily-EOD market-direction model has
worked. The best corrected historical result does not meet Sharpe 2.0, and the
most recent interval loses money while the Stockholm index rises. Adding more
variants to the same closed folds would now optimize the backtest rather than
produce credible evidence.

## 2026-08-13 corrected evaluation: phases combine on the calendar

Every twenty-phase number this document reported before today was produced by
averaging the staggered books **by period index**: phase 0's period 3 was averaged with
phase 1's period 3 even though those periods cover different sessions. That is a
moving average of overlapping windows. It suppresses volatility while preserving
the mean, and the annualisation then treated the smoothed series as independent
holding periods. The measured consequence on lean development fold 3 is
unambiguous: the combined Sharpe was **4.56 while no single phase exceeded
3.74** — an aggregate that beat every one of its own parts.

Phases are now combined on the session date from each phase's daily NAV marks
(`combination_method: "calendar_aligned_daily_nav"`), and performance is
measured on daily returns annualised over 252 sessions. Regenerated with the
same matrices, models, folds and costs as the frozen runs — 20 phases per fold,
`--bars-root` supplying the adjusted daily closes:

| interval / model | return | Sharpe | max drawdown | previously |
|---|---:|---:|---:|---|
| 2022-09–2025-09, lean 20-phase (4 folds) | +83.77% | **1.27** | -21.67% | +78.49%, 1.52, -11.95% |
| 2025-10–2026-07, rich v11 20-phase diagnostic | **-4.62%** | **-0.35** | -12.99% | -6.71%, -0.89, -8.78% |

Lean fold Sharpes are now 2.21, 0.53, **2.13**, 0.51 (were 2.28, 0.71, 4.56,
0.49). Fold 3's 4.56 was almost entirely the smoothing artefact. The combined
result no longer exceeds its best phase in any fold. Annualised volatility rose
across the board (lean aggregate 13.3% → 18.8%; fold 3 7.1% → 15.5%) because the
daily marks price the intra-period path the holding-period returns hid, and the
drawdowns deepened for the same reason. Total returns rose slightly because
equal capital is now allocated once at the first commonly invested session and
each phase then compounds, instead of compounding an average of phase returns.

The OMXSGI comparison is also corrected: it was averaged across phases the same
way and reported **2.38** for 2025-10–2026-07, when the index's own unsmoothed
20-session series over that window gives **1.25** (lean development window:
0.92, not 1.20). Every phase holds the identical index continuously, so no
averaging across phases is meaningful there; the summary now reports the
lowest-offset phase's own series and labels it
`benchmark_combination_method: "single_phase_index_path"`. The benchmark is
still measured at holding-period frequency while the portfolio is measured
daily — the two Sharpes are not yet frequency-comparable, and each performance
block carries an explicit `periods_per_year` saying so.

**None of this changes the verdict.** The corrected development Sharpe is
further from the 2.0 gate than the number it replaces, the recent diagnostic
still loses money, and the index still wins. The corrected numbers are recorded
in `var/stockholm-remediation/task2/` in the remediation worktree; the frozen
originals are unchanged. Older phase reports carrying no daily marks still
summarise, but the summary stamps
`combination_method: "legacy_period_index_average"` and the CLI warns that the
Sharpe is overstated.

## 2026-08-13 Task 3: excess-return Sharpe, standard errors, and an honest gate

Every Sharpe this document reported before today assumed a 0% risk-free rate
and carried no uncertainty: a 9-period fold Sharpe and a 160-period fold
Sharpe were compared as if equally precise, when the analytic standard error
of a 9-period Sharpe is roughly ±1.1 — wide enough to have driven accept/reject
calls across the ~85 arms this project has tried. `stockholm-portfolio` now
subtracts a Riksbank-policy-rate approximation (`--risk-free-annual`, default
2%, until a SWESTR series is wired) before computing Sharpe, reports an
analytic Lo (2002) standard error (`sharpe_se`) annualised the same way the
point estimate is, and reports an active-return t-stat (mean per-period bot
minus benchmark return, over its own standard error) wherever the bot and
benchmark share one observation grid. The `passed` field on a walk-forward
fold summary is now `active_tstat >= 2.0 AND sharpe - 1.64*sharpe_se >=
target_sharpe_floor`, with `target_sharpe_floor` a new explicit, provisional
default of 1.0 pending the user's ruling on Decision Point 1 in the
remediation plan; the old `target_sharpe` (2.0) field is retained for its
original meaning but no longer gates `passed`.

Regenerated from Task 2's already-regenerated phase replays (`--bars-root`,
same matrices/models/folds/costs):

| interval / model | return | Sharpe (excess, rf 2%) | ± SE | max drawdown | previously (non-excess) |
|---|---:|---:|---:|---:|---:|
| 2022-09–2025-09, lean 20-phase (4 folds) | +83.77% | **1.16** | ±0.60 | -21.67% | 1.27 |
| 2025-10–2026-07, rich v11 20-phase diagnostic | -4.62% | **-0.47** | ±1.22 | -12.99% | -0.35 |
| OMXSGI, lean development window | — | 0.79 | ±0.58 | — | 0.92 |
| OMXSGI, pseudo-holdout window | — | 1.11 | ±1.15 | — | 1.25 |

`sharpe - 1.64*sharpe_se` (the new gate's lower bound) is **0.18** for the
lean development aggregate and **-2.47** for the recent diagnostic — both well
short of even the provisional 1.0 floor, before the active-t-stat condition is
considered at all.

**The active-return t-stat was not yet computable for either headline number
when this section was written.** Task 4 below delivered the daily benchmark
mark and both t-stats; the paragraph is kept as the record of why the Task 3
artifacts carry `null`.
Both are `calendar_aligned_daily_nav` combined reports: the bot series is
daily (252 obs/yr) while the benchmark stays on holding-period frequency
(`single_phase_index_path`, 12.6 obs/yr) until Task 4 delivers a daily
benchmark mark, so the two series cannot be paired into one t-stat yet. Both
new reports carry `active_tstat: null` and
`active_tstat_status: "unavailable_mixed_frequencies_pending_task4"` rather
than a wrong number, and `passed` is `false` on that basis alone regardless of
the Sharpe-floor comparison above. `training/summarize_stockholm.py`'s fold
stitching (a separate pipeline, on one period grid on both sides) computes a
real `active_tstat` — verified against a hand-computed value in
`training/test_summarize_stockholm.py` — but has not been run against a
frozen headline fold set as part of this task.

The corrected-and-excess numbers are recorded in
`var/stockholm-remediation/task3/` in the remediation worktree; Task 2's
non-excess corrected numbers and the frozen originals are unchanged.

**None of this changes the verdict.** Subtracting a risk-free rate and adding
standard errors only makes the existing failure more precise: the lean
aggregate's Sharpe lower bound is close to zero, not two, and the recent
diagnostic's is solidly negative.

## 2026-08-13 Task 4: both legs priced at tradable closes

The OMXSGI archive's `start_value` is not an opening-auction level. It is the
prior session's close plus any dividend adjustment: 2,092 of 2,535 session
transitions have `start_value` *exactly* equal to the previous `end_value`, and
the 443 that differ do so by a median 0.05 bp, concentrated in the March–May
dividend season. Two things were priced off it, and both are fixed:

**1. The direction-model label is void for every result this document reports.**
The label was `SOD(t+1+h) / SOD(t+1) - 1`, which — given what SOD actually is —
is `close(t) / close(t+h)`, i.e. it began at the *decision* close and therefore
credited the model with the entire overnight gap into the first session it
could trade. A model deciding at Monday's close cannot own the Tuesday-open
gap. The label is now `EOD(t+1+h) / EOD(t+1) - 1`: the first tradable session's
close to the exit close, deliberately forfeiting one full session of gap in
exchange for a number a replay can act on. Its `label_version` moves from
`omxsgi-forward-start-value-{h}-v1` to `omxsgi-forward-close-{h}-v4`, and the
replay refuses any matrix or model still carrying the old contract rather than
mixing conventions.

Every direction number in "Direction models tested" below, and every
direction-overlay and market-forecast-composition result anywhere in this
document, was produced under the old label. **They are void, not merely
imprecise**, and their bias has a known sign: overnight drift is the larger
half of equity returns, so the old label systematically flattered any model
that could predict the *market's* next-session direction at all. None of them
may be cited or compared against a v4 number, and none may be re-used as
evidence for or against the direction layer. They have not been regenerated —
Task 8 in the remediation plan decides the direction layer's fate.

**2. The replay's benchmark leg is now on the portfolio's own daily grid.**
`benchmark_period_return` was `SOD(entry) → SOD(entry+h)` while the portfolio
was open-to-open with daily NAV marks at session closes. It is now
`EOD(decision session) → EOD(exit session)` — the same calendar span the daily
NAV marks cover — and each replay step also records the index's closing level
on every session in between (`benchmark_daily_marks`). A combined phase report
therefore compares bot daily NAV against benchmark daily close on one shared
session grid (`benchmark_combination_method: calendar_aligned_daily_index_close`),
which finally makes the active-return t-stat computable; it was `null` /
`unavailable_mixed_frequencies_pending_task4` in the Task 3 numbers above.

On the 2022-09–2025-09 replay window every `start_value` equals the prior
close exactly, so the **period** benchmark returns are numerically unchanged;
what changes is the frequency the benchmark is measured and annualised at, and
the window it covers (the combined window, not the lowest-offset phase's own).

Regenerated with the new benchmark path (`var/stockholm-remediation/task4/`,
`regenerate.sh` beside it; same matrices, models, folds and costs):

| interval | bot Sharpe (excess, rf 2%) | OMXSGI Sharpe, daily grid | active t-stat | previously |
|---|---:|---:|---:|---|
| 2022-09–2025-09, lean 20-phase (4 folds) | 1.16 ± 0.60 | **0.95 ± 0.60** | **1.08** | index 0.79 at 12.6/yr, t-stat unavailable |
| 2025-10–2026-07, rich v11 20-phase diagnostic | -0.47 ± 1.22 | **0.82 ± 1.22** | **-1.93** | index 1.11 at 12.6/yr, t-stat unavailable |

Per lean fold, active t-stat: 0.60, -0.07, 1.56, 0.11. The bot's own return,
Sharpe, standard error and drawdown are unchanged from the Task 3 table — only
the index leg moved — and `passed` remains `false`: the aggregate active t-stat
of 1.08 is barely half the 2.0 the gate requires, and the recent diagnostic is
significantly *negative* against the index.

**This is the first honest bot-versus-index comparison in the project.** Both
legs are now marked at closes, on identical dates, at the same annualisation,
excess of the same risk-free rate. It says the lean development book's
outperformance is not statistically distinguishable from zero, and that the
recent book underperforms the index at roughly 2 sigma.

## 2026-08-13 Task 8: trained direction forecasting retired, score-scale defect fixed

Task 8 was the "decides the direction layer's fate" step promised in the Task
4 section above. **The decision is retirement.** Trained market-direction
forecasting is removed from every promotable Stockholm configuration.

**Why.** Independent of the void v1 label, the trained direction model (any
tested response, any tested feature set) never produced a validated forecast:
best case 22.2% directional accuracy and −0.037 forecast correlation on the
Stockholm-close v3 arm, every arm losing to both a fixed five-vote trend
control and buy-and-hold OMXSGI (see "Direction models tested" above; those
specific numbers are void under the v1 label, but the qualitative verdict —
every tested variant lost to controls — was consistent across all of them,
old label and new). Ten years of daily EOD data supplies roughly 250
independent 20-session market outcomes. That sample size cannot support a
high-capacity model learning a market-timing signal, regardless of feature
set or reward. This is the "no validated direction signal, structurally
unlearnable at this sample size" audit finding. Retraining on the corrected
v4 label would not change this: the constraint is sample size, not label
timing.

**A separate, compounding defect: broken score normalization.** Independent
of whether the direction model had any skill, its scores could never have
reached the policy's decision thresholds. `train_stockholm_direction.py`
exported `score_scale = std(target)` — the label's own spread, roughly 0.05
for the 20-session absolute return. Rust computes
`score = clip(prediction / score_scale, −1, 1)` and compares it against
`DirectionConfig` thresholds `enter_threshold=0.4` / `strong_threshold=0.8`
(`portfolio-construction/src/lib.rs`). But `PARAMS` fits a deliberately
shallow, heavily L2-regularized (`lambda_l2=25`) tree ensemble, so its
predictions are shrunk to a small fraction of the target's spread — in the
regression test added for this task, predictions with std 0.005 against a
target std 0.05 produced scores with population std ≈ 0.1 under the old
convention, an order of magnitude short of even the 0.4 entry threshold. The
book would have parked at `neutral_gross = 0.30 × max_gross`, `net = 0`
almost every session, regardless of what the model actually believed.

**The fix.** For the `absolute_return` reward — the one this defect affects —
`score_scale` is now the model's own in-sample prediction spread —
`std(booster.predict(x))` on the training rows — instead of the target's.
Rust's formula is unchanged (`score = clip(prediction / score_scale, −1, 1)`);
only the scale Python hands it changed. With the fix, the same 0.005-std
predictions against a 0.05-std target now produce scores with population std
≈ 1 (near-full threshold range), confirmed by a Rust unit test at the
score-computation boundary
(`stockholm-portfolio::tests::score_scale_from_prediction_spread_spans_threshold_range`)
and a Python test asserting the exported `score_scale` equals
`std(booster.predict(x))` on the training design matrix, not `std(target)`
(`training/test_train_stockholm_direction.py`). This fix does not rescue the
direction model — it corrects a normalization bug that was independently
suppressing any signal the model might have had, on top of a model that has
no validated signal to suppress.

`score_scale` means something different for the `direction_sign` reward:
there, Rust's raw (already bounded) prediction *is* the score directly, and
`score_scale` only converts it into a return-scale `predicted_return`
(`predicted_return = clamp(prediction, −1, 1) * score_scale`). Nothing
divides the score by it, so that branch was never affected by the
score-suppression defect and correctly remains `std(absolute_y)` — the
trainer's `score_scale` export is now reward-conditional, and a second
Python test pins the `direction_sign` branch to its unchanged formula
(`test_direction_sign_score_scale_is_target_std_not_prediction_spread`).
Because the `absolute_return` semantics changed, `model_version` bumped
`stockholm-direction-model-1` → `-2`; Rust's `DirectionModel::load` refuses
any document still carrying the old version, so a pre-fix model.json cannot
be loaded and silently reproduce the defect
(`stockholm-portfolio::tests::stale_v1_direction_model_version_is_refused`).

**Disposition.** `stockholm-portfolio backtest` (the only promotable replay
path) now refuses `--market-forecast-matrix`/`--market-forecast-model`
composition unless the caller also passes
`--trained-direction-diagnostic`, an explicit, loud opt-in that prints a
retirement warning to stderr. No promotable configuration passes that flag;
it exists only so a future research/diagnostics replay can still ask for the
trained model deliberately. `training/walk_forward_stockholm.py` — itself a
research harness, not a promotable path (its output is stamped
`SURVIVORSHIP_CONTAMINATED` and explicitly may not authorize capital) —
supplies the flag automatically when a caller asks it to fit a market
forecast model, since that whole script is already diagnostics by
construction. The fixed five-vote OMX trend state (`market_trend`,
`fixed_direction_score`) is unaffected: it remains available behind
`--direction-overlay`, off by default, as an optional drawdown-guard sizing
input, not a return-seeking forecast. It is not a trained model and carries
none of the sample-size or score-scale defects above; whether it belongs in
a promoted book is Task 13–15's overlay-budget-mode work, not this one.

Every direction-model result in this document, under either label, remains
what it always was: research history, not evidence for a promotable
direction layer. This section does not reopen or rehabilitate any of those
numbers; it records why the direction *model* is retired and confirms the
one implementation defect (score normalization) that was independently
capable of hiding whatever signal, if any, a differently-scaled version might
have shown.

## 2026-08-13 Task 15: overlay book widened; Phase 2 control replay BLOCKED

**Step 1 (done).** `stockholm-portfolio backtest --allocation-mode overlay`
now defaults `--max-positions` to 40 **per sleeve** (long and short are
ranked and admitted against separate 40-name caps, so up to 80 names total),
`--position-weight` to 0.015, and `--max-gross` (the overlay's gross budget)
to 0.6 — 100% core plus up to 30% long / 30% short overlay at the same total
gross as before. Directional mode's defaults (20 combined names, 0.05,
1.0) are unchanged; any explicit flag, in either mode, overrides its
default. Rationale, from the earlier overlay audit: the 20-name overlay
book's phase-to-phase dispersion (−21%..+6%) was uncompensated concentration
noise; more names at smaller size shrinks it roughly √2–√3 while spending the
same gross.

Candidate admission was also fixed to match: `max_positions` previously fed
one combined ranking across both directions even in overlay mode (via
`buffered_ranked_ids`), so a run of one-sided edges could fill the whole cap
from one sleeve and admit zero names on the other — the un-hedged outcome
the widened book is supposed to avoid. `backtest()` now ranks and admits the
long and short pools independently in overlay mode, each against its own
cap; directional mode's historical combined-cap behaviour (no side quota) is
unchanged.

**Step 2 (BLOCKED).** The brief asked for the frozen lean model
(`baseline-membership-prenorm-v2/model-{1..4}.json`, the same artifacts
Task 2/3 replayed) to be replayed in overlay mode, unrefit, as the Phase 2
constructor control. The fold-1 smoke test specified before the full sweep
caught the problem immediately: `Model::load` refuses the model outright —

```
unsupported Stockholm feature version "fs-rust-stockholm-1"
```

Task 5 (`673219b`) deliberately moved every reachable stock
`feature_set_version` constant past the pre-membership-fix block (old
version `n` → `n + 16`) specifically so a model or matrix built with the
latent cross-section look-ahead can never again be loaded and silently
replayed as if unaffected. That is a correct and deliberate safety rail, but
its consequence is total: every stock model file under `var/stockholm/`
predates the bump (checked all `model.json`/`model-*.json` files present —
every one declares `fs-rust-stockholm-1` through `-16`, or a `-direction-*`
version; none matches a currently-registered constant). The exact
directional-mode command from `var/stockholm-remediation/task2/regenerate.sh`
— no overlay flags at all — fails identically on this same worktree at the
current HEAD, confirming the block is general, not overlay-specific, and not
a consequence of the Step 1 change above (verified with only Step 1's two
files staged). No currently existing frozen model can be replayed fresh by
`stockholm-portfolio backtest` under today's binary; the earlier
Task 2/3/4 numbers in this document survive only because they were produced
before Task 5 landed and are read back as saved JSON, never reloaded through
`Model::load`.

Fitting a new lean model is out of scope here twice over — it would be a
refit, which the brief explicitly forbids, and model fitting is a Python job
per this project's Rust/Python split. Loosening `Model::load`'s version
check to let this one replay through is also out of scope: that check is a
cross-cutting safety rail Task 5 debated and hardened across two review
rounds, and weakening it is a controller-level call, not something to bundle
into a CLI-default change. No overlay-vs-directional numbers for the Phase 2
constructor control are reported below; the overlay mechanism itself is
verified only by the unit test added in Step 1
(`overlay_mode_admits_candidates_per_sleeve_not_from_one_combined_ranking`).
Unblocking Step 2 needs one of: a fresh model fit on the corrected feature
pipeline (a Phase-1/controller decision, not "no refit" in the sense the
brief meant it), or an explicit, disclosed, narrowly-scoped exception for
read-only comparison replays that does not weaken the check for any
promotion-relevant path.

## Current verdict

The strongest corrected development result is the lean LightGBM stock model
with twenty staggered 20-session books:

| interval / model | return | Sharpe | max drawdown | OMXSGI return | OMXSGI Sharpe |
|---|---:|---:|---:|---:|---:|
| 2022-09–2025-09, lean 20-phase | +83.77% | **1.27** | -21.67% | +49.44% | 0.92 |
| 2025-10–2026-07, rich v11 20-phase diagnostic | **-4.62%** | **-0.35** | -12.99% | +14.05% | **1.25** |

All four lean development folds made money, but their Sharpes were 2.21, 0.53,
2.13, and 0.51. Three folds fail the 2.0 threshold, the aggregate fails it, and
the last fold was already weak before the recent sign reversal. This is not a
small benchmark gap hidden by costs: recent predicted top-decile stocks lost
while lower-ranked stocks did better, so the ordering itself reversed.

The best rich corrected model has a different but equally non-promotable
profile. Across the four development folds it returned +97.12% at Sharpe 1.21
and -11.14% drawdown; at doubled spread and impact it returned +64.69% at
Sharpe 0.96. Fold Sharpes were 1.66, 0.93, 3.76, and 0.38. Its twenty-phase
recent diagnostic is the -4.62% / -0.35 result above.

Those rich development figures, and every other twenty-phase Sharpe further
down this document, were **not** regenerated on 2026-08-13 and still carry the
period-index smoothing described above. Read every one of them as inflated in
magnitude: the correction raises measured volatility, so it shrinks a Sharpe
toward zero from whichever side it sits — lean aggregate 1.52 → 1.27, lean fold
3 4.56 → 2.13, recent diagnostic -0.89 → -0.35. Fold 3's 3.76 in the rich
series covers the same calendar window as the lean 4.56 and is the most likely
to be largely artefact.

These numbers are research diagnostics, not investable estimates. Every
current stock matrix projects a present/prior-snapshot survivor universe back
through history and is explicitly stamped `SURVIVORSHIP_CONTAMINATED`. The
2025-10–2026-07 interval is a pseudo-holdout because prior experiments have
already exposed it.

## What exists in the codebase

The requested architecture is in place:

- `equity-data` owns reusable Nasdaq, FI, Skatteverket, Riksbank, ESEF, EODHD,
  and report data providers and archives;
- `cme-data` owns reusable global-futures history, Stockholm-close
  availability, CET/CEST conversion, and NQ-to-MNQ archive stitching;
- `ib` owns shared IB market-data and broker functionality, including
  historical borrow-fee collection;
- `features-stockholm` owns causal Rust feature, label, normalization,
  universe-admission, and factor calculations;
- `portfolio-construction` owns shared gross/net budget decomposition,
  hysteresis, volatility ceilings, scale-down-only allocation, caps, cash
  residuals, and diagnostics;
- `stockholm-portfolio` owns Stockholm-specific orchestration and commands;
- Python is limited to training orchestration and model fitting. Final feature,
  label, cost, and portfolio semantics remain in Rust.

The bot is directional by design. It does not force 50% long / 50% short.
Gross and net targets become maximum sleeve budgets using
`long=(gross+net)/2` and `short=(gross-net)/2`. Only eligible positions with
sufficient predicted edge may consume a sleeve; unused capacity remains cash
and is never scaled up merely to meet a quota.

The frontend already creates one bot tab per registered bot from the API. A
separate Stockholm tab therefore needs no hard-coded frontend fork; it will
appear once a Stockholm bot is registered. It has deliberately not been
registered or deployed because no model passed the research gate.

Shared IB Gateway connectivity and the paper account were validated during the
integration work. Environment names are the broker-neutral
`IB_GATEWAY_HOST`/`IB_GATEWAY_PORT`, with credentials supplied to the Gateway
from secrets/environment rather than embedded in bot code. That proves the
transport path, not the model. No Stockholm order has been sent.

The latest causal global-risk feature work is committed in `260bf7a`; it is an
ancestor of the current main branch. That work includes the Stockholm-close
global-futures loader, NQ/MNQ stitching, v16 stock inputs, v3 direction inputs,
and the residual-factor membership correction. At completion it passed 63 Rust
tests, 12 Python tests, formatting, and Clippy. Later unrelated main-branch
commits do not change the Stockholm verdict.

## Data collected and tested

### Price, index, membership, and liquidity data

| source | collected coverage | use and limitation |
|---|---|---|
| Yahoo adjusted daily prices | approximately 2016–2026 for current/prior-snapshot names | adjusted stock OHLC used by research; inactive old securities are missing |
| Nasdaq Nordic market history | 878,723 valid sessions for 411 of 412 lines; 864,294 with two-sided closing quotes | official OHLC, bid/ask, turnover, trade count; endpoint stops resolving old delistings |
| Nasdaq gross-return indexes | OMXSGI, OMXS30GI, OMXSBGI from 2008/2011 plus eight sector indexes | benchmark and market/sector context |
| Skatteverket listings | 4,477 rows, 1,648 companies, 955 conservatively Main Market, 150 delistings | membership evidence; insufficient by itself for point-in-time segment reconstruction |
| Nasdaq equity notices | 1,071 Stockholm notices; 189 of 190 delisting notices have an ISIN | official corporate-event evidence; not inactive price history |
| EODHD inactive-price path | provider implemented and tested | not collected because `EODHD_API_TOKEN` is absent |

The target universe is Nasdaq Stockholm Main Market Large, Mid, and Small Cap.
First North is excluded in Rust and covered by a mixed-universe test. The old
First North exploratory result is retained only as provenance and is
superseded.

Nasdaq, IB, and Yahoo were directly tested for inactive Swedish Match, Acando,
and OX2 contracts. Nasdaq reported the instruments unavailable, IB failed
contract resolution with error 200, and Yahoo returned 404. The free sources
therefore do not solve delisted-price history. A prospective snapshot did save
Nilörngruppen's still-resolvable history during its short post-delisting grace
period, which confirms snapshots help but cannot reconstruct the past.

### Corporate, fundamental, macro, and borrow data

| source | collected coverage | use and limitation |
|---|---|---|
| FI PDMR | 165,140 published rows; 397/412 current lines have history | causal insider transaction features; best consistent incremental public input, but not sufficient |
| FI public short register | 38,190 holder events across 421 ISINs | short-demand proxy, not stock-loan availability |
| Nasdaq company news | 141,032 events; 97,885 mapped events covering 364/411 securities | causal event counts/reactions; did not stabilize the latest regime |
| Nasdaq financial reports | 31,617 announcements, 31,611 HTML bodies, 43,053 attachment links, 41,077 unique PDFs | report events and exact-label accounting extraction |
| ESEF annual filings | 1,400 filings, 417 entities, 243,517 normalized IFRS facts | mostly annual and stale relative to a 20-session forecast |
| Riksbank | causal USD/SEK, EUR/SEK, and KIX histories | market context and stock-specific FX sensitivities |
| IB `FEE_RATE` | 406 series, 749,715 observations from 2016–2026; 405 names mapped | historical annualized borrow fee; neither locate status nor lendable quantity |
| EODHD quarterly fundamentals | shared provider implemented and tested | blocked by absent token; revision history still requires conservative treatment |

The attachment collector is deliberately resource-bounded after the earlier
unsafe parallel decoder terminated WSL. Downloads are separate and atomic;
each decoder runs in its own process with a 512 MiB address-space ceiling and a
120-second timeout, and extraction is sequential. The initial cache contained
3,499 PDFs, 3,432 successful texts, and 67 explicit failures. The bounded
2026-08-13 expansion requested 5,000 of 41,077 available documents: 4,999 of
the requested set were available and the resulting cache/audit contained 5,000
documents, 4,891 extracted texts, and 109 isolated failures.

That audit augmented 4,758 messages and raised deduplicated report events with
at least one exact accounting metric from 9,424 to 9,953 (+529, or 5.6%). The
incremental field counts were sales +317, order intake +44, EBIT +414,
operating margin level +129, operating margin change +132, EPS +231, and
dividend +344. This confirms that attachments fill causal fields, but it is
coverage work only. It cannot produce a v13 model because the v13 training
contract rejects a partial archive, and the modest incremental yield does not
yet justify automatically downloading and decoding all remaining PDFs.

### Global market data

The causal global-risk archive has approximately 4,096 completed ES and
NQ/MNQ sessions from 2010–2026 and 1,273 ZN / 1,266 GC sessions from
2021–2026. Rust exposes only bars completed by the 17:30 Stockholm cash close,
with historical CET/CEST conversion. Equity entries remain the following
session's open. This closes the previous timing defect where a US session not
yet known at the Stockholm decision could leak into the signal.

## Models, rewards, and losses tested

The fitting protocol is purged, expanding walk-forward. Training labels whose
20-session exits overlap a test entry are purged; dates are never shuffled.
Cross-sectional sample weights total one per decision date. LightGBM capacity
is shallow and fixed within each declared comparison. Tests also covered
weighted ridge and fixed ensembles.

### Reward and objective study

| Rust-owned response | fitting loss | development return | base Sharpe | 2x-cost Sharpe | mean rank IC | result |
|---|---|---:|---:|---:|---:|---|
| absolute return | L2 | +97.0% | 1.07 | 0.95 | +0.0381 | selected historically, failed later |
| absolute return | Huber | +97.0% | 1.07 | 0.95 | +0.0381 | economically identical, no advantage |
| absolute return | L1 | +23.9% | 0.49 | 0.31 | +0.0557 | reject |
| return per risk | L2 | +11.1% | 0.26 | 0.16 | +0.0157 | reject |
| return per risk | L1 | +21.6% | 0.47 | 0.28 | +0.0341 | reject |
| return per risk | Huber | +30.1% | 0.44 | 0.34 | +0.0013 | reject |

The selected absolute-return/L2 arm then lost 10.8% at Sharpe -0.37 in its
candidate-specific 2025-10–2026-07 block while OMXSGI gained 14.0% at Sharpe
1.25. It averaged +99% net and beta 1.10 in the development folds. In other
words, much of the attractive development return was broad long equity drift,
not a durable ability to profit in both directions.

The crypto bot's main transferable lessons were explicitly tested rather than
ignored:

- purged expanding walk-forward, time ordering, prefix-only clipping, and
  one unit of weight per decision date are implemented;
- centered return-per-risk and cross-sectional rank responses were tested on
  the decomposed selector;
- the v10 centered relative-return-per-risk arm returned +23.8% at Sharpe 0.61
  and the rank arm +7.1% at Sharpe 0.32, versus +28.9% / 0.85 for plain
  relative return;
- a dense one-session rank label produced positive development IC but no
  executable edge after Stockholm spreads and turnover.

The lesson is not that the crypto objective was implemented incorrectly. It
is that a reward well suited to a continuously traded crypto universe did not
transfer economically to this lower-frequency, spread-constrained equity
cross-section.

### Model families and training windows

| test | development result | recent result | conclusion |
|---|---|---|---|
| shallow LightGBM | best lean 20-phase Sharpe 1.27 | rich 20-phase Sharpe -0.35 | best so far, unstable and below gate |
| weighted ridge | Sharpe 1.38 in corrected lean phase-zero study | +3.2%, Sharpe 0.45 before 2x-cost deterioration | smoother but no durable ordering |
| residual LightGBM | +9.7%, Sharpe 0.34 after state correction | failed residual diagnostics | reject |
| residual ridge, lambda 25 | +2.4%, Sharpe 0.13 | no repair | reject |
| fixed 50/50 tree/ridge | +5.2%, Sharpe 0.20 | no repair | reject |
| fixed lean/rich ensemble | +82.6%, Sharpe 1.19 | -9.6%, Sharpe -0.77 | reject |
| 504-session rolling LightGBM | +66.9%, Sharpe 0.99 | -14.4%, Sharpe -0.72 | reject |
| 504-session rolling ridge | +54.0%, Sharpe 0.90 | -2.1%, Sharpe -0.17 | reject |
| five-model recent ensemble | — | -19.2%, Sharpe -2.13 | reject |
| 252-session calibration | first development fold -20.6%, Sharpe -1.46 | -0.26%, Sharpe -0.10, mostly cash | reject |

No neural or reinforcement-learning model was promoted to a serious arm.
There are too few independent market-direction samples to justify their extra
capacity, and the action problem is already represented more transparently as
a selector plus a budgeted direction layer.

## Feature contracts tested

All production-candidate inputs below are calculated and finalized in Rust.
The table reports the economic test that closed each material feature family.

| contract / feature family | result | verdict |
|---|---|---|
| baseline price, momentum, reversal, volatility, liquidity, size ranks | corrected lean 20-phase development Sharpe 1.27; recent rank IC negative | strongest development control, not promotable |
| v3 residual risk: beta, residual vol, Amihud, range/location, market/sector residual momentum | recent -9.9%, Sharpe -0.54 vs control -8.6% / -0.75 | reject |
| v4 FI public shorts | recent -7.5%, Sharpe -0.91, IC -0.0078 | reject |
| v5 FI PDMR | development +15.9%, Sharpe 0.47; recent -2.3%, Sharpe -0.10, IC -0.0264 | useful incremental data, insufficient |
| v6 financial-report events | development +26.1%, Sharpe 0.69; later -7.7%, Sharpe -0.69 | reject |
| v7 annual ESEF, relative label | +17.9%, Sharpe 0.46 | reject |
| v7 annual ESEF, absolute diagnostic | +84.1%, Sharpe 1.40; later -11.8%, Sharpe -0.66 | market-drift fit, reject |
| v8 Riksbank FX/KIX | +10.7%, Sharpe 0.35 | reject |
| v9 Nasdaq spread/trades/turnover microstructure | +26.9%, Sharpe 0.76; stress 0.61; below OMXSGI 0.92 | real use, insufficient |
| v10 IB borrow fees | best +28.9%, Sharpe 0.85; stress 0.69 | fees useful for cost truth, not enough alpha |
| v11 issuer news | original assumed-cost +33.8%, Sharpe 0.95; measured-cost replay +7.1%, Sharpe 0.32; stress -0.08 | reject |
| v12 official report HTML text | first fold -5.4%, Sharpe -0.85 vs OMXSGI +18.6% / 1.18 | stopped by predeclared first-fold gate |
| v13 report PDF completion | partial coverage only; no matrix/model allowed | open data acquisition, not tested alpha |
| v14 prior-day global-risk context | direction studies Sharpe 0.11–0.44 depending response | reject |
| v15 quarterly fundamentals | provider ready, dataset absent | blocked by EODHD token |
| v16 causal Stockholm-close ES/NQ/ZN/GC in direct stock model | first fold +13.3%, Sharpe 0.75 vs v11 +39.0% / 1.66 and OMX +18.6% / 1.18 | stopped; common forecast overwhelmed selection |

The v16 result is diagnostically important. Its rank IC improved to +0.0621,
but the model set mean net exposure to -75.6% and predicted every score decile
negative while the market rose. This proves that putting common direction
features directly into every stock score can improve relative ordering yet
destroy the portfolio through the wrong common sign. The two-layer design is
therefore still structurally correct even though its direction layer has not
worked.

## Forecast horizon, cadence, and constructor tests

| test | development | recent / stress | result |
|---|---:|---:|---|
| 5-session horizon | Sharpe 0.87 | 2x cost -0.02; recent -0.52, IC -0.0138 | reject |
| 10-session horizon | Sharpe 1.09 | 2x cost 0.39; recent -1.50, IC -0.0367 | reject |
| 20-session horizon | Sharpe 1.41 phase zero; 1.27 all phases | 2x cost 1.13 phase zero; recent negative | retained research control only |
| 1-session label and execution | IC +0.0355 | Sharpe 0.42 executable; 150–190% turnover if forced | costs consume edge |
| 1-session training, 5-session holding | +20.8%, Sharpe 0.46 | stress -24.1%, Sharpe -0.50 | reject |
| exact adjusted 12-1 momentum | -37.8%, Sharpe -0.67 | recent -11.0%, Sharpe -0.57 | reject |
| 12-1 long-only control | +42.9%, Sharpe 0.73 | OMX +49.4%, Sharpe 0.92 | below index |
| edge/vol top 24 | Sharpe 0.79 | below control | reject |
| equal top 20 by risk | Sharpe 0.86 | below control | reject |

Rank/softmax sleeve normalization was rejected as a design because it converts
a maximum budget into a quota. The implemented constructor applies edge and
tradability gates, computes preliminary weights, caps only downward, never
redistributes capped excess upward, enforces cost-derived minimum trade value,
and leaves unused sleeve capacity as cash. Per-name caps, sector caps,
retention ranks, equal/rank/vol sizing, and turnover/cost sensitivity were
tested as ablations; none closed the gap to Sharpe 2.0 or fixed the recent sign
reversal. Portfolio-level results, not IC over changed eligible universes, were
used for gate comparisons.

The original First North-inclusive exploration reported 5/10/20-session
Sharpes of roughly 0.47/0.86/0.99. It is invalid for the requested universe and
survivor contaminated; it is not evidence that adding First North helps.

Additional closed-fold controls and constructor replays were also retained,
even when later corrections superseded them:

| control | return | Sharpe | status |
|---|---:|---:|---|
| clipped rather than prefix-normalized membership baseline | +79.0% | 1.02 | superseded by corrected admission-before-normalization |
| fixed 1,008-session rolling baseline | +61.9% | 0.86 | reject |
| measured-cost absolute/L1 baseline | +34.5% | 0.70 | reject |
| measured-cost relative-rank baseline | -2.0% | -0.04 | reject |
| mandatory trend gate on relative-rank baseline, first fold | -8.8% | -1.21 | stopped; trend remains a feature, not an alpha gate |
| rank-30 position retention | +66.9% | 0.85 | reject versus control |
| 25% sector cap | +71.3% | 0.93 | useful risk control, no Sharpe solution |
| isolated selection-layer diagnostic | +81.6% | 0.98 | confirms some selection value, below target |
| older risk-rank sizing replay | +53.4% | 0.86 | superseded / reject |
| older volatility-sizing replay | +54.9% | 0.79 | superseded / reject |

The generated artifact tree also contains smoke, correction, and composition
replays rather than independent model claims. These include the original
`walk-forward`, `walk-forward-10`, `walk-forward-20`, `context-v2`,
`risk-rank`, and `sizing` runs; `reward-loss-main` and selected-candidate
copies; membership clipped/pre-normalized and 504/1,008-session rolling
variants; measured-execution absolute replays; direction-overlay,
absolute-centered direction, residual tree/ridge/hybrid, state-fix, calibration,
and composition diagnostics; and IB audit/fee/delisted/price probes. They are
not omitted evidence: their material economic conclusions are represented in
the tables above, while their duplicate directories preserve the exact stage at
which a correction or stop rule was applied.

## Direction models tested

> **Every number in this section is void.** All of it was measured with the
> retired `omxsgi-forward-start-value-*-v1` label, which began at the decision
> session's close and so credited the model with the untradable overnight gap
> into the first held session — see the 2026-08-13 Task 4 section. The results
> are kept only as a record of what was tried; none of them may be cited, and
> none may be compared against a `omxsgi-forward-close-*-v4` number.

The portfolio is intended to make money in sustained up and down markets, so
direction was tested as a separate problem rather than forcing neutrality.
None of the direction sleeves below is used in a promoted model.

| direction test | out-of-sample result | comparison / diagnosis |
|---|---|---|
| fixed OMX trend state: MA50/200, 3/6/12-month votes, vol target, hysteresis | standalone -4.8%, Sharpe -0.29 | lowered combined drawdown but cut development Sharpe 1.07 to 0.63 |
| official OMX/sector trained 20-session direction model | +5.05%, Sharpe 0.11, DD -13.6% | fixed control +8.2% / 0.13; OMX long-only +243.8% / 0.67 |
| prior-day ES global v2, absolute response | +13.55%, Sharpe 0.22 | below OMX |
| prior-day ES global v2, sign response | +26.07%, Sharpe 0.44 | best old direction response, still weak |
| prior-day ES daily direction | +7.45%, Sharpe 0.28 | weak |
| Stockholm-close ES/NQ/ZN/GC v3, 20 sessions | first fold -1.33%, Sharpe -0.40; accuracy 22.2%, correlation -0.037 | OMX +18.6%, Sharpe 1.18; stopped |
| Stockholm-close ES/NQ/ZN/GC v3, 1 session | first fold -1.90%, Sharpe -1.59; 10 positive active periods | OMX positive; fixed control -5.39%, Sharpe -1.06; stopped |

The one-session v3 diagnostic had 194 periods and 52.6% raw sign accuracy, but
the stateful strategy was mostly neutral and its active forecasts were not
profitable. Forecast correlation was effectively zero. More overlapping daily
labels therefore did not create an economically useful direction signal.

## Cost and execution realism tested

Backtests include IB percentage commission, measured Nasdaq bid/ask spread,
impact, and short holding cost. Base round-trip commission is 10 bps; observed
20-session median spread is carried outside the model feature map; missing
spreads use 20 bps; impact has a 5 bps floor. Stress doubles spread and impact,
not commission. Historical IB fee rate replaces the fixed short fee where
observed, but an availability penalty remains because a fee observation does
not prove lendable quantity.

This correction mattered. The v11 news model fell from +33.8% / Sharpe 0.95
under a uniform 35 bps assumption to +7.1% / 0.32 under measured execution.
The previous apparent gain depended on Small Cap trades whose spread made them
non-economic. The daily ranker also showed positive gross ordering and then
lost it through 150–190% daily turnover. Costs are not the sole reason for the
latest failure, but ignoring them manufactures false alpha.

## Corrections found during the audit

Two data-ordering defects were fixed and the affected models replayed:

1. Historical admission was once applied after final cross-sectional ranks,
   labels, equal-weight market return, and sample weights. It is now applied
   while each decision-date candidate set is formed, and a prefix test proves
   an ineligible future issuer cannot influence another row.
2. Rich residual features could still include pre-admission issuers in trailing
   market/sector factor returns. The same admission rule is now applied before
   factors are calculated, with a reference-history prefix test.

A direction-state bug was also fixed: after a confirmed reversal, an up/down
state could temporarily retain net exposure in the old direction while the
ramp crossed zero, and neutral could retain stale net. The shared state now
drops the invalid side immediately and ramps only new exposure.

These corrections improved some historical reports, which is why the corrected
1.27 result supersedes earlier numbers. They did not create a promotable model.

## Why the result is not better

### 1. The number of independent observations is much smaller than the matrix

The rich stock matrix has about 511,000 rows, but hundreds of stocks on the
same date share the same market outcome and overlapping 20-session labels.
They provide useful cross-sectional comparisons; they do not provide 511,000
independent forecasts of market direction. Ten years supplies only about 250
non-overlapping 20-session market outcomes. The broader official-index
direction study had 149 non-overlapping out-of-sample decisions. This is too
little independent direction data for a high-capacity model to learn a stable
two-sided market-timing rule.

### 2. The selection signal is real-looking but weak and regime-dependent

Typical out-of-sample rank IC is about +0.03 to +0.05, which is plausible for
equities. It is not large. Tail portfolios amplify errors, turnover, spread,
and borrow constraints. More importantly, several relationships reversed in
2025–2026: volatility, Amihud illiquidity, traded notional, and the final score
ordering changed sign. Rolling windows, ridge, ensembles, and calibration
reduced different symptoms but did not restore positive recent IC.

### 3. The model can rank stocks or time the market, but those are not the same

Cross-sectional normalization deliberately removes the common market level.
That helps ranking but cannot choose net direction. Direct v16 common features
raised rank IC while sending the whole portfolio heavily short in a rising
market. Separate daily and 20-session direction models then failed on their own.
The missing piece is therefore not the gross/net budget equation; it is a
validated forecast of the common market sign.

### 4. The current historical universe is biased toward survivors

Known losers, bankruptcies, acquisitions, and old delistings are absent or
incomplete. That can make historical long results look safer and short results
less representative. Official notices identify many events, but free Nasdaq,
IB, and Yahoo histories do not resolve the inactive contracts. Until inactive
prices and point-in-time Large/Mid/Small membership are joined causally, the
backtest cannot authorize capital even if its headline Sharpe improves.

### 5. Fundamentals are too sparse for the intended horizon

Annual ESEF facts are stale for a 20-session forecast and did not survive the
later diagnostic. The implemented quarterly provider cannot run without the
licensed-data token. Report text and public events improved coverage but did
not supply the missing durable earnings/quality change signal.

### 6. Historical short feasibility is only partially observed

IB provides historical fee rate, not historical locate status or lendable
quantity. A model can identify an attractive weak stock and still be unable to
borrow it at the required size. This matters most in the down-market regimes
where the design wants a large short budget. The allocator correctly leaves an
unfilled short budget in cash, but a backtest can only model historical
availability through an explicit conservative scenario unless a separate
stock-loan dataset is licensed.

### 7. Stockholm execution costs consume short-horizon signal

Observed spreads, especially in Small Cap, are large relative to daily or
five-day expected edge. The one-day experiment demonstrated this directly:
positive ordering was not enough to fund 150–190% turnover. Moving to twenty
sessions helps, but leaves fewer independent samples and slower adaptation.

### 8. The remaining folds are no longer honest tuning surfaces

Reward, loss, horizons, features, windows, ensembles, constructors, and the
recent interval have all been inspected repeatedly. Searching another set of
thresholds until one prints Sharpe 2.0 would be selection bias. The correct
response to a negative recent interval is not unlimited retries on the same
data; it is new information, a predeclared hypothesis, and genuinely new
forward evidence.

### 9. The NQ comparison is not like-for-like

The NQ bot is a deterministic intraday noise-area mechanism on 5-minute bars,
flat each day, with thousands of trades over sixteen years and gross-positive
performance in 14 of 16 years. It is not a supervised daily direction model.
Its own research ledger puts the modern four-sleeve book around Sharpe 1.0,
not 2.0; attractive CAGR also comes from the capital/sizing dial and NQ's price
path. The transferable engineering patterns have been used, but the structural
source of NQ trades does not exist automatically in daily Stockholm bars.

## Open issues, in priority order

### P0 — evidence validity

1. **Acquire inactive/delisted daily history.** Configure a licensed provider
   token and collect only ISINs corroborated by official Nasdaq delisting
   notices. Validate terminal cash/merger outcomes before joining prices.
2. **Reconstruct point-in-time Main Market Large/Mid/Small membership.** Resolve
   identifier intervals and segment changes; then rebuild ranks, factors,
   labels, and weights from the correct eligible set.
3. **Acquire causal quarterly statements and filing dates.** Run the existing
   shared provider, preserve revision limitations, and test one frozen v15
   contract rather than parser/model variants.
4. **Define historical borrow feasibility.** License locate/quantity history if
   available; otherwise predeclare a conservative availability scenario by
   size/liquidity/fee bucket and report target versus realized short budget.
5. **Decide whether to complete the report-PDF archive.** The bounded audit is
   finished and improved exact-metric event coverage by 529 events (5.6%). A
   partial cache may not train v13. Completing all 41,077 PDFs should be a
   deliberate storage/time decision and would still test only one frozen v13
   arm; it should not be assumed to solve the model failure.

### P1 — research that can still be informative

1. Preserve a genuinely untouched future interval. Log shadow scores,
   allocations, modeled costs, and benchmark returns prospectively without
   sending orders. Do not tune from each day's result.
2. After P0, predeclare one mechanism-level test using information not already
   exhausted: quarterly earnings change/post-report drift, industry-relative
   residual selection, or another economically motivated event mechanism.
3. Evaluate direction and selection separately before combining. A direction
   sleeve must beat cash/index controls on its own; a selector must preserve
   rank IC and net tail spread after measured costs.
4. Keep the user's Sharpe 2.0 gate. Report both absolute performance and OMXSGI
   comparison, all folds, cost stress, target/realized gross and net, unfilled
   budgets, turnover attribution, and borrow coverage.

### P2 — production work, intentionally blocked

1. Register the Stockholm bot and expose its frontend tab only after a candidate
   clears data validity, walk-forward, stress, benchmark, and forward gates.
2. Revalidate IB paper execution, market-data subscription, short sale/locate
   behavior, reconciliation, minimum order value, and realized slippage.
3. Validate that the cluster does not run an unused IB Gateway sidecar and does
   not create competing market-data sessions.
4. Paper trade before any live credentials are selected. Live deployment
   requires an explicit later decision; it is not implied by research success.

## What should not be done next

- Do not deploy or paper-trade the current model as if it were a candidate.
- Do not force a 50/50 book or normalize sleeves upward to spend budgets.
- Do not add Stockholm-specific broker/data logic to the bot; extend the shared
  crates.
- Do not use Python to own final features, labels, normalization, costs, or
  portfolio semantics.
- Do not claim historical short availability from `FEE_RATE`.
- Do not treat the current OMXSGI comparison as optional; it is currently much
  better than the bot in the recent diagnostic.
- Do not search more rewards, horizons, tree parameters, regime thresholds, or
  constructors on the already-exposed folds.

## Reproducible evidence

The principal local artifacts are:

- close-priced benchmark regeneration of the two headline runs, the current
  reference (2026-08-13, Task 4):
  `var/stockholm-remediation/task4/lean-report-base.json`,
  `var/stockholm-remediation/task4/rich-pseudo-holdout-summary.json`, the
  per-fold summaries and 100 phase replays beside them, produced by
  `var/stockholm-remediation/task4/regenerate.sh`;
- excess-return re-summarisation of Task 2's replays (2026-08-13, Task 3),
  superseded for the benchmark leg only:
  `var/stockholm-remediation/task3/lean-report-base.json` and the summaries
  beside it, produced by `var/stockholm-remediation/task3/resummarize.sh`;
- calendar-aligned regeneration of the two headline runs (2026-08-13, Task 2):
  `var/stockholm-remediation/task2/lean-report-base.json`,
  `var/stockholm-remediation/task2/rich-pseudo-holdout-summary.json`, and the
  per-fold summaries and 100 phase replays beside them, produced by
  `var/stockholm-remediation/task2/regenerate.sh`;
- lean corrected all-phase development, period-index combination, superseded:
  `var/stockholm/baseline-membership-prenorm-v2-phases/report-base.json`;
- rich corrected development:
  `var/stockholm/rich-v11-membership-factor-corrected-v3/report-base.json`;
- rich recent all-phase diagnostic:
  `var/stockholm/rich-v11-membership-factor-corrected-v3-pseudo-holdout/phases-base/report.json`;
- v16 global-risk first-fold reports:
  `var/stockholm/rich-v16-global-risk-membership-v1/fold-1-base.json` and
  `fold-1-stress.json` in the same directory;
- 20-session direction v3:
  `var/stockholm/rich-direction-v3-stockholm-close/fold-1.json`;
- one-session direction v3:
  `var/stockholm/rich-direction-1d-v3-stockholm-close/fold-1.json`;
- full historical experiment narrative:
  [`stockholm-reward-loss-study.md`](stockholm-reward-loss-study.md).

`var/` is intentionally ignored by Git because matrices, models, PDFs, and
reports are large/generated artifacts. The committed Markdown records the
decisions; the local JSON reports retain the exact metrics and diagnostics.

## Bottom line

The system engineering is substantially built, the intended non-neutral
portfolio construction is represented correctly, and many plausible signals
have been tested causally. The research result is still bad relative to the
goal: no aggregate reaches Sharpe 2.0, the latest diagnostic loses money, and
OMXSGI wins decisively. The most defensible explanation is a combination of
weak/regime-dependent stock-selection alpha, no validated daily-EOD market
direction signal, execution friction, and incomplete point-in-time data—not a
single missing loss function or portfolio-weight formula.

The next credible improvement requires better evidence first: inactive prices,
point-in-time membership, quarterly fundamentals, a declared borrow scenario,
and new forward time. Until then, stopping promotion is the correct behavior.
