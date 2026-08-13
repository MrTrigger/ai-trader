# Crypto-Scalper Plan 3: Signal Research Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The research pipeline that decides whether the scalper deserves to exist: 1m features in Rust, a training matrix, LightGBM training in Python, and a cost-charging walk-forward that reports annualized Sharpe against the > 2.0 gate.

**Architecture:** New lib crate `features-scalper` (house pattern: Rust computes every feature, incremental/append-safe). `scalper-data` gains `training-matrix` (JSONL manifest + rows) and `gate` (Rust simulation: lightgbm-json inference, cost charging, daily Sharpe, gate report). Python's role is training orchestration ONLY (user-stated hard rule): `train_scalper.py` fits and dumps the house JSON artifact; `walk_forward_scalper.py` slices purged folds and fits one model per fold — no simulation, no predictions, no feature math in Python, ever. The definitive gate run needs weeks of recorded book costs — this plan delivers and smoke-tests the machinery end to end on available data; it does NOT claim the gate.

**Tech Stack:** Rust (features, matrix), Python/LightGBM (training, walk-forward), JSONL interchange. Spec: `docs/superpowers/specs/2026-08-12-crypto-scalper-design.md`. Prior plans: 1 (venue), 2 (data) — both merged.

## Global Constraints

- Identity rule (from plan 2's final review): `data/scalper-universe.json` is the sole identity source. A candidate's HL coin name (e.g. `kPEPE`) is the row identity everywhere in this plan's outputs; the Parquet store key is `coin.to_uppercase()` (`KPEPE`); NEVER call `binance_um_symbol` on a store key.
- Python is for training orchestration ONLY (user-stated hard rule): fold slicing, fitting, artifact dumping. Features, inference, simulation, costs, Sharpe, gate verdict — all Rust. `FEATURE_SET_VERSION = "fs-rust-scalper-1"`.
- Append-safety: recomputing features over a longer bar history must reproduce identical earlier rows (same discipline as `features-crypto`, asserted by test).
- Costs are never optimistic: symbols missing from the cost summary get `DEFAULT_ROUND_TRIP_BPS = 20.0`; an unabsorbable notional (null in the summary) excludes the symbol from simulation entirely and the report says so.
- Sharpe is computed on daily net returns, annualized with √365, out-of-sample folds only. Per-trade or in-sample numbers never appear in the gate report.
- ZERO changes to `crypto-portfolio` source; `training/train.py` and the Stockholm scripts stay untouched (new files only in `training/`).
- Python: follow `training/pyproject.toml` conventions; tests are pytest files next to the scripts (`test_train_scalper.py`, `test_walk_forward_scalper.py`); run with `uv run pytest` from `training/`.
- CLI args hand-rolled as in `scalper-data/src/main.rs`; data under `--data-root` subdirs (`perp/`, `costs/`, `matrices/`, `models/`, `reports/`).
- Commit style: plain descriptive, no prefixes, Claude co-author trailer. Cargo commands from `service/`, Python from `training/`.

---

### Task 1: The `features-scalper` crate

**Files:**
- Create: `crates/features-scalper/Cargo.toml`, `crates/features-scalper/src/lib.rs`
- Modify: `Cargo.toml` (workspace members + deps table entry `features-scalper = { path = "crates/features-scalper" }`)

**Interfaces:**
- Consumes: `features_crypto::Bar` (re-used bar type; dep `features-crypto.workspace = true`, plus `chrono`, `serde`).
- Produces:
  - `pub const FEATURE_SET_VERSION: &str = "fs-rust-scalper-1";`
  - `pub const FEATURE_NAMES: [&str; 26]` (exact order below — the model contract order)
  - `pub struct FeatureRow { pub ts_utc: chrono::DateTime<chrono::Utc>, pub asset: String, pub values: Vec<Option<f64>> }` (values parallel to `FEATURE_NAMES`)
  - `pub fn compute(bars: &[Bar], btc_bars: &[Bar]) -> Result<Vec<FeatureRow>, String>` — `bars` = one asset's 1m bars ascending; `btc_bars` = BTC's 1m bars ascending (context; for BTC itself pass the same slice). One `FeatureRow` per input bar. Single pass, trailing state only.

**The feature catalog (26 features, exact definitions).** All log returns `r_k(t) = ln(close_t / close_{t-k bars})`. `vol60(t)` = population std of the last 60 one-minute log returns. `eps = 1e-12`. Features are `None` until their window is fully warm (and vol-scaled ones also `None` while `vol60 < eps`); windows are by bar count, with a reset rule: a timestamp gap > 120s from the previous bar resets ALL accumulators (mirrors features-crypto's identity-break latch) — post-gap rows warm up from scratch.

| # | name | definition |
|---|------|-----------|
| 0 | ret_1 | r_1(t) |
| 1 | ret_3 | r_3(t) |
| 2 | ret_5 | r_5(t) |
| 3 | ret_15 | r_15(t) |
| 4 | ret_30 | r_30(t) |
| 5 | ret_60 | r_60(t) |
| 6 | mom_5 | r_5 / (vol60·√5 + eps) |
| 7 | mom_15 | r_15 / (vol60·√15 + eps) |
| 8 | mom_30 | r_30 / (vol60·√30 + eps) |
| 9 | mom_60 | r_60 / (vol60·√60 + eps) |
| 10 | vol_15 | std of last 15 one-minute log returns |
| 11 | vol_60 | vol60 |
| 12 | vol_ratio_15_60 | vol_15 / (vol_60 + eps) |
| 13 | vwap_dist_60 | ln(close / VWAP60) where VWAP60 = Σ(typical·volume)/Σ(volume) over last 60 bars, typical=(high+low+close)/3; None if Σvolume < eps |
| 14 | volume_z_60 | (volume − mean60(volume)) / (std60(volume)+eps) |
| 15 | volume_ratio_5_60 | mean5(volume) / (mean60(volume)+eps) |
| 16 | trades_z_60 | z-score of `trades` over 60 bars; None if any bar's trades is None |
| 17 | hl_range | (high − low)/close / (vol60+eps) |
| 18 | body_frac | (close − open)/(high − low + eps) |
| 19 | upper_wick_frac | (high − max(open,close))/(high − low + eps) |
| 20 | lower_wick_frac | (min(open,close) − low)/(high − low + eps) |
| 21 | tod_sin | sin(2π · minute_of_day/1440) |
| 22 | tod_cos | cos(2π · minute_of_day/1440) |
| 23 | dow | day of week, Monday=0.0 … Sunday=6.0 |
| 24 | btc_ret_5 | BTC's r_5 at the SAME ts (exact ts match into btc_bars; None if BTC bar missing or BTC window cold) |
| 25 | rel_ret_5 | ret_5 − btc_ret_5 (None if either is None) |

For BTC itself, 24 equals 2 and 25 is 0.0 — deliberate, no special-casing.

**Deliberate deferral:** the spec's funding-rate feature is NOT in v1 — it needs a Binance UM funding-archive ingestion job that doesn't exist yet. Recorded in the Task 6 runbook as the first candidate for `fs-rust-scalper-2`; do not sneak a live-only funding source into training (train/live parity would silently break).

- [ ] **Step 1: Write the failing tests**

```rust
use chrono::{Duration, TimeZone, Utc};
use features_crypto::Bar;

fn bar(ts_min: i64, close: f64, volume: f64) -> Bar {
    let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::minutes(ts_min);
    Bar {
        ts_utc: ts,
        asset: "TEST".into(),
        interval_s: 60,
        open: close * 0.999,
        high: close * 1.001,
        low: close * 0.998,
        close,
        volume,
        quote_volume: Some(close * volume),
        trades: Some(10),
    }
}

fn ramp(n: usize) -> Vec<Bar> {
    (0..n).map(|i| bar(i as i64, 100.0 + i as f64 * 0.01, 5.0 + (i % 7) as f64)).collect()
}

#[test]
fn one_row_per_bar_and_cold_windows_are_none_not_zero() {
    let bars = ramp(70);
    let rows = compute(&bars, &bars).unwrap();
    assert_eq!(rows.len(), 70);
    let i = |name: &str| FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
    assert!(rows[0].values[i("ret_1")].is_none(), "no history yet");
    assert!(rows[30].values[i("ret_60")].is_none(), "60m window not warm at t=30");
    assert!(rows[65].values[i("ret_60")].is_some());
    assert!(rows[65].values[i("mom_15")].is_some());
    assert!(rows[65].values[i("vwap_dist_60")].is_some());
    assert!(rows[65].values[i("tod_sin")].is_some());
}

#[test]
fn appending_bars_never_changes_emitted_rows() {
    let long = ramp(200);
    let short: Vec<Bar> = long[..150].to_vec();
    let a = compute(&short, &short).unwrap();
    let b = compute(&long, &long).unwrap();
    for (x, y) in a.iter().zip(b.iter().take(150)) {
        assert_eq!(x.ts_utc, y.ts_utc);
        assert_eq!(x.values, y.values, "append changed history at {}", x.ts_utc);
    }
}

#[test]
fn a_gap_resets_the_windows() {
    let mut bars = ramp(70);
    // 10-minute hole after bar 64
    for b in bars.iter_mut().skip(65) {
        b.ts_utc += Duration::minutes(10);
    }
    let rows = compute(&bars, &bars).unwrap();
    let i = |name: &str| FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
    assert!(rows[64].values[i("ret_1")].is_some(), "warm before the gap");
    assert!(rows[65].values[i("ret_1")].is_none(), "the gap resets state");
    assert!(rows[69].values[i("ret_1")].is_none() || rows[69].values[i("ret_60")].is_none());
}

#[test]
fn btc_context_aligns_by_timestamp_and_rel_ret_is_zero_for_btc_itself() {
    let bars = ramp(70);
    let rows = compute(&bars, &bars).unwrap();
    let i = |name: &str| FEATURE_NAMES.iter().position(|n| *n == name).unwrap();
    let last = &rows[69];
    assert_eq!(last.values[i("btc_ret_5")], last.values[i("ret_5")]);
    assert_eq!(last.values[i("rel_ret_5")], Some(0.0));
    // Missing BTC ts → None
    let mut btc = ramp(70);
    btc.retain(|b| b.ts_utc != bars[69].ts_utc);
    let rows2 = compute(&bars, &btc).unwrap();
    assert!(rows2[69].values[i("btc_ret_5")].is_none());
    assert!(rows2[69].values[i("rel_ret_5")].is_none());
}

#[test]
fn the_catalog_is_the_contract() {
    assert_eq!(FEATURE_NAMES.len(), 26);
    let rows = compute(&ramp(70), &ramp(70)).unwrap();
    assert_eq!(rows[0].values.len(), FEATURE_NAMES.len());
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p features-scalper` (after scaffolding the crate with empty lib) → FAIL.

- [ ] **Step 3: Implement.** Single pass over `bars` with: `VecDeque<f64>` of last 61 closes (for r_1..r_60), `VecDeque<f64>` last 60 one-minute log returns (vol windows), `VecDeque<(f64,f64)>` last 60 (typical·volume, volume) for VWAP, `VecDeque<f64>` last 60 volumes and trades. Gap check against previous bar's ts (`> 120s` → clear everything). BTC lookup: pre-build `BTreeMap<DateTime<Utc>, f64>` of BTC r_5 by running the same return logic over `btc_bars` first (a private helper reused for both) — gap/reset rules identical. Validate input ordering (strictly ascending ts; error otherwise). Mean/std computed from the deques each row (60-element loops per row are fine at this scale; no incremental-variance cleverness to get wrong).

- [ ] **Step 4: Run** — `cargo test -p features-scalper` then full workspace → PASS.

- [ ] **Step 5: Commit** — `Compute the minutes the model will see`

---

### Task 2: The training matrix

**Files:**
- Create: `crates/scalper-data/src/matrix.rs`
- Modify: `crates/scalper-data/src/main.rs` (subcommand), `crates/scalper-data/Cargo.toml` (add `features-scalper.workspace = true`)

**Interfaces:**
- Consumes: `features_scalper::{compute, FEATURE_NAMES, FEATURE_SET_VERSION, FeatureRow}`, `crypto_portfolio::store::read_asset`, `data/scalper-universe.json` (`Vec<Candidate>` — coin + binance_um from plan 2).
- Produces:
  - `pub struct MatrixRow { pub ts: i64, pub asset: String, pub features: BTreeMap<String, f64>, pub fwd_bps: BTreeMap<String, f64> }` (serde; ts = epoch seconds; asset = HL coin name, e.g. `kPEPE`)
  - `pub fn forward_returns_bps(bars: &[Bar], horizons_min: &[i64]) -> Vec<BTreeMap<String, f64>>` — per bar t: for each H, `1e4·ln(close[ts+H·60s]/close[t])` looked up by EXACT timestamp (BTreeMap of ts→close); missing target bar → entry absent. Pure.
  - `pub fn matrix_rows(rows: &[FeatureRow], fwd: &[BTreeMap<String, f64>], stride_min: usize, coin: &str) -> Vec<MatrixRow>` — keeps every `stride_min`-th row (by index) where ALL features are `Some` and ALL horizons present. Pure.
  - CLI: `scalper-data training-matrix --data-root <dir> --universe <path> --start YYYY-MM-DD --end YYYY-MM-DD --out <path> [--stride 5] [--horizons 15,30,60]` — for each universe candidate with `binance_um != null`: store key = `coin.to_uppercase()` (identity rule), read bars from `{data-root}/perp`, compute features (BTC context from store key `BTC` — hard requirement: error if BTC missing from the universe or the store), write JSONL: manifest line `{"kind":"manifest","feature_set_version":…,"features":[…],"horizons_min":[…],"stride_min":…,"assets":[…]}` then rows. Prints per-asset row counts.

- [ ] **Step 1: Failing tests** (in `matrix.rs`; reuse Task 1's `bar`/`ramp` helpers duplicated locally — test code may repeat)

```rust
#[test]
fn forward_returns_look_up_exact_timestamps_and_vanish_at_the_edge() {
    let bars = ramp(100);
    let fwd = forward_returns_bps(&bars, &[15, 30]);
    assert_eq!(fwd.len(), 100);
    let expect = 1e4 * (bars[15].close / bars[0].close).ln();
    assert!((fwd[0]["15"] - expect).abs() < 1e-9);
    // 100 bars, indices 0..=99: t=84 is the last with a 15m target (needs bar 99);
    // t=85 would need bar 100. No index has a 30m target beyond t=69.
    assert!(fwd[84].contains_key("15") && !fwd[84].contains_key("30"));
    assert!(!fwd[85].contains_key("15"));
}

#[test]
fn matrix_rows_drop_cold_rows_and_stride_samples() {
    let bars = ramp(200);
    let rows = compute(&bars, &bars).unwrap();
    let fwd = forward_returns_bps(&bars, &[15]);
    let m = matrix_rows(&rows, &fwd, 5, "kTEST");
    assert!(!m.is_empty());
    assert!(m.iter().all(|r| r.asset == "kTEST"));
    assert!(m.iter().all(|r| r.features.len() == FEATURE_NAMES.len()), "only fully-warm rows survive");
    assert!(m.iter().all(|r| r.fwd_bps.contains_key("15")));
    let ts: Vec<i64> = m.iter().map(|r| r.ts).collect();
    assert!(ts.windows(2).all(|w| w[1] - w[0] >= 300), "stride 5 = ≥300s apart");
}
```

(For the `fwd[90]` line: compute the exact boundary in the test rather than hand-waving — last index with a 15m target is `100-15-1 = 84`. Keep the two precise assertions on 84 and 86 and delete the imprecise one.)

- [ ] **Step 2-4: fail → implement → pass.** Manifest is written first, one `serde_json::to_string` line per row after. Note in a comment: rows are emitted per-asset sequentially (asset blocks, ts ascending within each) — the trainer shuffles/splits by time, order here carries no meaning.

- [ ] **Step 5: Live-ish check** — using the perp data already pulled (BTC, ETH from plan 2; pull more months if the range is thin): generate a small matrix for 2026-06→2026-07, confirm the manifest line parses, row count printed per asset, spot-check one row's feature count = 26.

- [ ] **Step 6: Commit** — `Emit the matrix the trainer reads`

---

### Task 3: `training/train_scalper.py`

**Files:**
- Create: `training/train_scalper.py`, `training/test_train_scalper.py`

**Interfaces:**
- Consumes: matrix JSONL (manifest + rows).
- Produces: CLI `uv run python train_scalper.py --matrix PATH --horizon 30 --through YYYY-MM-DD --out PATH` → LightGBM JSON artifact:
  - `{"format_version": "crypto-lightgbm-json-1", "model_version": "ml-scalper-rust-features-1", "feature_set_version": <from manifest>, "horizon_min": 30, "trained_through": "...", "features": [...manifest order...], "label": "fwd_bps_winsorized", "model": <booster.dump_model()>}` — same envelope discipline as `train.py` (read it for the dump/save idiom; do not modify it).
  - Public functions (imported by tests and by walk_forward): `load_matrix(path) -> (manifest, rows)` (streaming, list of dicts), `prepare(rows, feature_names, horizon, through_ts) -> (X, y)` — keeps rows with `ts <= through_ts`, label = `fwd_bps[str(horizon)]` winsorized at the 0.5/99.5 percentiles of the training set, `fit(X, y, feature_names) -> booster` with pinned params: `objective="regression", num_leaves=63, learning_rate=0.05, n_estimators=400, min_data_in_leaf=500, feature_fraction=0.8, bagging_fraction=0.8, bagging_freq=1, seed=7`, early stopping on the chronologically-last 10% of training rows (`early_stopping_rounds=50`). Refuse under 50,000 rows (scalping matrices are big; a small one means the pull was wrong).

- [ ] **Step 1: Failing tests** (`test_train_scalper.py`; synthetic matrix fixture built in-test: 60,000 rows, 26 feature names `f0..f25`, label = `3·f0 − 2·f1 + noise`)

```python
def synthetic_matrix(tmp_path, n=60_000, seed=0):
    rng = np.random.default_rng(seed)
    names = [f"f{i}" for i in range(26)]
    path = tmp_path / "m.jsonl"
    with open(path, "w") as fh:
        fh.write(json.dumps({"kind": "manifest", "feature_set_version": "fs-test",
                             "features": names, "horizons_min": [30], "stride_min": 5,
                             "assets": ["A", "B"]}) + "\n")
        for i in range(n):
            f = rng.normal(size=26)
            y = 3 * f[0] - 2 * f[1] + rng.normal(scale=0.5)
            fh.write(json.dumps({"ts": 1_700_000_000 + i * 300,
                                 "asset": "A" if i % 2 else "B",
                                 "features": dict(zip(names, f.round(6).tolist())),
                                 "fwd_bps": {"30": round(float(y), 6)}}) + "\n")
    return path, names

def test_fit_learns_a_planted_signal(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    manifest, rows = train_scalper.load_matrix(path)
    X, y = train_scalper.prepare(rows, names, 30, through_ts=10**12)
    booster = train_scalper.fit(X, y, names)
    imp = dict(zip(names, booster.feature_importance(importance_type="gain")))
    top2 = sorted(imp, key=imp.get, reverse=True)[:2]
    assert set(top2) == {"f0", "f1"}

def test_through_ts_excludes_the_future(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    _, rows = train_scalper.load_matrix(path)
    cutoff = 1_700_000_000 + 30_000 * 300
    X, y = train_scalper.prepare(rows, names, 30, through_ts=cutoff)
    assert len(X) == 30_001  # ts <= cutoff is inclusive: indices 0..=30000

def test_too_small_matrix_is_refused(tmp_path):
    path, names = synthetic_matrix(tmp_path, n=1_000)
    _, rows = train_scalper.load_matrix(path)
    with pytest.raises(ValueError):
        train_scalper.prepare(rows, names, 30, through_ts=10**12)

def test_artifact_envelope_roundtrips(tmp_path):
    path, names = synthetic_matrix(tmp_path)
    out = tmp_path / "model.json"
    train_scalper.main(["--matrix", str(path), "--horizon", "30",
                        "--through", "2262-01-01", "--out", str(out)])
    art = json.loads(out.read_text())
    assert art["format_version"] == "crypto-lightgbm-json-1"
    assert art["features"] == names
    assert art["horizon_min"] == 30
    assert "tree_info" in art["model"]
```

- [ ] **Step 2: Run to verify failure** — `uv run pytest test_train_scalper.py` → FAIL (module missing).
- [ ] **Step 3: Implement** (read `train.py` first for the artifact/save idioms; import nothing from it — copy the small dump pattern, it's ~10 lines).
- [ ] **Step 4: Tests pass.** Also run the whole training suite (`uv run pytest`) to prove Stockholm tests unaffected.
- [ ] **Step 5: Commit** — `Fit the scalper's model and dump it in house format`

---

### Task 4a: Fold orchestration — `training/walk_forward_scalper.py` (Python = training ONLY)

**House rule (user-stated, binding):** Python is allowed only for training orchestration. This script slices folds, fits per fold via Task 3's functions, and dumps artifacts. It computes NO feature, NO prediction file, NO simulation, NO Sharpe.

**Files:**
- Create: `training/walk_forward_scalper.py`, `training/test_walk_forward_scalper.py`

**Interfaces:**
- CLI: `uv run python walk_forward_scalper.py --matrix PATH --horizon 30 --out-dir DIR [--train-days 90] [--test-days 30] [--step-days 30]`
- Fold math: chronological folds over the matrix's ts span; first fold trains on days [0, 90), tests [90, 120), stepping 30; training rows with `ts > train_end − horizon·60` are dropped (embargo/purge — the label would peek across the boundary). Skip a fold whose training slice has < 50,000 rows (consistent with Task 3's floor); if ALL folds are skipped, exit nonzero with a clear message.
- Output: one model artifact per fold at `{out-dir}/fold-{i}.json` (Task 3's envelope, `trained_through` = fold train_end date) plus `{out-dir}/folds.json`:
  `{"matrix": ..., "horizon_min": H, "feature_set_version": <manifest's>, "folds": [{"i": 0, "train_start_ts": ..., "train_end_ts": ..., "test_start_ts": ..., "test_end_ts": ..., "model": "fold-0.json"}, ...]}`

- [ ] **Step 1: Failing tests** (`test_walk_forward_scalper.py`; reuse `synthetic_matrix` from `test_train_scalper.py` by import — same directory)

```python
def test_folds_are_chronological_purged_and_dumped(tmp_path):
    path, names = synthetic_matrix(tmp_path, n=120_000)  # 300s stride → ~416 days
    out = tmp_path / "folds"
    walk_forward_scalper.main(["--matrix", str(path), "--horizon", "30",
                               "--out-dir", str(out)])
    spec = json.loads((out / "folds.json").read_text())
    folds = spec["folds"]
    assert len(folds) >= 2
    for f in folds:
        assert f["train_end_ts"] <= f["test_start_ts"]
        assert (out / f["model"]).exists()
        art = json.loads((out / f["model"]).read_text())
        assert art["features"] == names
    # steps advance by test_days
    assert folds[1]["test_start_ts"] - folds[0]["test_start_ts"] == 30 * 86400

def test_embargo_drops_training_rows_inside_the_horizon(tmp_path):
    # prepare() must never see a training row whose label window crosses train_end.
    path, names = synthetic_matrix(tmp_path, n=60_000)
    _, rows = train_scalper.load_matrix(path)
    kept = walk_forward_scalper.training_slice(rows, train_start_ts=rows[0]["ts"],
                                               train_end_ts=rows[-1]["ts"], horizon_min=30)
    assert max(r["ts"] for r in kept) <= rows[-1]["ts"] - 30 * 60

def test_all_folds_skipped_is_a_loud_failure(tmp_path):
    path, _ = synthetic_matrix(tmp_path, n=60_000)  # ~208 days, but train floor will pass — shrink instead
    small, _ = synthetic_matrix(tmp_path / "s", n=52_000)
    with pytest.raises(SystemExit):
        walk_forward_scalper.main(["--matrix", str(small), "--horizon", "30",
                                   "--out-dir", str(tmp_path / "o"),
                                   "--train-days", "400"])
```

(`synthetic_matrix` writes to a fixed filename inside the directory it's given — create the `tmp_path / "s"` directory first. Adjust row counts if the fold arithmetic in your implementation makes a boundary differ by one; the CONTRACT is: ≥2 folds from ~416 days with defaults, embargo strictly enforced, loud failure when nothing is trainable.)

- [ ] **Step 2-4: fail → implement → pass** (`uv run pytest` — whole training suite green).
- [ ] **Step 5: Emit the Rust parity fixture.** Add `training/make_parity_fixture.py` (small, committed): builds a deterministic 30-row matrix with seed 7, fits a 20-tree model via Task 3's `fit`, dumps `service/crates/scalper-data/tests/fixtures/parity/model.json`, `rows.jsonl`, and `expected.json` (the Python booster's `predict` outputs, full float precision). Run it; commit the three fixtures with the scripts. Task 4b's Rust test asserts lightgbm-json reproduces `expected.json` to 1e-9 — THE train/live parity proof.
- [ ] **Step 6: Commit** — `Slice the folds and fit them; Python's job ends there`

---

### Task 4b: The gate engine in Rust — `scalper-data gate`

The simulation IS future live logic, so it is Rust (house rule). Prediction runs through the `lightgbm-json` crate — the exact artifact-loading path the plan-4 bot will use.

**Files:**
- Create: `crates/scalper-data/src/gate.rs`
- Modify: `crates/scalper-data/src/main.rs` (subcommand), `crates/scalper-data/Cargo.toml` (add `lightgbm-json.workspace = true`)

**The simulation rules (exact, they ARE the deliverable):**

- Folds come from Task 4a's `folds.json`; per fold: load `fold-{i}.json`, verify `features` == matrix manifest's feature list (order-exact; mismatch = hard error), predict every test row (`ts` in `[test_start_ts, test_end_ts)`) via `lightgbm_json::predict`.
- Costs: `round_trip_bps(asset)` = `2·(taker_fee_bps + spread_bps_median/2 + cross_bps[notional])` from `--costs` JSON (plan 2 summary; `--notional` selects the bucket, default "5000"); `taker_fee_bps = 4.5`. Asset absent from the JSON → `DEFAULT_ROUND_TRIP_BPS = 20.0`. `cross_bps[notional]` null → the asset is EXCLUDED from simulation and listed in the report under `"excluded_thin_books"`.
- Trading rule, per asset independently: when flat and `pred > threshold_mult · round_trip_bps` → open long; `pred < −threshold_mult·round_trip_bps` → open short (`threshold_mult = 1.5`). A position holds for exactly `horizon` minutes, then closes at the bar `horizon` after entry (the matrix's `fwd_bps` IS that trade's gross return by construction — signal row t's realized fwd). Net trade bps = `±fwd_bps − round_trip_bps`. While a position is open the asset takes no new signals (rows within the hold window are skipped — enforce via `ts >= flat_after` per asset).
- Aggregation: each trade's net bps is booked on the UTC day of its ENTRY. Daily portfolio return (bps) = sum of that day's booked trade bps / `len(traded_assets)` (equal-weight capital slots, idle slots earn 0). Days with no trades are 0-return days and COUNT in the Sharpe (a strategy that never trades must score 0, not NaN).
- `sharpe = mean(daily) / std(daily) · sqrt(365)` over all test-fold days, `None` if fewer than 2 trading days or zero variance.
- Report JSON (`--out`): `{"generated_utc", "matrix", "costs", "horizon_min", "threshold_mult", "notional", "folds": [...per-fold {train_span, test_span, n_trades, sharpe}...], "per_asset": {asset: {n_trades, total_net_bps, hit_rate}}, "excluded_thin_books": [...], "daily_returns_bps": {...date: bps...}, "overall": {"n_trades", "sharpe_annualized", "gate": "PASS"|"FAIL"|"NO-TRADES", "gate_threshold": 2.0}}`.
- CLI: `scalper-data gate --matrix PATH --folds DIR/folds.json --costs PATH [--threshold-mult 1.5] [--notional 5000] --out PATH`.

**Interfaces (all `pub` in `gate.rs`, all pure except the CLI driver):**
- `pub struct Pred { pub ts: i64, pub asset: String, pub pred_bps: f64, pub fwd_bps: f64 }`
- `pub struct Trade { pub asset: String, pub entry_ts: i64, pub side: i8, pub net_bps: f64 }`
- `pub fn simulate(preds: &[Pred], round_trip_bps: &BTreeMap<String, f64>, horizon_min: i64, threshold_mult: f64) -> Vec<Trade>` — preds sorted (asset, ts); assets absent from `round_trip_bps` are untradeable (caller decides default vs exclusion before this call).
- `pub fn daily_returns_bps(trades: &[Trade], n_assets: usize, span: (i64, i64)) -> BTreeMap<chrono::NaiveDate, f64>` — every UTC day in `span` present, no-trade days 0.0.
- `pub fn annualized_sharpe(daily: &BTreeMap<chrono::NaiveDate, f64>) -> Option<f64>` — None if < 2 days or zero variance.
- `pub fn load_model(path: &Path, expected_features: &[String]) -> Result<Vec<lightgbm_json::Tree>, String>` (or whatever tree type `lightgbm-json`'s `predict` takes — read `crates/lightgbm-json/src/lib.rs:74` first and match it; the artifact envelope's `model` field holds the booster dump).

- [ ] **Step 1: Failing tests** (in `gate.rs`; helper `fn pred(asset: &str, ts: i64, p: f64, f: f64) -> Pred`)

```rust
#[test]
fn parity_with_the_python_booster() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/parity");
    let art: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/model.json")).unwrap()).unwrap();
    let features: Vec<String> = art["features"].as_array().unwrap().iter()
        .map(|v| v.as_str().unwrap().to_string()).collect();
    let trees = load_model(Path::new(&format!("{dir}/model.json")), &features).unwrap();
    let expected: Vec<f64> =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/expected.json")).unwrap()).unwrap();
    for (i, line) in std::fs::read_to_string(format!("{dir}/rows.jsonl")).unwrap().lines().enumerate() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        let values: Vec<f64> = features.iter()
            .map(|n| row["features"][n].as_f64().unwrap()).collect();
        let got = lightgbm_json::predict(&trees, &values);
        assert!((got - expected[i]).abs() < 1e-9, "row {i}: {got} vs {}", expected[i]);
    }
}

#[test]
fn costs_gate_trades_out() {
    // |pred| = 30 < 1.5 × 25 → nothing trades.
    let preds: Vec<Pred> = (0..20).map(|i| pred("A", i * 300, if i % 2 == 0 { 30.0 } else { -30.0 }, 5.0)).collect();
    let costs = BTreeMap::from([("A".to_string(), 25.0)]);
    assert!(simulate(&preds, &costs, 30, 1.5).is_empty());
}

#[test]
fn the_hold_window_blocks_overlapping_trades() {
    let preds: Vec<Pred> = (0..24).map(|i| pred("A", i * 300, 100.0, 8.0)).collect();
    let costs = BTreeMap::from([("A".to_string(), 10.0)]);
    let trades = simulate(&preds, &costs, 30, 1.5);
    assert!(!trades.is_empty());
    let entries: Vec<i64> = trades.iter().map(|t| t.entry_ts).collect();
    assert!(entries.windows(2).all(|w| w[1] - w[0] >= 30 * 60));
}

#[test]
fn shorts_earn_the_negated_move_and_both_sides_pay_costs() {
    let preds = vec![pred("A", 0, -50.0, -20.0)]; // predicts down 50, market fell 20
    let costs = BTreeMap::from([("A".to_string(), 10.0)]);
    let t = &simulate(&preds, &costs, 30, 1.5)[0];
    assert_eq!(t.side, -1);
    assert!((t.net_bps - (20.0 - 10.0)).abs() < 1e-9);
}

#[test]
fn no_trade_days_count_as_zero_and_flat_series_has_no_sharpe() {
    let day = 86_400i64;
    let trades = vec![Trade { asset: "A".into(), entry_ts: 3 * day + 60, side: 1, net_bps: 12.0 }];
    let daily = daily_returns_bps(&trades, 2, (0, 10 * day));
    assert_eq!(daily.len(), 10);
    assert_eq!(daily.values().filter(|v| **v == 0.0).count(), 9);
    let booked: f64 = *daily.values().find(|v| **v != 0.0).unwrap();
    assert!((booked - 6.0).abs() < 1e-9, "12bps over 2 equal-weight slots");
    let flat: BTreeMap<_, _> = daily.iter().map(|(d, _)| (*d, 0.0)).collect();
    assert!(annualized_sharpe(&flat).is_none());
}
```

- [ ] **Step 2-4: fail → implement → pass.** The CLI driver: parse matrix (manifest + rows), folds.json, costs JSON (plan 2 `CostSummary` shape — compute `round_trip_bps = 2·(4.5 + spread_bps_median/2 + cross)`, thin-book nulls → `excluded_thin_books`, absent assets → `DEFAULT_ROUND_TRIP_BPS = 20.0`); predict per fold; simulate per fold over ONLY that fold's test window; stitch daily returns across folds; write the report. Run the whole workspace suite.
- [ ] **Step 5: Commit** — `Charge the measured costs and let the Sharpe gate speak, in Rust`

---

### Task 5: End-to-end smoke (NOT the gate)

**Files:**
- Create: `docs/scalper-research-smoke.md` (the run log)

- [ ] **Step 1:** Pull 6 months of perp 1m for five liquid names: `cargo run -p scalper-data -- pull-binance-perp --data-root ../data/perp --assets BTC,ETH,SOL,DOGE,XRP --start 2026-02-01 --end 2026-08-01` (BTC/ETH partly cached from plan 2; expect ~260k bars/asset total).
- [ ] **Step 2:** `scalper-data universe --data-root ../data --top 25` (refresh), then `training-matrix --data-root ../data --universe ../data/scalper-universe.json --start 2026-02-01 --end 2026-08-01 --out ../data/matrices/smoke-2026-02-08.jsonl --stride 5` — note: only the five pulled assets will have bars; the subcommand must skip-with-warning universe names whose store directory is absent (verify it does; if Task 2 made that an error, loosen it to a warning in this task with a one-line change + test tweak).
- [ ] **Step 3:** For each horizon 15/30/60: `uv run python walk_forward_scalper.py --matrix … --horizon H --out-dir ../data/models/smoke-hH` then `cargo run -p scalper-data -- gate --matrix … --folds ../data/models/smoke-hH/folds.json --costs ../data/costs/<plan-2 smoke summary> --out ../data/reports/smoke-hH.json` (document that the cost file is a 30-second sample standing in for weeks).
- [ ] **Step 4:** Write `docs/scalper-research-smoke.md`: commands run, row counts, per-horizon overall Sharpe + trade counts, and the explicit statement: **this validates plumbing; the gate decision requires ≥4 weeks of recorded book costs and the full candidate universe.** No gate claim, whatever the numbers say. If Sharpe happens to be high, the doc says why it doesn't count (5 assets, stand-in costs, one period).
- [ ] **Step 5:** Commit — `Prove the research pipeline end to end without claiming the gate`

---

### Task 6: The research runbook

**Files:**
- Create: `docs/scalper-research.md`

- [ ] **Step 1:** Write the runbook: (a) nightly `pull-binance-perp` command for the universe's mapped assets; (b) `record-books` cadence (hourly cron, `--top 25 --seconds 3600 --interval 10`) and where it accrues; (c) universe refresh policy (weekly, additions need 90d of Binance history before entering training); (d) matrix + fold-training (`walk_forward_scalper.py`) + Rust gate (`scalper-data gate`) commands; (e) **the gate protocol, written before the numbers exist** (evidence-gate house style): minimum 4 weeks of book data, all mapped candidates in the matrix, horizons 15/30/60 each run, gate = overall out-of-sample annualized Sharpe > 2.0 on daily net returns with measured costs at the intended notional; universe selection = symbols may be dropped only by the pre-registered rule (negative total_net_bps over the walk-forward), one re-run after dropping, no other iteration; a FAIL means the project stops or returns to feature research — not threshold shopping.
- [ ] **Step 2:** Commit — `Write the gate protocol before the numbers exist`
