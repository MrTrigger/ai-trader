# Crypto-Scalper Plan 3d: fs-3 and the Eligible Gate Run Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reach the pre-registered 18-month eligibility bar. Gate run 1 (`docs/scalper-gate-run-1.md`) was NOT eligible because fs-2's `depth_imb_02` needs the ±0.2% book band, which Binance's bookDepth archive carries only from 2026-01-15; the ±1.0% band exists for the full history (verified 2026-08-15 across BTC/ZEC/KAITO day files back to 2024-08). fs-3 removes the ±0.2% dependency; Amendment 2 pre-registers the change; run 2 is then eligible.

**Architecture:** `features-scalper` gains `FEATURE_SET_VERSION = "fs-rust-scalper-3"` with the three ±0.2%-dependent features (indices 27, 29, 37) replaced by ±1.0%-band-only equivalents; feature count stays 38, index 28 (`depth_imb_10`) is unchanged. `micro_join`/matrix/gate need no logic changes (the version wall refuses fs-2 artifacts automatically). Amendment 2 is written and committed BEFORE the run. Then the same commands as run 1, on the same frozen data (no backfill between).

**Tech Stack:** as plans 3/3c. User decision 2026-08-15: proceed with option (a).

## Global Constraints

- All of plan 3c's constraints. The DATA IS FROZEN: no pull/backfill of any kind before run 2 (runbook §4 rule) — the matrix is rebuilt from the data already on disk under `data/`.
- Pre-registration order is mandatory: Amendment 2 committed → code → run. Never the reverse.
- fs-1 golden values still bit-identical (the 26 shared features are untouched).

---

### Task 1: Amendment 2 (pre-registration)

**Files:** Modify `docs/scalper-research.md` (append "## Amendment 2 (2026-08-15): fs-3 for eligibility"), no other files.

Content (all of it): why (band availability fact with the verified evidence: ±0.2% first appears 2026-01-15 07:01 UTC BTCUSDT; ±1.0% present from 2024-08-01 for every symbol checked); what changes (exact feature substitutions below, feature-set version fs-3, everything else in Amendment 1 unchanged — fees, costs, horizons, thresholds, fold schedule, drop rule); the pre-committed expectation of consequence: run 2 replaces run 1 as the gate record; run 1 remains on file as non-eligible; **no result from run 1 informed the fs-3 substitution beyond the eligibility failure itself** (state this explicitly — the substitution is forced by data availability, not by run-1 P&L). The fee fixed-point rule applies to run 2 exactly as written; VIP1-8 remains unresolved until the user's authenticated fetch.

fs-3 substitutions (indices unchanged; only names/definitions at 27 and 29 change; 37's definition changes):
| # | fs-2 | fs-3 |
|---|------|------|
| 27 | depth_imb_02 = (bid_02−ask_02)/(bid_02+ask_02+eps) | **depth_10_z_60** = z-score of (bid_10+ask_10) over trailing 60 min (gap-reset, full-window) |
| 29 | depth_02_z_60 | **depth_10_log** = ln(bid_10+ask_10+eps) — raw log-depth level (a liquidity-regime feature; distinct from 27's z-score) |
| 37 | depth_slope = ln((bid_10+ask_10+eps)/(bid_02+ask_02+eps)) | **depth_imb_10_m15** = mean of depth_imb_10 over trailing 15 min (all-Some window, gap-reset) |

Rationale for each (in the amendment): 27/29 keep a depth-dynamics and a depth-level signal at the band that exists; 37 keeps an imbalance-persistence signal instead of a slope that needs two bands. `MicroMinute` keeps its bid_02/ask_02 fields (they may be None) — fs-3 simply doesn't read them.

- [ ] Commit — `Pre-register fs-3 before touching a feature`

---

### Task 2: fs-3 in code

**Files:** `crates/features-scalper/src/lib.rs`; tests in `scalper-data` that reference feature names by string (grep `depth_imb_02|depth_02_z_60|depth_slope`).

- [ ] Failing tests: golden fs-1 test still passes untouched (bit-identity); new tests for the three substituted features on synthetic MicroMinutes (z-score warm-up + gap reset; log level; m15 all-Some window); `FEATURE_NAMES` order test updated; version constant test.
- [ ] Implement; `cargo test --workspace` green; the end-to-end version-wall test in gate.rs proves an fs-2 artifact is refused against an fs-3 matrix (adjust the existing test's "stale" version string to fs-2 or add a sibling — either).
- [ ] Commit — `fs-3: read only the band the archive always had`

---

### Task 3: Gate run 2 (eligible)

Exactly run 1's commands with `gate-run-2` names, no data changes. Then `docs/scalper-gate-run-2.md` in run 1's corrected structure (eligibility checklist — condition 1 must now show ≥18 months per asset with the kept-row start dates; commands+runtimes; matrix/cost stats; per-horizon table incl. n_preds and zero-day counts; fold table; per-asset tables incl. concentration shares; fixed-point outcome; VERDICT). If eligible: the verdict sentence per protocol; if any horizon PASSes, that IS the gate outcome for that horizon subject to the fee fixed-point (which will again be PROVISIONAL if volume maps above VIP0 — state so).

- [ ] Rebuild release binary; run; write; audit-by-independent-reviewer (the controller does this); apply corrections; commit — `Run the eligible gate`
