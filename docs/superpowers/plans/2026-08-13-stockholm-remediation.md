# Stockholm Bot Remediation & Relaunch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix every confirmed measurement/data defect from the 2026-08-13 audit, rebuild the evidence base on trustworthy data, restructure the portfolio as an index-core + long/short overlay, and run exactly one predeclared fundamentals-based research test under an evaluation that can actually resolve the answer.

**Architecture:** Four phases, strictly ordered. Phase 0 (code fixes to measurement, labels, and features) must land before any data rebuild, because one fix (Task 6) prevents a look-ahead leak that activates the moment delisted data arrives. Phase 1 acquires the missing data. Phase 2 changes the portfolio architecture so shorts are funded by selection, not market timing. Phase 3 runs one frozen research test and starts prospective shadow logging.

**Tech Stack:** Rust (features-stockholm, stockholm-portfolio, portfolio-construction, equity-data), Python (training orchestration only, per the standing rule), LightGBM.

**Spec:** `docs/stockholm-portfolio-status.md` (current state), the 2026-08-13 audit findings (summarized in the "Audit findings this plan fixes" section below), and `docs/stockholm-portfolio-design.md` (production contract).

## Global Constraints

- Python fits models only; all feature, label, cost, normalization, and portfolio semantics stay in Rust (standing project rule).
- No new experiment arms on the already-exposed 2016–2026 folds beyond the single predeclared Phase 3 test.
- `var/` stays out of Git; docs record decisions, JSON artifacts record metrics.
- Every matrix carries an explicit survivorship status; `SURVIVORSHIP_CLEAN` may be stamped only after Tasks 10–12 are complete and verified.
- All replays remain deterministic; no wall-clock or RNG in Rust simulation paths.
- Feature/label changes bump `feature_set_version` / `label_version`; old artifacts stay readable.

## Audit findings this plan fixes

| # | Finding (CONFIRMED unless noted) | Anchor |
|---|---|---|
| F1 | Phase combiner averages staggered overlapping period returns by index, then annualizes as independent → Sharpe inflated ~1.2–1.9× for bot and benchmark | `portfolio-construction/src/lib.rs:417` (`equal_weight_phase_returns`), consumed by `summarize_rebalance_phases` in `stockholm-portfolio/src/lib.rs:2169-2289` |
| F2 | OMXSGI `start_value` is prior close (+dividend adj), not an open: benchmark is close-to-close vs portfolio open-to-open; direction label collects the untradable overnight gap | `equity-data/src/lib.rs:1570-1629`; `features-stockholm/src/lib.rs:704-706`; `stockholm-portfolio/src/lib.rs:3021-3062` |
| F3 | Latent look-ahead: stocks lacking bars through t+1+H are dropped from the cross-section before rank normalization/weights — dormant with survivor data, becomes an active leak when delisted histories arrive | `features-stockholm/src/lib.rs:4170-4174` |
| F4 | No risk-free rate anywhere: Sharpe is non-excess for bot and benchmark; backtest cash earns 0% | `summarize_stockholm.py`, `return_metrics` in `stockholm-portfolio/src/lib.rs:3157-3188` |
| F5 | No uncertainty reporting: 9-period Sharpes (SE ±1.1) drove accept/reject decisions across ~85 arms | `training/summarize_stockholm.py` |
| F6 | `gap_1` uses raw open / raw prior close → spurious gap on ex-dividend/split dates | `features-stockholm/src/lib.rs:1297-1299` |
| F7 | `volume_surge_20` uses raw share volume → fake surge after splits | `features-stockholm/src/lib.rs:4811-4822` |
| F8 | Trained direction score normalized by target std, not prediction spread → 0.4 enter threshold nearly unreachable, book parks at gross 0.30/net 0 | `training/train_stockholm_direction.py:118`, thresholds `portfolio-construction/src/lib.rs:203-205` |
| F9 | `allocate_with_group_cap` doesn't re-apply `min_abs_weight` after sector scaling (dormant: min weight defaults 0) | `portfolio-construction/src/lib.rs:631-685` |
| F10 | Library borrow-fallback default is implicitly "per 5 sessions"; only correct because the CLI rescales it | `stockholm-portfolio/src/main.rs:2264,2458` vs `lib.rs:651-666` |
| F11 | FI net-short historical events keyed by position date; late filings are backfilled look-ahead for `ResidualPublicShort` features | `equity-data/src/lib.rs:4873-4922`, `features-stockholm/src/lib.rs:1743-1772` |
| F12 | SKV admission-date parser silently returns `None` on unparseable Swedish dates → issuer gets no admission gate | `equity-data/src/lib.rs:5149-5181` |
| F13 | (Structural) 20-name concentrated book replaces index exposure instead of overlaying it; net hard-capped at 0.5; direction ML unlearnable at ~250 independent samples | design |

---

## Phase 0 — Measurement and correctness fixes (code only, no new data)

### Task 1: Emit daily NAV marks from the backtest

**Files:**
- Modify: `service/crates/stockholm-portfolio/src/lib.rs` (backtest loop around lines 1900–2030 where `nav *= 1 + Σ(w·r) − costs` is applied per period; `Step` struct at ~line 700)
- Test: same file, `#[cfg(test)]` module

**Interfaces:**
- Produces: `Step.daily_marks: Vec<DailyMark>` where `DailyMark { date: Date, nav: f64 }` — one entry per session inside the holding period, NAV marked at each session's adjusted close with entry/exit costs applied on their actual sessions. Serialized with `#[serde(default)]` so old reports stay readable.

- [ ] **Step 1: Write the failing test.** Construct a two-position, one-period synthetic replay (reuse the existing test fixtures in the crate's test module that build a minimal matrix) and assert: (a) `step.daily_marks.len() == cadence_sessions`, (b) the last daily mark equals `step.nav` to 1e-12, (c) compounding the implied daily returns reproduces `step.period_return` to 1e-12.
- [ ] **Step 2: Run `cargo test -p stockholm-portfolio daily_marks` — expect FAIL (field does not exist).**
- [ ] **Step 3: Implement.** Inside the holding loop, for each session `s` in `(entry, exit]`, mark every held position at `adjusted_close(s)` (positions are already priced from the same bar store used for labels; reuse the per-instrument bar lookup the cost engine uses). Charge entry-leg costs on the first mark's session and exit-leg costs on the rebalance session, matching the existing period totals exactly so `period_return` is unchanged.
- [ ] **Step 4: Run the test — expect PASS. Also run the full crate test suite to prove `period_return`, `nav`, and every existing report field are bit-identical (regression: rerun one frozen fold via `backtest` against `var/stockholm/rich-v11-membership-factor-corrected-v3-pseudo-holdout/phases-base/phase-0.json` inputs and diff all pre-existing JSON fields).**
- [ ] **Step 5: Commit** — `feat(stockholm): mark portfolio NAV daily inside holding periods`

### Task 2: Calendar-aligned phase combination (fixes F1)

**Files:**
- Modify: `service/crates/portfolio-construction/src/lib.rs:417-433` (add new function; keep `equal_weight_phase_returns` for old-report compatibility but mark `#[deprecated]`)
- Modify: `service/crates/stockholm-portfolio/src/lib.rs:2169-2289` (`summarize_rebalance_phases`) and the fold-level phase summarizer command in `main.rs`
- Test: `portfolio-construction` test module

**Interfaces:**
- Produces: `pub fn equal_weight_phase_daily_navs(phases: &[Vec<(Date, f64)>]) -> Result<Vec<(Date, f64)>, String>` — input is each phase's daily `(date, nav)` series from Task 1; output is the equal-capital combined book's daily NAV on the intersection of sessions; combined performance is then computed from **daily** returns with `√252` annualization in `phase_performance`.

- [ ] **Step 1: Write the failing tests.**
  - *No-smoothing invariant:* feed 20 phases that each hold the same asset (identical daily returns, staggered rebalance days). The combined daily return series must equal the common daily return series exactly — today's index-averaging would flatten it.
  - *Benchmark identity:* combined-book Sharpe of N identical phases equals the single-phase Sharpe.
  - *Impossible-Sharpe guard:* combined Sharpe must be ≤ max single-phase Sharpe + tolerance when all phases hold the same asset (dev fold 3 currently violates this: averaged 4.56 vs max phase 3.74).
- [ ] **Step 2: Run — expect FAIL (function missing).**
- [ ] **Step 3: Implement** the date-keyed average: align phases on session date, average NAVs (equal capital, each phase normalized to NAV 1.0 at the common start date), error on date mismatches rather than truncating silently.
- [ ] **Step 4: Wire `summarize_rebalance_phases`** to consume daily marks when present and fall back (with an explicit `"combination_method": "legacy_period_index_average"` disclosure field) for old reports. New reports write `"combination_method": "calendar_aligned_daily_nav"`.
- [ ] **Step 5: Run all tests; regenerate `report-base.json` for the lean dev run and the rich pseudo-holdout from the existing phase JSONs (rerun phases with the Task 1 binary first) and record the corrected headline numbers in `docs/stockholm-portfolio-status.md`.** Expected direction: dev ~1.52→~1.4, benchmark recent ~2.38→~1.25.
- [ ] **Step 6: Commit** — `fix(stockholm): combine rebalance phases on calendar dates, not period index`

### Task 3: Excess returns, uncertainty, and an honest gate (fixes F4, F5)

**Files:**
- Modify: `service/crates/stockholm-portfolio/src/lib.rs:3157-3188` (`return_metrics`), CLI args in `main.rs`
- Modify: `training/summarize_stockholm.py`
- Test: Rust test module + `training/test_walk_forward_stockholm.py`

**Interfaces:**
- Produces: every performance block gains `risk_free_annual: f64` (CLI `--risk-free-annual`, default 0.02, documented as Riksbank policy-rate approximation until a SWESTR series is wired), `sharpe` becomes excess-return Sharpe, and gains `sharpe_se` (analytic Lo (2002) SE adjusted for the observation count) plus `active_tstat` (mean of per-period bot-minus-benchmark returns over its SE). `passed` gate becomes: `active_tstat >= 2.0` AND `sharpe - 1.64*sharpe_se >= target_sharpe_floor` (floor is a new explicit config; see Decision Point 1).

- [ ] **Step 1: Failing tests.** Rust: constant 1%-per-period returns with rf=0 vs rf=2% produce Sharpes that differ by exactly `0.02/vol_ann`; `sharpe_se` for 9 periods of known σ matches the closed form to 1e-9. Python: a synthetic two-fold summary reports `active_tstat` matching a hand-computed value.
- [ ] **Step 2: Run — expect FAIL.**
- [ ] **Step 3: Implement** in both places; per-period rf = `(1+rf_annual)^(cadence/252) − 1`, subtracted from bot and benchmark identically. Daily-NAV-based metrics (Task 2 path) use daily rf.
- [ ] **Step 4: Run tests — PASS. Regenerate the two headline summaries; update the status doc table with `± SE` columns.**
- [ ] **Step 5: Commit** — `feat(stockholm): excess-return Sharpe with standard errors and active-return t-stat gate`

### Task 4: Benchmark and direction-label timing convention (fixes F2)

**Files:**
- Modify: `service/crates/features-stockholm/src/lib.rs:614-618, 693-706` (direction label), `service/crates/stockholm-portfolio/src/lib.rs:3021-3062` (`benchmark_return`)
- Test: both crates' test modules

**Interfaces:**
- Produces: direction label v4 (`label_version` bump): `target = EOD(t+1+h) / EOD(t+1) − 1` — first tradable session's close to exit close; no overnight-gap credit. Benchmark period return in replays: `EOD(entry_session) → EOD(exit_session)`, matching the portfolio's daily-mark grid from Task 1 (both legs now marked at closes on identical dates). `start_value` (SOD) is no longer used for any label or comparison; keep it in the archive for provenance.

- [ ] **Step 1: Failing tests.** Feed a synthetic index series where SOD(t) == EOD(t−1) and EOD gaps 1% overnight: assert the new direction label excludes the decision-adjacent overnight gap (old label captured it); assert `benchmark_return` over one period equals `EOD(exit)/EOD(entry) − 1`.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement; bump direction `label_version` to `...-4` and refuse to mix old direction matrices with new replays (same guard pattern as the existing `trained_through` mismatch check at `main.rs:2537-2542`).**
- [ ] **Step 4: Run tests — PASS. Note in the status doc that all prior direction-model results were inflated by overnight-gap credit and are void.**
- [ ] **Step 5: Commit** — `fix(stockholm): index labels and benchmark legs use tradable closes, not prior-close SOD`

### Task 5: Fix the latent cross-section look-ahead (fixes F3) — MUST precede Phase 1

**Files:**
- Modify: `service/crates/features-stockholm/src/lib.rs:4170-4195` (candidate assembly), `4562-4631` (labels/weights)
- Modify: `training/train_stockholm.py` (`load_matrix` row filter)
- Test: features-stockholm test module (extend the existing prefix-invariance tests at ~4936, 5057)

**Interfaces:**
- Produces: cross-section membership at decision date t is determined only by information available at t (bars through t + admission gates). Stocks lacking bars through t+1+H stay in the peer group for rank normalization, sector medians, `market_target`, and weight denominators; their own row is emitted with `"target": null` (and null relative/risk targets). Python drops null-target rows before fitting; Rust replay skips them as candidates for that date only if the *entry bar* (t+1) is missing.

- [ ] **Step 1: Failing test (prefix invariance, the strong form).** Build a universe where instrument X has bars only through t+3 (H=20). Assert: every *other* instrument's features, ranks, sector medians, market_target, and sample weights at date t are **identical** whether X's history ends at t+3 or continues through t+1+H. Today this fails because X's disappearance from the group shifts peers' ranks.
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement**: move the `exit_index` availability check from group formation (4170-4174) to label emission; emit null targets; adjust sample weights to count all emitted rows of the date (weights describe the decision cross-section, not the trainable subset — document this in the manifest).
- [ ] **Step 4: Update `train_stockholm.py`** to filter `row["target"] is None` (and the reward-specific target fields) with a manifest-declared count of dropped rows; keep the ≥10,000-row floor.
- [ ] **Step 5: Run full Rust + Python test suites — PASS. Bump `feature_set_version`. Commit** — `fix(stockholm): decision-date cross-sections no longer conditioned on future label availability`

### Task 6: Corporate-action feature artifacts (fixes F6, F7)

**Files:**
- Modify: `service/crates/features-stockholm/src/lib.rs:1297-1299` (`gap_1`), `4811-4822` (`volume_surge_20`)
- Test: features-stockholm test module

- [ ] **Step 1: Failing tests.** Synthetic 2:1 split between t−1 and t with unchanged true price: `gap_1` must be ~0 (today ≈ −50%); `volume_surge_20` must be ~1 when true traded notional is flat (today ≈ 2×).
- [ ] **Step 2: Run — FAIL.**
- [ ] **Step 3: Implement**: `gap_1 = adjusted_open(t) / adjusted_close(t−1) − 1` (adjusted open already exists per the label code at 4638-4640); `volume_surge_20` computed on traded notional (`raw_close × raw_volume`, continuous across splits) instead of share volume.
- [ ] **Step 4: Run — PASS. Same `feature_set_version` bump as Task 5 (coordinate: one bump for the pair). Commit** — `fix(stockholm): gap and volume-surge features survive splits and dividends`

### Task 7: Small confirmed defects batch (fixes F9, F10, F11, F12)

**Files:**
- Modify: `service/crates/portfolio-construction/src/lib.rs:631-685`; `service/crates/stockholm-portfolio/src/lib.rs:651-666` + `main.rs:2264,2458`; `service/crates/equity-data/src/lib.rs:5149-5181`; candidate feature contracts that include `ResidualPublicShort`
- Test: respective crate test modules

- [ ] **Step 1: Failing tests, one per defect.** (a) Group-cap: a position scaled below `min_abs_weight` by a sector cap is dropped, not kept tiny. (b) Borrow fallback: library takes explicit `cadence_sessions` and charges `short_borrow_bps_annual × cadence/252` — a caller with cadence 20 and no CLI rescale gets the right charge (delete the implicit per-5-session convention and the CLI rescale together). (c) SKV parser: an unparseable Swedish date string produces a logged warning and a counted `admission_date_unparsed` entry in the collection manifest, not a silent `None`. (d) Public-short: `ResidualPublicShort` features are absent from every candidate contract; the historical position-date-keyed cursor is only reachable from a diagnostics path, and a new prospective collector records FI publication timestamps going forward.
- [ ] **Step 2–4: Standard red → green → full suites.**
- [ ] **Step 5: Commit** — `fix(stockholm): group-cap minimum weights, cadence-explicit borrow fallback, loud SKV parsing, retire lookahead-prone short features`

### Task 8: Direction layer — retire the trained model, fix the fixed one (fixes F8, part of F13)

**Files:**
- Modify: `training/train_stockholm_direction.py`, `service/crates/stockholm-portfolio/src/lib.rs:373-465`, candidate configs
- Test: stockholm-portfolio test module

**Interfaces:**
- Produces: trained direction models are removed from every promotable configuration (the ~250 independent 20-session market outcomes cannot support one; every tested variant lost to controls). The trainer stays for research but exports `score_scale = std(train_predictions)` (the model's own prediction spread) instead of `std(target)`, so scores actually span the `[−1, 1]` range the 0.4/0.8 thresholds assume. The fixed MA/vote trend state remains available as an optional **drawdown guard only**, default off.

- [ ] **Step 1: Failing test.** A model whose predictions have std 0.005 against targets with std 0.05 must produce scores with std ≈ 1 (near-full threshold range), not ≈ 0.1.
- [ ] **Step 2–4: red → green → suites; document in the status doc that direction forecasting is retired pending fundamentally new information, per audit finding "no validated direction signal, structurally unlearnable at this sample size".**
- [ ] **Step 5: Commit** — `fix(stockholm): normalize direction scores by prediction spread; retire trained direction from candidates`

---

## Phase 1 — Data validity (the P0 list, correctly ordered)

*Gate: Tasks 5–6 merged first. All collection tasks are operational; each ends with a written verification artifact.*

### Task 9: EODHD token + delisted daily histories

- [ ] **Step 1 (USER ACTION):** obtain an EODHD licence and set `EODHD_API_TOKEN` in the environment/secrets. This is the single cheapest unlock in the project — the provider code is already written and tested.
- [ ] **Step 2:** collect inactive/delisted daily histories **only** for ISINs corroborated by the 189 official Nasdaq delisting notices (`equity-data` delisted provider; the notice archive already maps 189/190 to ISINs).
- [ ] **Step 3:** validate terminal outcomes (cash mergers, bankruptcies → terminal price semantics) against the notice text before joining; write `var/stockholm/delisted-validation.json` with per-ISIN outcome, coverage, and rejects.
- [ ] **Step 4:** wire the delisted set into `matrix()` (it is currently collected but unconsumed — `main.rs:674`). The Task 5 fix guarantees dying names stay in their final cross-sections.

### Task 10: Point-in-time Main Market membership

- [ ] **Step 1:** reconcile SKV listings (with Task 7's loud parser), Nasdaq notices, and segment-change events into per-ISIN membership intervals; unresolvable issuers are listed explicitly in a coverage report, not silently unrestricted.
- [ ] **Step 2:** rebuild admission gates from intervals; extend the two existing prefix-invariance tests to interval exits (a stock leaving Main Market mid-history must leave the cross-section at its exit date).
- [ ] **Step 3:** write `var/stockholm/membership-coverage.json`: % of universe-days with interval evidence vs heuristic vs none.

### Task 11: Rebuild matrices and re-baseline honestly

- [ ] **Step 1:** rebuild lean + rich matrices with delisted names, PIT membership, new feature/label versions. Stamp `SURVIVORSHIP_CORRECTED` only if Task 9 coverage ≥ the declared threshold (delisting notices joined for ≥95% of notice-identified ISINs); otherwise keep the contaminated stamp with the coverage number.
- [ ] **Step 2:** replay the two frozen headline configurations (lean 20-phase dev, rich v11 pseudo-holdout) on corrected data with the Phase 0 evaluation. **This is a re-baseline, not a new experiment arm.** Record the honest corrected numbers in the status doc — expect dev to drop; that drop *is* the survivorship premium, worth documenting.
- [ ] **Step 3:** declare the conservative borrow-availability scenario (haircut by size/liquidity/fee bucket, from the audit's recommendation) in `docs/stockholm-portfolio-design.md` and implement it as a replay config; report target vs realized short budget in every future report.

### Task 12: Quarterly fundamentals collection (v15 data, no model)

- [ ] **Step 1:** run the existing shared EODHD quarterly provider across the (now PIT-corrected) universe; preserve report/filing dates; document revision-history caveats.
- [ ] **Step 2:** build the v15 matrix (one frozen feature contract: earnings surprise vs trailing consensus-free baseline, YoY revenue/EBIT change, post-report drift window flags — exactly the fields the report-event extraction already defines, now with quarterly cadence) — **but do not fit any model yet.** That happens once, in Phase 3.

---

## Phase 2 — Architecture: index core + self-funding overlay (fixes F13)

### Task 13: Overlay budget mode in portfolio-construction

**Files:**
- Modify: `service/crates/portfolio-construction/src/lib.rs` (new budget mode alongside `Budget::from_gross_net`)
- Test: portfolio-construction test module

**Interfaces:**
- Produces: `pub struct OverlayBudget { pub core_weight: f64 /* 1.0 */, pub overlay_gross: f64 /* e.g. 0.6 */, pub overlay_net_cap: f64 /* e.g. 0.1 */ }` and an allocator entry point that (a) always allocates `core_weight` to the benchmark instrument, (b) builds the long/short overlay from candidates under `overlay_gross`, (c) constrains |overlay net| ≤ `overlay_net_cap` by scaling down the heavier sleeve (scale-down-only, consistent with the existing no-redistribution contract), (d) leaves unfilled overlay capacity as nothing (core stays fully invested).

- [ ] **Step 1: Failing tests.** (a) With zero candidates the portfolio is exactly the core (beta ≈ 1 floor). (b) Overlay long and short sleeve sums differ by ≤ `overlay_net_cap`. (c) Caps never redistribute. (d) Realized combined net = core + overlay net.
- [ ] **Step 2–4: red → green → suites.**
- [ ] **Step 5: Commit** — `feat(portfolio): index-core plus self-funding overlay budget mode`

### Task 14: Overlay replay support in stockholm-portfolio

**Files:**
- Modify: `service/crates/stockholm-portfolio/src/lib.rs` (backtest), `main.rs` (CLI: `--allocation-mode overlay --core-tracking-cost-bps 10`)
- Test: crate test module + one full replay

**Interfaces:**
- Consumes: `OverlayBudget` from Task 13.
- Produces: core P&L = OMXSGI daily return minus `core_tracking_cost_bps`/yr (models an OMXS30-futures or ETF core; live implementation via the existing `cme-data`/`ib` crates is Phase P2 production work, not this plan); overlay P&L and costs exactly as today. Reports gain `core_return`, `overlay_return`, `overlay_alpha_tstat` (overlay return vs zero, since it is self-funding).

- [ ] **Step 1: Failing test.** Synthetic replay with a do-nothing model: total return equals benchmark minus tracking cost to 1e-9 (the "at least the index" floor, mechanically enforced).
- [ ] **Step 2–4: red → green; run the widened-book control below.**
- [ ] **Step 5: Commit** — `feat(stockholm): overlay backtest with index core and overlay alpha attribution`

### Task 15: Widen the overlay book

- [ ] **Step 1:** set overlay defaults: 40 names per side max, `position_weight` 0.015, `overlay_gross` 0.6 (i.e. up to 30% long + 30% short overlay on a 100% core). Rationale from the audit: the −21%→+6% phase dispersion at 20 names is uncompensated concentration noise; more names at smaller size shrinks it ~√2–√3 while spending the same gross.
- [ ] **Step 2:** replay the *existing frozen lean model* (no refit — this is a constructor re-baseline, allowed once) in overlay mode on corrected data, all phases, calendar-aligned combination. Record as the Phase 2 control in the status doc.

---

## Phase 3 — One predeclared test + prospective evidence

### Task 16: Shadow forward logging (start immediately — it needs no research result)

- [ ] **Step 1:** nightly local job (extend `bin/cycle.sh` or a systemd timer): after each Stockholm close, build the day's live-contract feature rows, score with the current frozen lean model + the Phase 2 overlay constructor, and append scores/allocations/modeled-costs/benchmark to an append-only `var/stockholm/shadow-log/` (never tuned against, per the P1 rule).
- [ ] **Step 2:** verify three consecutive sessions produce rows; document the untouched-interval policy in the status doc: this log is the only future evidence source that is not already exposed.

### Task 17: The single predeclared research test (v15 fundamentals / post-earnings drift)

- [ ] **Step 1: Predeclare in writing** (`docs/research/2026-XX-stockholm-v15-predeclaration.md`) **before any fit**: hypothesis (quarterly earnings change + post-report drift adds cross-sectional ordering orthogonal to the price-derived features); exactly one frozen v15 contract (Task 12 fields appended to the lean baseline); one reward (absolute return, L2 — the historical control, per the study's conclusion that the reward was never the binding constraint); purged expanding folds on the corrected matrix; overlay constructor; evaluation = Phase 0 metrics.
- [ ] **Step 2: Gates, declared up front:** promotable candidate requires `overlay_alpha_tstat ≥ 2.0` aggregate, positive overlay alpha in ≥ 3 of 4 folds, surviving 2× cost stress, and a positive shadow-log interval (Task 16) of ≥ 3 months before any paper order. Fail → the conclusion is "the limit is the market, not the pipeline" and the project pivots or stops per the capital plan.
- [ ] **Step 3: Run once. Write the result into the status doc either way.**

---

## Decision Point 1 (USER): the promotion gate

The audit's arithmetic (IC ≈ 0.03–0.05, ~250 names, 12.6 rebalances/yr) caps this structure near Sharpe ~1.0 net standalone. The overlay architecture changes what the right gate is: the floor is the index, and the question becomes *overlay alpha after costs*. Proposed replacement gate, for your explicit sign-off (this touches the frozen-strategy/capital-plan rules): **combined book must beat OMXSGI with `active_tstat ≥ 2.0`, and overlay alpha must be positive in ≥ 3 of 4 folds and in the shadow interval.** The absolute Sharpe 2.0 requirement, measured honestly (Task 2/3), would remain unmet by essentially any daily-EOD single-country equity design; keeping it means deciding *now* to widen scope (Nordic, intraday) rather than iterating Stockholm-only.

## Decision Point 2 (USER): First North — recommendation: NO for trading, defer even for training

Analyzed against the audit evidence:

**Against inclusion (decisive today):**
- **Costs already killed better-quality small caps.** The v11 news model collapsed +33.8%→+7.1% when measured Small Cap spreads replaced assumed costs; observed 20-session median spreads run ~36 bps on *Main Market*. First North spreads are materially wider — the 20-session edge (~1–2% gross for top decile when the signal works) does not fund FN round trips plus impact at realistic size.
- **Shorts are effectively impossible** (no borrow), so FN can only feed the long sleeve — reintroducing net-long asymmetry exactly where the overlay design just removed it.
- **Survivorship is worst there.** FN has the highest delisting/failure rate; the current data machinery (even after Phase 1) reconstructs Main Market membership from SKV/Nasdaq notices — FN's evidence trail is thinner, so FN rows would re-contaminate matrices that Phase 1 just cleaned. The old FN-inclusive Sharpes (0.47/0.86/0.99) were explicitly invalidated for this reason.
- **Data quality:** thinner official coverage (news mapping, PDMR matching, ESEF) → more silently-missing features, which the audit flagged as survivorship-shaped dropout.

**For inclusion (real, but not now):** breadth — FN roughly doubles the name count, and breadth is the binding constraint on the Sharpe ceiling.

**Recommendation:** exclude First North from the tradable universe and from Phase 1–3 matrices. The breadth argument is better served by **Nordic Main Markets (Helsinki, Copenhagen, Oslo)** — similar liquidity tier to Stockholm Mid/Small, far better data trails, existing shared-crate architecture extends naturally, and roughly 3× the cross-section at comparable cost realism. If Phase 3 passes on Stockholm, "Nordic expansion" becomes the next predeclared step; if you still want FN examined, the honest version is a *training-data-only* augmentation test (FN rows fitted, never traded) — predeclared, after Phase 3, and only with FN delisting evidence collected first.

---

## Execution order and independence

- Phase 0 tasks 1→2→3 are sequential (daily NAV feeds combination feeds metrics); 4–8 are independent of each other and can parallelize after 1–3.
- Phase 1 requires Tasks 5–6 merged (leak guard) and the user's EODHD token (Task 9 Step 1) — everything else in Phase 1 is blocked behind that token.
- Phase 2 is independent of Phase 1 (runs on contaminated data as a constructor control) but its *re-baseline* (Task 15 Step 2) should be redone once Phase 1 lands.
- Task 16 (shadow logging) can start the moment Phase 0 merges — it accrues untouched evidence while everything else proceeds.

## Self-review notes

- Spec coverage: F1–F13 each map to a task (F1→T2, F2→T4, F3→T5, F4/F5→T3, F6/F7→T6, F8→T8, F9–F12→T7, F13→T13–15); status-doc P0 items map to T9–T12; P1 items map to T16–T17; First North question → Decision Point 2.
- Deliberately out of scope: live/paper execution work (status doc P2 — still blocked on a passing candidate), completing all 41,077 report PDFs (superseded by quarterly fundamentals as the fundamentals bet; revisit only if T17 passes and v13 remains interesting), Nordic expansion (next predeclaration, not this plan).
- The two frozen headline replays are re-run twice (Phase 0 evaluation fix, Phase 1 data fix) — these are re-baselines of existing arms under corrected measurement, not new search, and are labeled as such in the status doc each time.
