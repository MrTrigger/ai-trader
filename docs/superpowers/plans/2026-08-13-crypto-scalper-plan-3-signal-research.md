# Crypto-Scalper Plan 3: Signal Research Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The research pipeline that decides whether the scalper deserves to exist: 1m features in Rust, a training matrix, LightGBM training in Python, and a cost-charging walk-forward that reports annualized Sharpe against the > 2.0 gate.

**Architecture:** New lib crate `features-scalper` (house pattern: Rust computes every feature, incremental/append-safe). `scalper-data` gains a `training-matrix` subcommand emitting JSONL (manifest + rows). Python gains `training/train_scalper.py` (fit + house JSON dump) and `training/walk_forward_scalper.py` (purged folds, cost-aware simulation, daily Sharpe, gate report). The definitive gate run needs weeks of recorded book costs — this plan delivers and smoke-tests the machinery end to end on available data; it does NOT claim the gate.

**Tech Stack:** Rust (features, matrix), Python/LightGBM (training, walk-forward), JSONL interchange. Spec: `docs/superpowers/specs/2026-08-12-crypto-scalper-design.md`. Prior plans: 1 (venue), 2 (data) — both merged.

## Global Constraints

- Identity rule (from plan 2's final review): `data/scalper-universe.json` is the sole identity source. A candidate's HL coin name (e.g. `kPEPE`) is the row identity everywhere in this plan's outputs; the Parquet store key is `coin.to_uppercase()` (`KPEPE`); NEVER call `binance_um_symbol` on a store key.
- Features are Rust-only; Python fits and simulates but computes no feature (house rule). `FEATURE_SET_VERSION = "fs-rust-scalper-1"`.
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

### Task 4: `training/walk_forward_scalper.py`

**Files:**
- Create: `training/walk_forward_scalper.py`, `training/test_walk_forward_scalper.py`

**The simulation rules (exact, they ARE the deliverable):**

- Folds: `train_days=90, test_days=30, step_days=30`, chronological, first fold starts after 90 days of data; embargo: training rows with `ts > fold_train_end − horizon·60` are dropped (purge — the label would peek across the boundary).
- Per fold: fit on train rows (Task 3's `prepare`/`fit`), predict on test rows.
- Costs: `round_trip_bps(asset)` = `2·(taker_fee_bps + spread_bps_median/2 + cross_bps[notional])` from `--costs` JSON (plan 2 summary; `--notional` selects the bucket, default "5000"); `taker_fee_bps = 4.5`. Asset absent from the JSON → `DEFAULT_ROUND_TRIP_BPS = 20.0`. `cross_bps[notional]` null → the asset is EXCLUDED from simulation and listed in the report under `"excluded_thin_books"`.
- Trading rule, per asset independently: when flat and `pred > threshold_mult · round_trip_bps` → open long; `pred < −threshold_mult·round_trip_bps` → open short (`threshold_mult = 1.5`). A position holds for exactly `horizon` minutes, then closes at the bar `horizon` after entry (the matrix's `fwd_bps` IS that trade's gross return by construction — signal row t's realized fwd). Net trade bps = `±fwd_bps − round_trip_bps`. While a position is open the asset takes no new signals (rows within the hold window are skipped — enforce via `ts >= flat_after` per asset).
- Aggregation: each trade's net bps is booked on the UTC day of its ENTRY. Daily portfolio return (bps) = sum of that day's booked trade bps / `len(traded_assets)` (equal-weight capital slots, idle slots earn 0). Days with no trades are 0-return days and COUNT in the Sharpe (a strategy that never trades must score 0, not NaN).
- `sharpe = mean(daily) / std(daily) · sqrt(365)` over all test-fold days, `None` if fewer than 2 trading days or zero variance.
- Report JSON (`--out`): `{"generated_utc", "matrix", "costs", "horizon_min", "threshold_mult", "notional", "folds": [...per-fold {train_span, test_span, n_trades, sharpe}...], "per_asset": {asset: {n_trades, total_net_bps, hit_rate}}, "excluded_thin_books": [...], "daily_returns_bps": {...date: bps...}, "overall": {"n_trades", "sharpe_annualized", "gate": "PASS"|"FAIL"|"NO-TRADES", "gate_threshold": 2.0}}`.
- CLI: `uv run python walk_forward_scalper.py --matrix PATH --costs PATH --horizon 30 [--threshold-mult 1.5] [--notional 5000] --out PATH`.

**Interfaces:** public functions `simulate(rows_by_asset, preds, costs, horizon, threshold_mult) -> list[Trade]`, `daily_returns(trades, assets) -> dict[date, float]`, `annualized_sharpe(daily) -> float | None`, `run(args) -> report dict` — all imported by tests.

- [ ] **Step 1: Failing tests**

```python
def test_planted_signal_with_zero_costs_passes_and_noise_fails(tmp_path):
    # label == 10*f0 exactly; predictions from a real fit will track it.
    ...build synthetic matrix as in test_train_scalper but y = 10*f0, 250 days of rows...
    report = walk_forward_scalper.run([...--costs, zero_costs_path, ...])
    assert report["overall"]["gate"] == "PASS"
    ...same rows with y = pure noise...
    report2 = walk_forward_scalper.run([...])
    assert report2["overall"]["gate"] in ("FAIL", "NO-TRADES")

def test_costs_gate_trades_out():
    # One asset, alternating +30/-30bps preds, threshold_mult=1.5, round_trip=25
    # → |pred|=30 < 37.5 → no trades.
    trades = walk_forward_scalper.simulate(rows, preds, {"A": 25.0}, 30, 1.5)
    assert trades == []

def test_hold_window_blocks_overlapping_trades():
    # Constant +100bps pred every 5 minutes, horizon 30 → trades at least 30min apart per asset.
    ...
    entries = [t.entry_ts for t in trades]
    assert all(b - a >= 30 * 60 for a, b in zip(entries, entries[1:]))

def test_thin_book_assets_are_excluded_not_defaulted():
    # costs JSON with cross_bps {"5000": None} → asset in excluded_thin_books, zero trades.
    ...

def test_no_trade_days_count_as_zero():
    daily = walk_forward_scalper.daily_returns([one_trade_on_day_3], ["A"], span_days=10)
    assert len(daily) == 10 and sum(1 for v in daily.values() if v == 0.0) == 9

def test_sharpe_of_flat_series_is_none():
    assert walk_forward_scalper.annualized_sharpe({d: 0.0 for d in ten_days}) is None
```

(Write the elided setup code fully in the test file; the assertions above are the contract. `daily_returns` takes an explicit `span_days`/date-range argument so zero-days are constructible — match the signature to that need.)

- [ ] **Step 2-4: fail → implement → pass** (`uv run pytest` whole suite green).
- [ ] **Step 5: Commit** — `Charge the measured costs and let the Sharpe gate speak`

---

### Task 5: End-to-end smoke (NOT the gate)

**Files:**
- Create: `docs/scalper-research-smoke.md` (the run log)

- [ ] **Step 1:** Pull 6 months of perp 1m for five liquid names: `cargo run -p scalper-data -- pull-binance-perp --data-root ../data/perp --assets BTC,ETH,SOL,DOGE,XRP --start 2026-02-01 --end 2026-08-01` (BTC/ETH partly cached from plan 2; expect ~260k bars/asset total).
- [ ] **Step 2:** `scalper-data universe --data-root ../data --top 25` (refresh), then `training-matrix --data-root ../data --universe ../data/scalper-universe.json --start 2026-02-01 --end 2026-08-01 --out ../data/matrices/smoke-2026-02-08.jsonl --stride 5` — note: only the five pulled assets will have bars; the subcommand must skip-with-warning universe names whose store directory is absent (verify it does; if Task 2 made that an error, loosen it to a warning in this task with a one-line change + test tweak).
- [ ] **Step 3:** Train + walk-forward for horizons 15/30/60 with the plan-2 smoke cost file (and document that it is a 30-second sample standing in for weeks): three `walk_forward_scalper.py` runs, reports into `data/reports/`.
- [ ] **Step 4:** Write `docs/scalper-research-smoke.md`: commands run, row counts, per-horizon overall Sharpe + trade counts, and the explicit statement: **this validates plumbing; the gate decision requires ≥4 weeks of recorded book costs and the full candidate universe.** No gate claim, whatever the numbers say. If Sharpe happens to be high, the doc says why it doesn't count (5 assets, stand-in costs, one period).
- [ ] **Step 5:** Commit — `Prove the research pipeline end to end without claiming the gate`

---

### Task 6: The research runbook

**Files:**
- Create: `docs/scalper-research.md`

- [ ] **Step 1:** Write the runbook: (a) nightly `pull-binance-perp` command for the universe's mapped assets; (b) `record-books` cadence (hourly cron, `--top 25 --seconds 3600 --interval 10`) and where it accrues; (c) universe refresh policy (weekly, additions need 90d of Binance history before entering training); (d) matrix + train + walk-forward commands; (e) **the gate protocol, written before the numbers exist** (evidence-gate house style): minimum 4 weeks of book data, all mapped candidates in the matrix, horizons 15/30/60 each run, gate = overall out-of-sample annualized Sharpe > 2.0 on daily net returns with measured costs at the intended notional; universe selection = symbols may be dropped only by the pre-registered rule (negative total_net_bps over the walk-forward), one re-run after dropping, no other iteration; a FAIL means the project stops or returns to feature research — not threshold shopping.
- [ ] **Step 2:** Commit — `Write the gate protocol before the numbers exist`
