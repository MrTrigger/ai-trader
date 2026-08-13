# Crypto-Scalper Plan 3c: Binance Venue Pivot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-aim the research pipeline at Binance UM as the execution venue, using Binance's own free historical microstructure archives for time-varying costs and a richer feature set — so the pre-registered gate can run on years of measured data without waiting for HL book recording. User decision (2026-08-13): prove profitability on Binance first; HL becomes a later port that justifies buying vendor data only after a PASS.

**Architecture:** Everything lands in existing crates (`scalper-data`, `features-scalper`) plus the runbook. New Binance archive sources (all verified live on data.binance.vision): `bookDepth` (30s depth bands ±0.2..±5%), `aggTrades` (per-trade, maker-side flag — the spread estimator's input; `bookTicker` is discontinued), `fundingRate`, `metrics` (5m OI + taker flow). Raw aggTrades are huge, so ingestion AGGREGATES streaming (per-minute stats kept, raw discarded). Python's role unchanged (fit only).

**Tech Stack:** Rust ingestion/features/costs/gate; house patterns from plans 2-3 throughout (404-vs-error discipline, `[start,end)` windows, append-safe features, pre-registration before numbers).

## Global Constraints

- All of plan 3's Global Constraints carry over (Python-training-only, no crypto-portfolio changes, identity rule, conservative costs, commit style).
- New data lives under `data/binance-micro/` (per-minute aggregates + daily band/metrics series), never mixing with `data/perp` bars or the frozen stores.
- Every derived series is causally aligned: a 1m bar at ts may only use micro data with timestamp ≤ that bar's close. bookDepth snapshots snap DOWNWARD (latest ≤ ts).
- Fees: Binance USDT-M with BNB discount. The CURRENT published VIP fee table must be fetched from binance.com during Task 4, committed as `docs/binance-um-fee-table-2026-08.md` (source URL + retrieval date), and the protocol references that snapshot — the gate must be reproducible.
- fs-2 features must be computable live from Binance WS streams (depth/aggTrade/markPrice) — nothing archive-only sneaks in.

---

### Task 1: Micro-archive ingestion (`scalper-data pull-binance-micro`)

**Files:** Create `crates/scalper-data/src/binance_micro.rs`; modify `main.rs` (subcommand), `binance_um.rs` (nothing — new module mirrors its fetch/404 idioms).

**Sources & products** (per symbol, per day; monthly zips where published, daily otherwise — probe like klines):
1. `bookDepth` daily zips (CSV: `timestamp,percentage,depth,notional`) → stored as-is per day, downsampled to one snapshot per minute (the last ≤ each minute close), JSONL `{ts_s, bands: {"-1.0": notional, "-0.2": ..., "0.2": ..., "1.0": ...}}` keeping the ±0.2 and ±1.0 bands only (the rest are noise for $5-20k clips) → `data/binance-micro/book/{SYMBOL}/{YYYY-MM-DD}.jsonl`.
2. `aggTrades` daily zips (CSV: `agg_id,price,qty,first_id,last_id,ts,is_buyer_maker`) → STREAMED, never stored raw. Per minute emit `{ts_s, spread_bps_med, n_spread_samples, taker_buy_ratio, n_trades, notional}` → `data/binance-micro/flow/{SYMBOL}/{YYYY-MM-DD}.jsonl`.
   - **Spread estimator (pre-registered):** pair each ask-side trade (`is_buyer_maker=false`) with the nearest sell-side trade (`is_buyer_maker=true`) within 1000ms; sample = (ask_px − bid_px)/mid × 1e4, discard negatives; minute value = median of samples; require ≥5 samples else the minute has no spread estimate.
   - `taker_buy_ratio` = buy-side notional / total notional that minute.
3. `fundingRate` monthly zips → `data/binance-micro/funding/{SYMBOL}.jsonl` (append-safe by ts).
4. `metrics` daily zips (5m rows: OI, OI value, top-trader ratios, taker long/short vol ratio) → per-5m rows passed through → `data/binance-micro/metrics/{SYMBOL}/{YYYY-MM-DD}.jsonl`.

CLI: `scalper-data pull-binance-micro --data-root data --assets ... --start --end [--sources book,flow,funding,metrics]`. 404-vs-error discipline identical to klines (404 = skip-with-note; anything else aborts). aggTrades files are large — stream-decompress (zip crate `by_index` reader straight into a line loop; NEVER read_to_string a multi-hundred-MB CSV).

- [ ] **Step 1: Failing tests** — pure parsers with in-memory zips (reuse `zip_with_one_file`): bookDepth minute-downsample keeps the LAST snapshot ≤ each minute and only ±0.2/±1.0 bands; spread estimator on a hand-built trade tape (known pairs → known median, unpaired trades ignored, <5 samples → None, negative spreads discarded); metrics/funding row passthrough with epoch autodetect (`epoch_utc` reuse).
- [ ] **Step 2-4:** fail → implement → pass; full workspace green.
- [ ] **Step 5: Live check** — one day, one symbol per source (`--assets BTC --start <yesterday> --end <today>`); eyeball each output file; note file sizes and runtimes in the report (aggTrades day for BTC is the stress case).
- [ ] **Step 6: Commit** — `Ingest the microstructure Binance publishes for free`

---

### Task 2: fs-rust-scalper-2 features

**Files:** Modify `crates/features-scalper/src/lib.rs`; modify `crates/scalper-data/src/matrix.rs` + `main.rs` (feed micro series in).

**Interfaces:**
- `FEATURE_SET_VERSION = "fs-rust-scalper-2"`. `FEATURE_NAMES` grows to 38 (the 26 existing + 12 below, appended in this order).
- `pub struct MicroMinute { pub ts_s: i64, pub spread_bps: Option<f64>, pub taker_buy_ratio: Option<f64>, pub bid_02: Option<f64>, pub ask_02: Option<f64>, pub bid_10: Option<f64>, pub ask_10: Option<f64>, pub oi_value: Option<f64>, pub taker_ls_ratio: Option<f64>, pub funding_rate: Option<f64> }` — assembled by the caller (matrix/live) from the micro files, aligned ≤ bar close; funding = the CURRENT period's rate (latest ≤ ts); metrics snap ≤ ts (5m lag is inherent and identical live).
- `compute` gains the series: `pub fn compute(bars: &[Bar], btc_bars: &[Bar], micro: &[Option<MicroMinute>]) -> Result<Vec<FeatureRow>, String>` (`micro.len() == bars.len()`; `None` = no micro data that minute → the 12 new features are None; existing 26 unchanged).

| # | name | definition |
|---|------|-----------|
| 26 | spread_bps | as ingested (minute median) |
| 27 | depth_imb_02 | (bid_02 − ask_02)/(bid_02 + ask_02 + eps) |
| 28 | depth_imb_10 | same at ±1.0% |
| 29 | depth_02_z_60 | z-score of (bid_02+ask_02) over trailing 60 minutes (gap/reset rules as ever) |
| 30 | taker_buy_ratio | as ingested |
| 31 | taker_buy_ratio_m15 | mean over trailing 15 minutes |
| 32 | oi_change_60 | ln(oi_value / oi_value 60m earlier); None if either missing |
| 33 | taker_ls_ratio | as ingested (metrics) |
| 34 | funding_rate_bps | funding_rate × 1e4 |
| 35 | funding_x_mom | funding_rate_bps × sign(ret_15) (crowding interaction) |
| 36 | spread_z_60 | z-score of spread_bps over trailing 60 minutes |
| 37 | depth_slope | ln((bid_10+ask_10+eps)/(bid_02+ask_02+eps)) |

Warm-up/None/append-safety discipline identical to fs-1; matrix rows still require ALL features Some (rows before micro coverage begins simply drop — state this in the matrix report output per asset: `kept N of M (micro coverage from DATE)`).

- [ ] **Step 1: Failing tests** — extend the fs-1 test style: micro all-None → 12 Nones + 26 unchanged values (regression: identical to fs-1 outputs for the shared 26); imbalance/slope hand-computed on synthetic MicroMinutes; z-score gap-reset; append-safety re-asserted on the new signature.
- [ ] **Step 2-4:** fail → implement → pass. **The version bump is the contract**: fs-1 artifacts must now be REFUSED by the gate against fs-2 matrices (the Task-1/3b cross-check already does this — add one test proving it end-to-end).
- [ ] **Step 5: Commit** — `Let the model see the book, the flow and the funding`

---

### Task 3: Time-varying Binance costs in the gate

**Files:** Create `crates/scalper-data/src/binance_costs.rs`; modify `gate.rs`, `main.rs`.

**Interfaces:**
- `scalper-data binance-costs --data-root data --assets ... --start --end --notional 5000 --out data/binance-micro/costs-daily.json` → `BTreeMap<String /*asset*/, BTreeMap<String /*YYYY-MM-DD*/, DayCost>>` with `DayCost { spread_bps_p75: f64, impact_bps: Option<f64>, samples: u32 }`.
  - `spread_bps_p75`: p75 of the day's minute spread estimates (flow files).
  - **Impact model (pre-registered):** from the day's median ±0.2% band notionals: `impact_bps = 1.5 × (notional / min(bid_02, ask_02)) × 10.0`, `None` (thin) if `notional > min(bid_02, ask_02)` — uniform-density walk within the 20bps band, halved for average depth, ×1.5 safety.
- `gate` gains `--binance-costs PATH --fee-taker-bps F --fee-maker-bps F` (mutually exclusive with `--costs`): per trade, `round_trip_bps = 2×fee_taker + spread_p75 + 2×impact` looked up by the ENTRY day; missing day → nearest PRIOR day within 14 days, else the asset is untradeable that day (counted and reported as `days_without_costs`). Thin-`impact` days are untradeable days, not exclusions of the whole asset.
- Report additions: `overall.projected_30d_volume_usd` = (total trade count × 2 × notional) ÷ (test-span days) × 30, and `overall.fee_bps_used`. Funding cost on holds is NOT charged — pre-registered as negligible for ≤60m holds (expected |cost| < 0.2bps/trade at typical 1bp/8h rates); documented in the runbook amendment with this arithmetic.

- [ ] **Step 1: Failing tests** — impact model hand-computed cases incl. the thin→None branch and the 1.5 multiplier; day-lookup fallback (exact day, prior-within-14, nothing → untradeable); round-trip formula; volume projection arithmetic on a synthetic trade set.
- [ ] **Step 2-4:** fail → implement → pass; full workspace green.
- [ ] **Step 5: Commit** — `Charge each trade what Binance would have charged that day`

---

### Task 4: Protocol amendment + fee-table snapshot

**Files:** Create `docs/binance-um-fee-table-2026-08.md`; modify `docs/scalper-research.md`.

- [ ] **Step 1:** Fetch Binance's current USDT-M VIP fee schedule (binance.com fee page; WebFetch/curl), transcribe the maker/taker bps per tier WITH the BNB-discount column, source URL and retrieval date, into the snapshot doc.
- [ ] **Step 2:** Amend the runbook (a new dated "Amendment 1: Binance venue" section — do NOT rewrite history; the original HL protocol text stays, marked superseded):
  - Venue = Binance UM. Gate eligibility: ≥18 months of matrix span, all mapped candidates, fs-2 features, time-varying costs from Task 3, all three horizons.
  - **Fee fixed-point rule (pre-registered):** run 1 charges VIP0+BNB taker; read `projected_30d_volume_usd`; map to the snapshot table's tier (volume criterion only — assume BNB holding requirement met); if the tier differs, ONE re-run at that tier's fees; report both runs; the SECOND run's verdict is the gate verdict. No further iteration.
  - Funding-not-charged note with the arithmetic; HL recording continues accruing (port option); "buy HL data" is the PASS-branch action, named explicitly.
- [ ] **Step 3:** Commit — `Amend the protocol: prove it on Binance first`

---

### Task 5: The backfill and the gate run

- [ ] **Step 1:** Kick off the deep backfill (background, it's days of downloads): klines (already partly present) + micro sources for all mapped candidates, 2024-08-01 → now. Order: klines → bookDepth/metrics/funding (small) → aggTrades (the bulk; majors last so small caps land early). Log progress per symbol-month to `var/live/scalper-backfill.log`.
- [ ] **Step 2:** When ingestion completes: build the fs-2 matrix (full universe, full span, stride 5), run the fold-training and gate for horizons 15/30/60 at VIP0+BNB fees, then apply the fee fixed-point rule. Write `docs/scalper-gate-run-1.md` recording commands, spans, per-horizon reports, the volume/tier arithmetic, and the verdict — in the smoke log's epistemically-careful voice, but THIS one is gate-eligible if the amendment's conditions are met; say so explicitly either way.
- [ ] **Step 3:** Commit — `Run the gate on the venue with the evidence`
