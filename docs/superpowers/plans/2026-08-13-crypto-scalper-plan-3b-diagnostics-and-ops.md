# Crypto-Scalper Plan 3b: Gate Diagnostics and Operational Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the gate report explain *why* it passed or failed (signal-quality diagnostics), close the three carried hardening items from plan 3's final review, and ship the cron wrappers that start the book-recording evidence clock.

**Architecture:** All Rust changes in `scalper-data`'s `gate.rs`/`main.rs`. Two new `bin/` scripts in the house style (`bin/cycle.sh` is the model: plain script, non-zero exit fails loud, cron invokes what a human invokes). Follow-up to `docs/superpowers/plans/2026-08-13-crypto-scalper-plan-3-signal-research.md`.

**Tech Stack:** Rust, bash. Global constraints of plan 3 carry over verbatim (Python-training-only, no crypto-portfolio changes, commit style).

---

### Task 1: Gate diagnostics + carried hardening

**Files:**
- Modify: `crates/scalper-data/src/gate.rs`, `crates/scalper-data/src/main.rs`

**Interfaces:**
- `pub fn pearson_ic(preds: &[Pred]) -> Option<f64>` — Pearson correlation of `pred_bps` vs `fwd_bps`; `None` under 30 preds or zero variance on either side.
- `pub fn rank_ic(preds: &[Pred]) -> Option<f64>` — Spearman (Pearson over average ranks, ties averaged); same `None` rules.
- `pub fn pred_magnitude_quantiles(preds: &[Pred]) -> Option<(f64, f64, f64)>` — (p50, p90, p99) of `|pred_bps|` via the linear-interpolation percentile already used in `costs.rs` (duplicate the small helper or make the existing one `pub(crate)` — prefer the latter).
- Report additions: each fold entry gains `"ic"`, `"rank_ic"`, `"pred_abs_p50"/"p90"/"p99"`, `"n_preds"`; `overall` gains `"ic"`, `"rank_ic"` (computed over ALL folds' test preds pooled) and `"threshold_bps_by_asset"` (the per-asset entry threshold actually used = threshold_mult × round_trip, so NO-TRADES reports are self-explaining).
- Hardening (parked/carried from plan 3's final review):
  1. `main.rs` USAGE line for `gate`: `">= 2.0 to PASS"` → `"> 2.0 to PASS"`.
  2. `load_model` and `cmd_gate` cross-check `feature_set_version`: matrix manifest's value must equal each fold artifact's value; mismatch = hard error naming both.
  3. Runbook `docs/scalper-research.md`: add one sentence (in §4, matrix step) that deepening BACKFILL between a gate run and its re-run changes stride phase/warmup and thus rows — pull new history only BEFORE a gate cycle starts, never between run and re-run.

- [ ] **Step 1: Failing tests** (in `gate.rs` `mod tests`)

```rust
#[test]
fn ic_matches_a_hand_computed_correlation() {
    // pred = 2·fwd exactly → both ICs are 1; anti-correlated → -1.
    let mk = |sign: f64| -> Vec<Pred> {
        (0..40).map(|i| {
            let f = (i as f64) - 20.0 + ((i % 3) as f64) * 0.1;
            pred("A", i * 60, sign * 2.0 * f, f)
        }).collect()
    };
    assert!((pearson_ic(&mk(1.0)).unwrap() - 1.0).abs() < 1e-9);
    assert!((pearson_ic(&mk(-1.0)).unwrap() + 1.0).abs() < 1e-9);
    assert!((rank_ic(&mk(1.0)).unwrap() - 1.0).abs() < 1e-9);
}

#[test]
fn rank_ic_ignores_scale_but_pearson_does_not() {
    // Monotone but convex mapping: rank IC stays 1, Pearson dips below 1.
    let preds: Vec<Pred> = (0..40).map(|i| {
        let f = i as f64;
        pred("A", i * 60, f * f, f)
    }).collect();
    assert!((rank_ic(&preds).unwrap() - 1.0).abs() < 1e-9);
    assert!(pearson_ic(&preds).unwrap() < 0.999);
}

#[test]
fn too_few_or_degenerate_preds_yield_no_ic() {
    let few: Vec<Pred> = (0..10).map(|i| pred("A", i * 60, 1.0, 1.0)).collect();
    assert!(pearson_ic(&few).is_none(), "under 30 preds");
    let flat: Vec<Pred> = (0..40).map(|i| pred("A", i * 60, 5.0, i as f64)).collect();
    assert!(pearson_ic(&flat).is_none(), "zero pred variance");
}

#[test]
fn magnitude_quantiles_are_ordered() {
    let preds: Vec<Pred> = (0..100).map(|i| pred("A", i * 60, (i as f64) - 50.0, 0.0)).collect();
    let (p50, p90, p99) = pred_magnitude_quantiles(&preds).unwrap();
    assert!(p50 <= p90 && p90 <= p99);
    assert!(p50 > 0.0);
}

#[test]
fn a_feature_set_version_mismatch_is_refused() {
    // Build a minimal artifact JSON on disk with feature_set_version "fs-other"
    // and call the version check used by cmd_gate against manifest "fs-rust-scalper-1";
    // the error must name both versions.
}
```

(Fill the last test's body concretely once you see `load_model`'s current signature — the check may live in `load_model` gaining an `expected_version: &str` parameter, which existing tests then pass the fixture's own version to.)

- [ ] **Step 2-4: fail → implement → pass** (`cargo test -p scalper-data`, then full workspace). Wire the diagnostics into `cmd_gate`'s per-fold loop and the overall block; add the USAGE fix and the runbook sentence in the same commit.
- [ ] **Step 5: Commit** — `Make the gate report explain itself`

---

### Task 2: The evidence-clock scripts

**Files:**
- Create: `bin/scalper-record.sh`, `bin/scalper-pull.sh`
- Modify: `docs/scalper-research.md` (§1/§2: replace the raw-command cron examples with the scripts + crontab lines)

Both scripts follow `bin/cycle.sh`'s conventions exactly: bash, `set -euo pipefail`, resolve `ROOT` from `BASH_SOURCE`, prefer `service/target/release/scalper-data` falling back to debug, refuse with exit 2 and a message naming the fix if no binary, every failure fails the script.

- `bin/scalper-record.sh [top] [seconds] [interval]` — defaults 25 / 3600 / 10; runs `record-books --data-root data --top ... --seconds ... --interval ...`. Doc comment: `# Cron it: 0 * * * *  cd /path/to/ai-trader && bin/scalper-record.sh >> var/live/scalper-record.log 2>&1`.
- `bin/scalper-pull.sh [days]` — default 3; reads `data/scalper-universe.json` with `jq -r '.[] | select(.binance_um != null) | .coin'` (check jq is present; exit 2 naming it if not), then one `pull-binance-perp --data-root data/perp --assets <comma-list> --start <UTC today − days> --end <UTC tomorrow>` call. Doc comment: `# Cron it: 20 0 * * *  cd /path/to/ai-trader && bin/scalper-pull.sh >> var/live/scalper-pull.log 2>&1`. (End is exclusive on bar timestamps; "tomorrow" just means "through now".)

- [ ] **Step 1:** Write both scripts; `bash -n` both; run `bin/scalper-record.sh 3 30 10` for a live 30-second smoke (needs the release or debug binary built — build if absent) and confirm rows appended to `data/books/<today>.jsonl`; run `bin/scalper-pull.sh 2` and confirm it pulls without error for the mapped universe coins (universe file must exist — run `scalper-data universe --data-root data --top 25` first if absent).
- [ ] **Step 2:** Update the runbook's §1/§2 cron examples to invoke the scripts.
- [ ] **Step 3:** Commit — `Start the evidence clock from cron`
