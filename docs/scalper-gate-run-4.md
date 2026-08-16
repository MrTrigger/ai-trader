# Crypto scalper — gate run 4 (fs-4, Amendment 4 causal metrics join)

> **Bottom line up front: this run IS gate-eligible, and it is the first
> uncontaminated gate result — GATE FAILED at all three horizons.** Runs 1–3
> are invalid for signal purposes: a look-ahead in the Binance `metrics` join
> (5-minute rows stamped with the START of their aggregation window; the join
> treated that stamp as the observation time, so every bar could see up to
> five minutes of its own future) contaminated every fs-2/fs-3 model. Run 3
> (fs-3 + ATR exits + the Amendment 3b cost model) reported Sharpe 15–18 at
> every horizon — the number that triggered the investigation, since fold
> Sharpes of 25–42 confined to 2024–25 and negative in 2026 is not a trading
> result. Amendment 4 fixed the join (`ts_s + 300 ≤ bar_ts`); this run
> (fs-4, everything else unchanged from run 3) is the first record built on
> a causal metrics join. **Every horizon fails the 2.0 Sharpe gate: h15
> 0.7567, h30 0.6869, h60 0.5127** — all positive, all far below 2.0, on a
> matrix whose measured span (24.4 months) and costed span (now
> full-span — see "Eligibility checklist") both clear Amendments 1–3b's
> conditions. This is the record that closes the research phase under the
> protocol's FAIL branch; see "VERDICT" below.

**Run date:** 2026-08-16
**Repo:** `/home/magnus/dev/magnus/ai-trader`, branch `scalper-plan3d` (verified
with `git branch --show-current`)
**Backfill:** none performed. This run rebuilds its matrix and cost summary
from the data already on disk under `data/`, per the protocol's frozen-data
rule (§4, restated by every amendment through 3b) — no pull or backfill of
any kind since Amendment 4 was pre-registered.

---

## History: runs 1–4

Four gate runs exist under this protocol. Three are invalid for signal
purposes; this document is the record of the fourth.

| run | features | costs | exits | tradeable window | h15 / h30 / h60 sharpe | status |
|---|---|---|---|---|---|---|
| 1 | fs-2 | Amendment 1 (±0.2%-band impact) | fixed-time | ~7 months (±0.2% band starts 2026-01-15; features *and* costs both bound to it) | 6.3059 PASS / 2.0620 PASS / −2.9575 FAIL (1 fold each) | **NOT gate-eligible** — matrix span ~6.9 months against the ≥18-month bar (Amendment 1 condition 1). PASS/PASS/FAIL numbers explicitly non-authoritative. Contaminated by the metrics look-ahead (undiscovered at the time). |
| 2 | fs-3 (Amendment 2: 3 features moved off the ±0.2% band) | Amendment 1 (±0.2%-band impact, unchanged) | fixed-time | ~6.2 months (189 days, 2026-01-15..2026-07-22) — features stopped needing the band, but the *cost model* was still bound to it | −1.783482 FAIL / −1.092699 FAIL / −0.995603 FAIL (21 folds, 4–5 traded) | **Gate-eligible** (matrix span 24.4 months measured). GATE FAILED. Fee fixed-point PROVISIONAL (volume cleared every disputed VIP1 candidate). Pooled IC (0.24/0.17/0.11) implausibly high — flagged in the audit, not yet explained. Contaminated by the metrics look-ahead (undiscovered at the time). |
| 3 | fs-3 (unchanged) | Amendment 3b (±1%-band impact, m=2.3, floored — fixes run 2's cost-side band dependency) | Amendment 3 ATR stop/target (k=4, R:R 1.2) | full 24.4 months (3b removed the cost-side band dependency) | 17.88 / 16.91 / 15.04 — nominal "PASS" at every horizon | **Never written up as a doc.** The number that triggered the investigation: fold Sharpes of 25–42 confined to 2024–25 and negative in 2026 is not a trading result. Root cause: `taker_ls_ratio` (~42% of model gain) and `oi_change_60` (~11%) are read from Binance `metrics` rows whose `create_time` marks the *start* of a 5-minute window; the old join (`ts_s ≤ bar_ts`) handed each bar a metrics row that summarized five minutes of its own future. Measured on BTC 2025-03..05 (26,496 rows): taker ratio@T correlates **+0.365** with return over [T, T+5m) and −0.013 with [T+5m, T+10m). **INVALID — contaminated by the metrics look-ahead (Amendment 4's finding).** |
| **4** | **fs-4** (Amendment 4: metrics join becomes `ts_s + 300 ≤ bar_ts` — a 5-minute row is usable only after its window closes; all 38 feature *definitions* unchanged) | Amendment 3b (unchanged) | Amendment 3 ATR (unchanged) | full 24.4 months | **0.7567 FAIL / 0.6869 FAIL / 0.5127 FAIL** (21 folds, 14/16/14 traded) | **Gate-eligible. GATE FAILED at all three horizons. First uncontaminated record — this document.** |

None of runs 2–4 differ in fold schedule, universe, threshold-mult, notional,
or fee fixed-point rule — the only things that changed across them are the
cost model (Amendment 3b, run 3 onward), the exit rule (Amendment 3, run 3
onward), and the metrics join (Amendment 4, run 4 only). Run 3's numbers are
stated here, in this table, as the leaked result — per instruction, no
separate `docs/scalper-gate-run-3.md` exists; this is the only place its
numbers are on the record.

---

## Eligibility checklist (Amendments 1–4)

| # | Condition | Met? | Evidence |
|---|---|---|---|
| 1 | ≥18 months of matrix span (`training-matrix --start`/`--end`) | **MET** | Matrix streamed once (`ts`/`asset` fields only): 2024-08-01 01:05 to 2026-08-13 22:55, **24.4 months measured**, matching run 2's measured span exactly (fs-4 changes the metrics join, not row survival on the ±1% bands the features actually read). 21 of 24 assets independently clear 18 months from their own first kept row; the same three assets short of it for listing-date reasons as run 2 — PUMP (16.0mo), KAITO (17.7mo), XPL (11.6mo). See "Matrix" below for the full per-asset table. |
| 2 | Every mapped, 90-day-eligible candidate is in the matrix | Met | 24 assets in the manifest's `assets` list, all 24 with kept rows (see "Matrix" below); universe frozen and unchanged since run 1. |
| 3 | fs-4 features (`fs-rust-scalper-4`, Amendment 4) | Met | Manifest line: `"feature_set_version":"fs-rust-scalper-4"`, 38 features listed, `"horizons_min":[15,30,60]`, `"stride_min":5`. 2,446,520 data rows, 1 manifest line (2,446,521 total lines) — 0 nulls (Amendment 4 changed the metrics join's timestamp comparison, not the finiteness invariant from `5f8e77a`, which still holds). |
| 4 | Time-varying costs via `binance-costs` + `gate --binance-costs`, Amendment 3b model | Met | Every report's `binance_costs` field is `data/binance-micro/costs-daily-3b.json`. **Costed span is now full-span, unlike run 2**: `days_without_costs` totals **59** asset-days across 16 of 24 assets (max 7, on ZEC), against 744 possible days for the 17 assets with full 2024-08-01 history — essentially zero, versus run 2's 8,280 total (up to 442 per asset) under the old ±0.2%-band-bound cost model. See "Costs" below. |
| 5 | All three horizons (15/30/60) run, every run, all reported | Met | See "Per-horizon results" below. |
| — | ATR stop/target exits (Amendment 3), pre-registered k=4, R:R 1.2:1 | Met | Every report's `exit_mode` is `"atr"`; `exit_stats` present in all three (stops/targets/time_exits sum to `n_trades`; `skipped_no_atr`/`skipped_no_end_bar` both 0 at every horizon). |

**Conclusion: this run clears the eligibility bar on every condition, unconditionally** — unlike run 2, where the ~6.2-month costed window (a fact outside the eligibility test itself) meant the FAIL/FAIL/FAIL verdict rested on a fraction of the matrix span. Run 4's 24.4-month matrix span and its now-full-span costed window are the same span: **the FAIL below is a verdict over the whole out-of-sample record**, not a truncated slice of it.

---

## Commands run, in order

This section is reconstructed from the committed code (`gate`'s USAGE block,
`service/crates/scalper-data/src/main.rs`), the report/manifest contents, and
file modification times (`stat`, local time Europe/Stockholm CEST = UTC+2,
cross-checked against each report's own `generated_utc`). **No live execution
transcript or RSS sampler log survives for this run in this session** — unlike
run 2's ps-polled h15 phase, this run's per-phase peak memory is not
available and is not estimated here; that gap is stated rather than filled
with a guess.

### Step 0 — build

```
cargo build --release -p scalper-data
```

Run from `service/`. Binary timestamp `service/target/release/scalper-data`:
2026-08-16 11:44:04 +0200 — after commit `934eb4f` ("fs-4: a metrics row is
usable only after its window closes"), the Amendment 4 fix commit.

### Step 1 — Binance costs (Amendment 3b model, unchanged CLI from Amendment 1)

```
service/target/release/scalper-data binance-costs \
  --data-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --notional 5000 \
  --out data/binance-micro/costs-daily-3b.json
```

`data/binance-micro/costs-daily-3b.json` timestamp: 2026-08-16 11:26:06
+0200 (before this run's binary rebuild — Amendment 3b's `binance_costs.rs`
impact-model change and the causal-metrics-join fix are independent code
paths; the cost file did not need regenerating for the join fix and was
carried over frozen). Per-asset day entries carry `"impact_model":"b10"`,
confirming the 3b model (±1% band, m=2.3, old-model floor) is in effect, not
Amendment 1's ±0.2%-band model.

### Step 2 — training matrix (fs-4, full universe, full span)

```
service/target/release/scalper-data training-matrix \
  --data-root data --micro-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --out data/matrices/gate-run-4.jsonl --stride 5
```

`data/matrices/gate-run-4.jsonl` timestamp: 2026-08-16 12:01:03 +0200 (after
the 11:44 binary rebuild). **2,446,520 kept rows across 24 assets**
(2,446,521 lines total: 1 manifest + 2,446,520 data rows — independently
recounted with `wc -l` and a streaming per-asset pass over `ts`/`asset`,
below). Manifest confirms `feature_set_version = fs-rust-scalper-4`, 38
features, `horizons_min = [15,30,60]`, `stride_min = 5`, 0 nulls.

The command's own per-asset `kept N of M (book from <date>, flow from
<date>, metrics from <date>, funding from <date>)` line prints `book from
2026-01-15` for every asset, same as run 2. **This remains the same stale
diagnostic label documented in run 2's record** — `coverage_starts()`'s
`book` field is hard-coded to the ±0.2%-band check (`bid_02`/`ask_02`
`is_some()`), which fs-3 stopped needing and fs-4 (Amendment 4 changes only
the metrics join, not any feature's band dependency) still does not need.
It is not wired to `matrix::matrix_rows()`, which decides which rows
actually survive, and none of fs-4's 38 features read `bid_02`/`ask_02`. The
authoritative per-asset span is the one measured directly from the matrix
file's own `ts` values, in "Matrix" below.

### Step 3 — per-horizon fold fit + gate

```
uv run --with lightgbm --with numpy python walk_forward_scalper.py \
  --matrix ../data/matrices/gate-run-4.jsonl --horizon <H> \
  --out-dir ../data/models/gate-run-4-h<H>

service/target/release/scalper-data gate \
  --matrix data/matrices/gate-run-4.jsonl \
  --folds data/models/gate-run-4-h<H>/folds.json \
  --binance-costs data/binance-micro/costs-daily-3b.json \
  --fee-taker-bps 4.5 --fee-maker-bps 1.8 --notional 5000 \
  --exit atr --data-root data \
  --out data/reports/gate-run-4-h<H>.json
```

Fold fit from `training/`, gate from repo root, all three horizons run
strictly sequentially (`--horizon 15|30|60`, in that order — fold and
report file timestamps below are monotonically increasing, consistent with
a sequential run, not a parallel one). `--exit atr --data-root data` are new
relative to run 2's command: Amendment 3's ATR exit needs 1-minute bars from
the same data root to compute stop/target walks.

| horizon | fold files (`fold-0.json` → `fold-20.json`) mtimes | inferred fit window | folds.json mtime | report `generated_utc` | inferred gate runtime |
|---:|---|---|---|---|---|
| 15 | 2026-08-16 12:0?:?? → 12:07:14 +0200 | ends 12:07:14 (started sometime after the 12:01:03 matrix) | 12:08:36 +0200 | 2026-08-16T10:09:12.51Z (= 12:09:12 +0200) | ~36s (12:08:36 → 12:09:12) |
| 30 | 2026-08-16 12:0?:?? → 12:10:15 +0200 | ends 12:10:15 | 12:11:27 +0200 | 2026-08-16T10:12:02.85Z (= 12:12:02 +0200) | ~35s (12:11:27 → 12:12:02) |
| 60 | 2026-08-16 12:1?:?? → 12:13:19 +0200 | ends 12:13:19 | 12:14:58 +0200 | 2026-08-16T10:15:34.12Z (= 12:15:34 +0200) | ~36s (12:14:58 → 12:15:34) |

All three folds.json files record 21 folds (`fold-0.json`..`fold-20.json`),
matching the anchored-expanding-window default (90-day train floor, 30-day
test, 30-day step) used by every run since run 2. Gate runtimes (~35-36s
each) are consistent with run 2's h15/h30 gate runtimes (~38s, ~10s) on a
comparably-sized matrix; fit windows (fold-0 through fold-20 completing
within roughly 5-7 minutes per horizon, back-to-back across the three
horizons between 12:01 and 12:15) are consistent with run 2's ~3-6 minute
per-horizon fit times. Exit code and console output for each command are not
preserved in this session; the reports' presence, `generated_utc` fields, and
`exit_stats.skipped_no_atr`/`skipped_no_end_bar` both being 0 at every
horizon are the evidence that all three completed without error.

---

## Matrix

Per-asset kept/first/last row (streamed once from the matrix file itself,
`asset`/`ts` fields only — the same method run 2's record used):

| asset | kept | first kept row (UTC) | last kept row (UTC) | months |
|---|---:|---|---|---:|
| ETH | 203,387 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| BTC | 203,303 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| SOL | 201,085 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| XRP | 196,888 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| DOGE | 190,781 | 2024-08-01 01:05 | 2026-08-13 17:15 | 24.4 |
| kPEPE | 185,503 | 2024-08-01 01:05 | 2026-08-13 20:35 | 24.4 |
| SUI | 170,464 | 2024-08-01 15:45 | 2026-08-13 17:40 | 24.4 |
| ENA | 121,205 | 2024-08-01 01:05 | 2026-08-13 17:10 | 24.4 |
| WLD | 117,199 | 2024-08-01 01:05 | 2026-08-13 17:50 | 24.4 |
| TAO | 104,257 | 2024-08-01 15:55 | 2026-08-13 20:10 | 24.4 |
| LINK | 99,788 | 2024-08-01 05:25 | 2026-08-13 17:10 | 24.4 |
| FARTCOIN | 98,001 | 2024-12-20 20:00 | 2026-07-15 14:25 | 18.8 |
| ZEC | 86,001 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| AAVE | 83,715 | 2024-08-01 01:05 | 2026-08-13 16:55 | 24.4 |
| VIRTUAL | 69,203 | 2024-12-10 16:00 | 2026-08-12 20:15 | 20.0 |
| UNI | 53,668 | 2024-08-01 05:25 | 2026-08-13 13:10 | 24.4 |
| NEAR | 49,670 | 2024-08-01 05:25 | 2026-08-13 16:50 | 24.4 |
| ETHFI | 48,262 | 2024-08-01 15:25 | 2026-08-13 22:55 | 24.4 |
| PUMP | 45,278 | 2025-04-12 16:00 | 2026-08-13 20:50 | 16.0 |
| KAITO | 37,219 | 2025-02-20 16:00 | 2026-08-13 21:50 | 17.7 |
| XPL | 29,534 | 2025-08-22 12:00 | 2026-08-11 16:30 | 11.6 |
| CRV | 20,129 | 2024-08-01 09:20 | 2026-08-12 08:45 | 24.3 |
| LIT | 18,159 | 2024-10-21 15:25 | 2026-08-13 22:35 | 21.7 |
| XMR | 13,821 | 2024-08-02 15:15 | 2026-08-13 22:55 | 24.4 |

Total kept rows: **2,446,520** (manifest/`wc -l` both confirm 2,446,520 data rows + 1 manifest line). Below-18-month assets: PUMP (16.0), KAITO (17.7), XPL (11.6) — same three as run 2, for the same listing-date reason (their own Binance UM listing dates, not a data gap). All 24 assets' first-kept-row and last-kept-row dates are within ~15 minutes of run 2's corresponding per-asset dates (both matrices cover the same frozen underlying data), confirming Amendment 4's join fix changed which *metrics values* land on a row, not which rows survive.

---

## Costs

`data/binance-micro/costs-daily-3b.json` — Amendment 3b's ±1%-band impact
model (m=2.3, floored at Amendment 1's old ±0.2%-band model where that band
also exists). Per-asset day counts and thin days (`impact_bps: null` — the
day's ±1% band depth was itself too thin to price the $5,000 notional, or
absent):

| asset | days | thin days |
|---|---:|---:|
| AAVE | 744 | 5 |
| BTC | 744 | 0 |
| CRV | 744 | 6 |
| DOGE | 744 | 0 |
| ENA | 744 | 0 |
| ETH | 744 | 0 |
| ETHFI | 744 | 5 |
| LINK | 744 | 5 |
| NEAR | 744 | 5 |
| SOL | 744 | 1 |
| SUI | 744 | 1 |
| TAO | 744 | 6 |
| UNI | 744 | 1 |
| WLD | 744 | 6 |
| XMR | 744 | 6 |
| XRP | 744 | 5 |
| ZEC | 744 | 7 |
| kPEPE | 744 | 0 |
| VIRTUAL | 613 | 1 |
| FARTCOIN | 603 | 5 |
| KAITO | 541 | 0 |
| PUMP | 464 | 1 |
| LIT | 419 | 0 |
| XPL | 358 | 1 |

Thin days now run 0-7 per asset (max: ZEC 7) against 358-744 possible days
— a different, much smaller quantity than run 2's 146-601 thin days per
asset under the ±0.2%-band model, because the ±1% band (unlike the ±0.2%
one) exists across the full ingested history.

`days_without_costs` (from the gate report — the entry-day-or-14-day-lookback
cost lookup failing entirely; identical across all three horizon reports,
a matrix/cost-file property, not horizon-dependent) totals **59** across
16 of 24 assets, max 7 (ZEC):

| asset | days_without_costs |
|---|---:|
| ZEC | 7 |
| TAO | 6 |
| AAVE | 5 |
| ETHFI | 5 |
| FARTCOIN | 5 |
| LINK | 5 |
| NEAR | 5 |
| WLD | 5 |
| XRP | 5 |
| CRV | 4 |
| XMR | 2 |
| PUMP | 1 |
| SOL | 1 |
| SUI | 1 |
| UNI | 1 |
| VIRTUAL | 1 |

Against run 2's **8,280** total (up to 442 per asset, every OOS day before
2026-01-15 for the seven assets with full history), this is the direct
confirmation that the costed/tradeable window is now the same ~24.4-month
span as the matrix, not the ~6.2-month slice run 2's cost-side band
dependency left tradeable. The out-of-sample window below (631 days,
fold-0's test start 2024-10-30 through fold-20's test end 2026-07-22) is now
genuinely tradeable across essentially all of it, subject only to whether a
given prediction clears the entry threshold.

---

## Per-horizon results

| horizon | n_trades | sharpe_annualized | gate | ic (pooled) | rank_ic (pooled) | folds traded/21 | positive-sharpe folds | zero-return days/631 | exits stops/targets/time | net total bps | net bps/trade | hit rate |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---|---:|---:|---:|
| 15 | 4,144 | 0.7567 | FAIL | 0.0366 | 0.0144 | 14/21 | 7 | 404/631 | 464/358/3,322 | +124,882.21 | +30.14 | 49.35% |
| 30 | 5,892 | 0.6869 | FAIL | 0.0216 | 0.0046 | 16/21 | 8 | 358/631 | 1,422/1,036/3,434 | +98,816.81 | +16.77 | 48.69% |
| 60 | 5,648 | 0.5127 | FAIL | 0.0148 | 0.0002 | 14/21 | 7 | 407/631 | 2,074/1,584/1,990 | +59,938.86 | +10.61 | 47.17% |

Out-of-sample window: **631 days** (fold-0's test start 2024-10-30 through
fold-20's test end 2026-07-22), identical across horizons since the fold
schedule doesn't depend on horizon — the same 631-day window run 2 reported,
now genuinely tradeable across nearly all of it (see "Costs" above), not
truncated to a 189-day slice. `excluded_thin_books` is `[]` at all three
horizons. Mean bars held under the ATR exit: h15 13.68 bars, h30 23.73
bars, h60 37.59 bars (`exit_stats.mean_bars_held`) — well under each
horizon's own H-minute cap, confirming stops/targets are firing before the
time-exit fallback for a large share of trades, most visibly at h60 (stops +
targets = 3,658 of 5,648 trades, 64.8%, vs. only 1,990 time exits).
`n_preds` totals **2,143,834** at every horizon (same folds, same rows;
horizon only changes the forward-return target and which rows clear the
entry threshold) — essentially identical to run 2's 2,143,846 (a 12-row
difference plausibly from Amendment 4's finiteness/join edge cases, not
investigated further since it is immaterial to any figure above).

---

## Fold-level detail

`ic`/`rank_ic` are each fold's own model (a fresh LightGBM fit per fold,
walk-forward — not the pooled `overall.ic` above). `pred_abs p50/p90/p99`
are that fold's predicted |return| percentiles, in bps; the entry rule
requires `|pred| > 1.5 × round_trip_bps` (round trip ≈ 11-46bps by asset,
`threshold_bps_by_asset` above), so a fold whose p99 sits under roughly
15-20bps trades rarely or not at all regardless of direction skill.

### h15

| fold | test window | n_trades | sharpe | ic | rank_ic | n_preds | pred_abs p50 | p90 | p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2024-10-30..2024-11-29 | 228 | 3.6092 | 0.0275 | 0.0349 | 107,044 | 0.697 | 4.048 | 10.357 |
| 1 | 2024-11-29..2024-12-29 | 23 | 2.7781 | 0.0079 | 0.0056 | 133,017 | 0.584 | 3.017 | 7.771 |
| 2 | 2024-12-29..2025-01-28 | 770 | 2.3214 | -0.0005 | -0.0026 | 118,772 | 0.694 | 4.480 | 17.938 |
| 3 | 2025-01-28..2025-02-27 | 0 | — | 0.0408 | 0.0027 | 118,138 | 0.381 | 0.803 | 3.159 |
| 4 | 2025-02-27..2025-03-29 | 673 | -6.2118 | -0.0050 | 0.0040 | 108,695 | 0.660 | 3.755 | 15.994 |
| 5 | 2025-03-29..2025-04-28 | 0 | — | 0.0112 | 0.0169 | 99,330 | 0.022 | 0.241 | 1.447 |
| 6 | 2025-04-28..2025-05-28 | 12 | -2.0652 | 0.0025 | 0.0174 | 120,882 | 0.169 | 0.564 | 3.281 |
| 7 | 2025-05-28..2025-06-27 | 85 | -1.7519 | 0.0154 | 0.0278 | 116,504 | 0.297 | 1.019 | 4.625 |
| 8 | 2025-06-27..2025-07-27 | 0 | — | 0.0248 | 0.0178 | 110,220 | 0.102 | 0.378 | 1.236 |
| 9 | 2025-07-27..2025-08-26 | 151 | 1.0170 | 0.0365 | 0.0303 | 114,737 | 0.421 | 1.435 | 6.574 |
| 10 | 2025-08-26..2025-09-25 | 254 | -3.3765 | 0.0371 | 0.0393 | 107,160 | 0.472 | 2.021 | 10.820 |
| 11 | 2025-09-25..2025-10-25 | 906 | 3.2191 | 0.0957 | 0.0249 | 123,659 | 0.799 | 4.905 | 23.730 |
| 12 | 2025-10-25..2025-11-24 | 850 | 7.1835 | 0.0384 | 0.0135 | 122,715 | 0.792 | 4.297 | 16.975 |
| 13 | 2025-11-24..2025-12-24 | 62 | -0.3495 | 0.0004 | 0.0232 | 93,572 | 0.338 | 1.614 | 6.172 |
| 14 | 2025-12-24..2026-01-23 | 84 | -3.6192 | -0.0372 | 0.0146 | 87,092 | 0.399 | 1.518 | 7.171 |
| 15 | 2026-01-23..2026-02-22 | 0 | — | 0.0688 | 0.0339 | 86,132 | 0.061 | 0.184 | 1.024 |
| 16 | 2026-02-22..2026-03-24 | 0 | — | 0.0035 | 0.0196 | 70,242 | 0.108 | 0.220 | 0.917 |
| 17 | 2026-03-24..2026-04-23 | 0 | — | 0.0402 | 0.0313 | 59,272 | 0.120 | 0.367 | 1.244 |
| 18 | 2026-04-23..2026-05-23 | 0 | — | 0.0408 | 0.0480 | 66,936 | 0.182 | 0.363 | 1.224 |
| 19 | 2026-05-23..2026-06-22 | 10 | -1.7742 | 0.0209 | 0.0220 | 98,228 | 0.296 | 0.966 | 4.399 |
| 20 | 2026-06-22..2026-07-22 | 36 | 2.2083 | 0.0117 | 0.0180 | 81,487 | 0.448 | 1.298 | 5.148 |

### h30

| fold | test window | n_trades | sharpe | ic | rank_ic | n_preds | pred_abs p50 | p90 | p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2024-10-30..2024-11-29 | 0 | — | 0.0179 | 0.0118 | 107,044 | 0.468 | 1.214 | 2.574 |
| 1 | 2024-11-29..2024-12-29 | 1,989 | -5.7353 | 0.0061 | 0.0028 | 133,017 | 1.991 | 10.690 | 25.659 |
| 2 | 2024-12-29..2025-01-28 | 1,188 | -2.5431 | -0.0055 | -0.0050 | 118,772 | 1.363 | 7.455 | 28.680 |
| 3 | 2025-01-28..2025-02-27 | 0 | — | 0.0215 | 0.0024 | 118,138 | 0.735 | 1.793 | 5.553 |
| 4 | 2025-02-27..2025-03-29 | 4 | -0.5830 | -0.0108 | 0.0057 | 108,695 | 0.313 | 1.433 | 5.660 |
| 5 | 2025-03-29..2025-04-28 | 0 | — | -0.0088 | -0.0009 | 99,330 | 0.111 | 0.502 | 2.849 |
| 6 | 2025-04-28..2025-05-28 | 32 | 8.9634 | 0.0235 | 0.0291 | 120,882 | 0.305 | 1.303 | 4.932 |
| 7 | 2025-05-28..2025-06-27 | 24 | 4.2303 | -0.0280 | -0.0196 | 116,504 | 0.277 | 0.920 | 3.174 |
| 8 | 2025-06-27..2025-07-27 | 0 | — | 0.0119 | 0.0089 | 110,220 | 0.248 | 0.471 | 1.317 |
| 9 | 2025-07-27..2025-08-26 | 57 | -2.1580 | -0.0022 | 0.0103 | 114,737 | 0.413 | 1.688 | 6.126 |
| 10 | 2025-08-26..2025-09-25 | 228 | -4.0012 | 0.0147 | 0.0343 | 107,160 | 0.728 | 2.202 | 12.390 |
| 11 | 2025-09-25..2025-10-25 | 634 | 3.2229 | 0.0781 | 0.0040 | 123,659 | 0.788 | 4.473 | 22.614 |
| 12 | 2025-10-25..2025-11-24 | 1,231 | 5.3465 | 0.0462 | 0.0111 | 122,715 | 1.223 | 6.099 | 23.514 |
| 13 | 2025-11-24..2025-12-24 | 16 | 6.1084 | 0.0034 | 0.0073 | 93,572 | 0.259 | 1.396 | 5.707 |
| 14 | 2025-12-24..2026-01-23 | 249 | -5.0130 | -0.0205 | 0.0075 | 87,092 | 0.732 | 3.040 | 13.637 |
| 15 | 2026-01-23..2026-02-22 | 0 | — | 0.0855 | 0.0390 | 86,132 | 0.152 | 0.405 | 1.050 |
| 16 | 2026-02-22..2026-03-24 | 50 | 3.9028 | 0.0215 | 0.0018 | 70,242 | 0.538 | 1.728 | 6.289 |
| 17 | 2026-03-24..2026-04-23 | 15 | -1.8034 | -0.0346 | -0.0213 | 59,272 | 0.444 | 1.193 | 3.976 |
| 18 | 2026-04-23..2026-05-23 | 59 | -3.1101 | 0.0337 | 0.0054 | 66,936 | 0.744 | 2.243 | 7.449 |
| 19 | 2026-05-23..2026-06-22 | 1 | 3.4314 | 0.0177 | 0.0037 | 98,228 | 0.386 | 0.812 | 3.844 |
| 20 | 2026-06-22..2026-07-22 | 115 | 3.5532 | 0.0425 | 0.0254 | 81,487 | 0.750 | 2.751 | 9.167 |

### h60

| fold | test window | n_trades | sharpe | ic | rank_ic | n_preds | pred_abs p50 | p90 | p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2024-10-30..2024-11-29 | 0 | — | 0.0078 | -0.0081 | 107,044 | 1.247 | 2.900 | 5.001 |
| 1 | 2024-11-29..2024-12-29 | 1,979 | -4.6878 | 0.0088 | -0.0009 | 133,017 | 3.608 | 12.876 | 26.445 |
| 2 | 2024-12-29..2025-01-28 | 347 | 0.6742 | -0.0144 | -0.0368 | 118,772 | 2.611 | 5.942 | 18.777 |
| 3 | 2025-01-28..2025-02-27 | 0 | — | 0.0181 | 0.0101 | 118,138 | 1.733 | 2.373 | 6.322 |
| 4 | 2025-02-27..2025-03-29 | 0 | — | 0.0140 | 0.0055 | 108,695 | 0.742 | 1.439 | 4.658 |
| 5 | 2025-03-29..2025-04-28 | 0 | — | -0.0073 | 0.0085 | 99,330 | 0.289 | 0.939 | 4.069 |
| 6 | 2025-04-28..2025-05-28 | 18 | 9.3337 | 0.0170 | 0.0152 | 120,882 | 0.494 | 1.621 | 5.258 |
| 7 | 2025-05-28..2025-06-27 | 0 | — | -0.0322 | -0.0301 | 116,504 | 0.383 | 0.906 | 3.199 |
| 8 | 2025-06-27..2025-07-27 | 0 | — | -0.0028 | -0.0006 | 110,220 | 0.647 | 0.667 | 2.364 |
| 9 | 2025-07-27..2025-08-26 | 17 | 2.0301 | 0.0226 | 0.0052 | 114,737 | 0.271 | 1.097 | 4.515 |
| 10 | 2025-08-26..2025-09-25 | 123 | -1.6497 | 0.0257 | 0.0496 | 107,160 | 0.842 | 2.126 | 11.071 |
| 11 | 2025-09-25..2025-10-25 | 371 | 3.5052 | 0.0530 | 0.0008 | 123,659 | 1.207 | 3.666 | 19.485 |
| 12 | 2025-10-25..2025-11-24 | 2,229 | -1.2631 | 0.0285 | 0.0166 | 122,715 | 3.048 | 11.270 | 41.440 |
| 13 | 2025-11-24..2025-12-24 | 1 | 3.4314 | -0.0029 | -0.0111 | 93,572 | 0.098 | 0.459 | 3.930 |
| 14 | 2025-12-24..2026-01-23 | 363 | -3.0162 | 0.0142 | 0.0123 | 87,092 | 1.655 | 5.793 | 25.625 |
| 15 | 2026-01-23..2026-02-22 | 19 | 3.9745 | 0.1352 | 0.0182 | 86,132 | 0.466 | 1.446 | 5.122 |
| 16 | 2026-02-22..2026-03-24 | 43 | -1.2465 | 0.0133 | 0.0246 | 70,242 | 0.738 | 2.288 | 7.321 |
| 17 | 2026-03-24..2026-04-23 | 61 | -3.1122 | -0.0553 | -0.0589 | 59,272 | 0.994 | 2.815 | 9.784 |
| 18 | 2026-04-23..2026-05-23 | 55 | -0.3695 | -0.0039 | -0.0057 | 66,936 | 1.048 | 3.155 | 10.022 |
| 19 | 2026-05-23..2026-06-22 | 0 | — | 0.0269 | -0.0281 | 98,228 | 0.609 | 1.011 | 3.338 |
| 20 | 2026-06-22..2026-07-22 | 22 | 3.6033 | 0.0430 | 0.0028 | 81,487 | 1.035 | 2.743 | 9.483 |

---

## Per-asset breakdown

`days_without_costs` per-asset totals are in "Costs" above (identical
across horizons).

### h15 (24 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| XPL | 390 | -5,156.81 | 0.472 |
| ETH | 93 | -1,657.66 | 0.333 |
| LIT | 8 | -621.83 | 0.375 |
| KAITO | 122 | -543.77 | 0.443 |
| BTC | 49 | +197.05 | 0.449 |
| CRV | 7 | +290.09 | 0.714 |
| XMR | 14 | +515.03 | 0.500 |
| ZEC | 436 | +531.78 | 0.525 |
| NEAR | 51 | +789.83 | 0.529 |
| SOL | 140 | +955.95 | 0.500 |
| WLD | 191 | +1,230.58 | 0.492 |
| TAO | 138 | +1,377.38 | 0.449 |
| ETHFI | 97 | +4,882.31 | 0.577 |
| XRP | 170 | +5,207.70 | 0.506 |
| DOGE | 250 | +5,904.15 | 0.484 |
| LINK | 139 | +7,818.83 | 0.432 |
| AAVE | 113 | +11,024.56 | 0.584 |
| UNI | 120 | +11,168.65 | 0.508 |
| PUMP | 167 | +11,885.13 | 0.533 |
| FARTCOIN | 640 | +11,976.80 | 0.461 |
| ENA | 185 | +12,112.22 | 0.530 |
| kPEPE | 262 | +12,124.02 | 0.519 |
| VIRTUAL | 201 | +13,442.63 | 0.507 |
| SUI | 161 | +19,427.58 | 0.540 |

Gain side (20 assets net positive, +132,862.28bps total): top-3 = **SUI** (+19,427.58, 14.6% of gains), **VIRTUAL** (+13,442.63, 10.1% of gains), **kPEPE** (+12,124.02, 9.1% of gains). Loss side (4 assets net negative, -7,980.06bps total): top-3 = **XPL** (-5,156.81, 64.6% of losses), **ETH** (-1,657.66, 20.8% of losses), **LIT** (-621.83, 7.8% of losses). Net total: +124,882.21bps.

### h30 (24 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| XPL | 322 | -4,550.99 | 0.472 |
| WLD | 348 | -2,629.84 | 0.486 |
| XMR | 49 | -990.91 | 0.449 |
| LIT | 40 | -829.80 | 0.450 |
| TAO | 238 | -769.42 | 0.458 |
| CRV | 46 | -390.43 | 0.522 |
| NEAR | 152 | +272.84 | 0.461 |
| BTC | 95 | +281.74 | 0.484 |
| SOL | 195 | +637.79 | 0.482 |
| ETH | 122 | +2,917.98 | 0.590 |
| ETHFI | 189 | +3,387.56 | 0.497 |
| KAITO | 71 | +3,498.95 | 0.535 |
| LINK | 300 | +3,547.44 | 0.463 |
| kPEPE | 362 | +3,733.17 | 0.475 |
| PUMP | 160 | +4,151.11 | 0.450 |
| VIRTUAL | 361 | +4,529.76 | 0.454 |
| ZEC | 504 | +5,137.48 | 0.532 |
| XRP | 316 | +5,828.01 | 0.491 |
| DOGE | 277 | +6,381.01 | 0.487 |
| UNI | 242 | +8,244.15 | 0.488 |
| AAVE | 263 | +8,767.41 | 0.471 |
| ENA | 332 | +9,223.60 | 0.506 |
| SUI | 288 | +16,881.47 | 0.483 |
| FARTCOIN | 620 | +21,556.70 | 0.495 |

Gain side (18 assets net positive, +108,978.19bps total): top-3 = **FARTCOIN** (+21,556.70, 19.8% of gains), **SUI** (+16,881.47, 15.5% of gains), **ENA** (+9,223.60, 8.5% of gains). Loss side (6 assets net negative, -10,161.39bps total): top-3 = **XPL** (-4,550.99, 44.8% of losses), **WLD** (-2,629.84, 25.9% of losses), **XMR** (-990.91, 9.8% of losses). Net total: +98,816.81bps.

### h60 (24 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| ZEC | 737 | -7,793.99 | 0.472 |
| XMR | 100 | -3,281.49 | 0.490 |
| WLD | 275 | -2,032.15 | 0.440 |
| ETHFI | 206 | -1,786.11 | 0.461 |
| CRV | 17 | -1,377.30 | 0.412 |
| TAO | 254 | -1,027.65 | 0.469 |
| UNI | 245 | -224.42 | 0.424 |
| NEAR | 158 | +140.81 | 0.462 |
| BTC | 59 | +452.70 | 0.525 |
| LIT | 82 | +552.62 | 0.512 |
| KAITO | 118 | +797.86 | 0.508 |
| kPEPE | 350 | +846.78 | 0.471 |
| LINK | 273 | +1,618.63 | 0.462 |
| SOL | 160 | +1,790.10 | 0.500 |
| ETH | 109 | +2,686.20 | 0.578 |
| XPL | 271 | +3,220.84 | 0.469 |
| XRP | 290 | +3,421.71 | 0.500 |
| DOGE | 232 | +4,183.50 | 0.478 |
| AAVE | 261 | +4,437.85 | 0.441 |
| PUMP | 190 | +6,148.22 | 0.437 |
| ENA | 251 | +7,362.13 | 0.494 |
| SUI | 265 | +10,242.88 | 0.445 |
| FARTCOIN | 358 | +13,614.59 | 0.466 |
| VIRTUAL | 387 | +15,944.56 | 0.494 |

Gain side (17 assets net positive, +77,461.97bps total): top-3 = **VIRTUAL** (+15,944.56, 20.6% of gains), **FARTCOIN** (+13,614.59, 17.6% of gains), **SUI** (+10,242.88, 13.2% of gains). Loss side (7 assets net negative, -17,523.12bps total): top-3 = **ZEC** (-7,793.99, 44.5% of losses), **XMR** (-3,281.49, 18.7% of losses), **WLD** (-2,032.15, 11.6% of losses). Net total: +59,938.86bps.

---

## Fee fixed-point step (Amendment 1, unchanged by Amendments 2-4)

Run 4 charges VIP0 + BNB-discount taker/maker (4.50/1.80 bps) as
pre-registered. `overall.projected_30d_volume_usd` from the three reports:

| horizon | projected_30d_volume_usd |
|---:|---:|
| 15 | 1,970,206.02 |
| 30 | 2,801,267.83 |
| 60 | 2,685,261.49 |

**These figures are markedly smaller than run 2's** ($4.86M-$11.19M
smoothed): $1.97M (h15), $2.80M (h30), $2.69M (h60) — roughly **4-5x
smaller** per matching horizon, because run 4 trades far fewer times than
run 3's leaked numbers would have and the entry threshold now rarely clears
(see "What the diagnostics say" below), not because the notional or fee
model changed (both are fixed at $5,000 and 4.5/1.8bps as pre-registered).

Mapping to a tier using `docs/binance-um-fee-table-2026-08.md` **by the
volume criterion only**, per the pre-registered rule: **VIP0** has no
stated volume floor and **VIP9** needs ≥$30,000,000,000 30-day volume
(cited, not verified) — none of this run's figures come close to VIP9 on
any reading. The snapshot's "Gap" section leaves VIP1-VIP8 unresolved,
with four disagreeing, uncorroborated secondary-source candidate floors for
VIP1 alone: **$250k, $1M, $5M, $15M** (one flagged by the snapshot itself as
likely contamination, none Binance-authored).

**Applying the same volume-vs-candidates test to check whether VIP0 can be
read as clearly correct — map to VIP0 only if every
horizon's volume is under every one of those four candidates — this run
does NOT clear it.** All three horizons' figures exceed the two lowest
candidates ($250k by 7.9-11.2x, $1M by 1.97-2.80x) while sitting under the
two highest ($5M, $15M). Unlike run 2, whose realized volume cleared *all
four* candidates (making "VIP0 is implausible" a one-sided argument), run
4's volume sits **inside** the disputed band itself on every horizon —
neither confidently below it (ruling in VIP0) nor confidently above it
(ruling in some higher tier). No reading of these unverified candidate
figures resolves the tier either way.

Per the pre-registered rule: *"If the mapped tier differs from VIP0, run
exactly ONE re-run at that tier's fees... The second run's verdict is the
gate verdict... If the fee-table snapshot doesn't cover the mapped tier...
re-fetch Binance's authenticated fee schedule before doing the one allowed
re-run; don't substitute an unverified number."* Since this run's volume
cannot be confidently mapped to VIP0 by the candidate thresholds available,
and no VIP1-8 fee is invented to test at:

**Outcome: STOPPED. Verdict marked PROVISIONAL pending an authenticated fee
fetch**, same disposition as run 2, though for a different reason (run 2's
volume was unambiguously above the disputed band; run 4's sits inside it).
As a numeric observation only, not a substitute for the required re-run:
VIP1+ fees can only be lower than the VIP0 rate charged here, and every
horizon already fails by 1.24-1.49 points at the higher VIP0 rate (see
"VERDICT" below) — a cheaper fee schedule would move the exact Sharpe
figures upward but is very unlikely to flip FAIL/FAIL/FAIL to a 2.0 clear
given the current margin, though that expectation is not itself a
substitute for the required re-run.

---

## Drop-rule candidates (report only — not exercised)

Amendment 1's one-allowed-drop rule requires negative `total_net_bps` in
**all three** horizon reports before a symbol is eligible to be excluded.
**Zero of 24 assets meet that bar this run.** Every asset is net
positive in at least one of the three horizon reports — a materially
different fact from run 2, where 13 of 24 assets were negative in all
three. The closest cases (negative in two of three horizons) are shown for
reference, not as candidates:

| asset | h15 | h30 | h60 |
|---|---:|---:|---:|
| XPL | -5,156.81 | -4,550.99 | +3,220.84 |
| XMR | +515.03 | -990.91 | -3,281.49 |
| WLD | +1,230.58 | -2,629.84 | -2,032.15 |
| CRV | +290.09 | -390.43 | -1,377.30 |
| LIT | -621.83 | -829.80 | +552.62 |
| TAO | +1,377.38 | -769.42 | -1,027.65 |

No symbol qualifies for the drop-and-rerun step; per Amendment 1 that step
is not exercised here, consistent with prior runs, and would not have
changed the VERDICT below regardless (see next section).

---

## VERDICT

**h15: FAIL** (sharpe_annualized 0.756750, 1.24 points below the 2.0 gate threshold).
**h30: FAIL** (sharpe_annualized 0.686916, 1.31 points below the 2.0 gate threshold).
**h60: FAIL** (sharpe_annualized 0.512679, 1.49 points below the 2.0 gate threshold).

**Overall gate outcome: GATE FAILED at all three horizons.** This run is
eligible under every one of Amendments 1-4's conditions without
qualification (see "Eligibility checklist" above) — the matrix's measured
span reaches 24.4 months, the cost model's tradeable span now matches it,
fs-4's causal metrics join replaces the contaminated fs-2/fs-3 join, and
all three horizons ran under Amendment 3's pre-registered ATR exits. Unlike
run 2, whose FAIL/FAIL/FAIL verdict rested on a 6.2-month costed slice of a
24.4-month matrix, and unlike run 3, whose apparent "PASS" numbers were the
contaminated result Amendment 4 exists to correct, **this run's
FAIL/FAIL/FAIL is a verdict over the full out-of-sample record, on the
first uncontaminated feature set.** The fee fixed-point step is separately
**PROVISIONAL, not final** (see above): the volume-mapped tier is not
resolved by the available candidate thresholds. As a numeric observation
only, not a substitute for that required re-run: VIP1+ fees can only be
lower than the VIP0 rate charged here, and every horizon already fails by
1.24-1.49 points at the higher VIP0 rate.

Per Amendment 1: *"A FAIL on Binance is not grounds to buy HL data hoping
the other venue does better — §5's FAIL handling (stop, or return to
feature research under a new `FEATURE_SET_VERSION`) applies exactly as
written, venue included."* And per §5, restated by every amendment through
4 without change: *"A FAIL — on either the first run or the one allowed
re-run — means the project stops or returns to feature research. Not: lower
the 2.0 threshold. Not: keep adding horizons until one clears. Not: shrink
the notional until costs look better. Not: re-run with a different fold
schedule until a favorable window turns up. 'Returns to feature research'
means new signal work under a new `FEATURE_SET_VERSION`, followed by this
exact protocol run again from a fresh first run — not a second attempt at
massaging the same features past the same gate."*

**This document is the record that closes the research phase under that
FAIL branch.** No parameter, threshold, exit, cost, or fold-schedule change
is proposed here or authorized by this result — the protocol's own text,
quoted above, is what governs what happens next.

---

## What the diagnostics say

**Pooled IC is weak-but-real at h15 and collapses toward zero by h60.**
`overall.ic`/`rank_ic`: h15 0.0366/0.0144, h30 0.0216/0.0046, h60
0.0148/0.0002 — an order of magnitude smaller than run 2's contaminated
0.24/0.17/0.11 pooled figures, and smaller still than run 3's leaked
25-42-per-fold Sharpes implied. This is a genuine, if small, ranking skill
at the shortest horizon that erodes as the horizon lengthens, not the
implausible near-0.3 IC the metrics look-ahead produced.

**Net per-trade is positive but small, and shrinks with horizon.** Mean
net/trade: h15 +30.14bps, h30 +16.77bps, h60 +10.61bps, against a
trade-weighted round-trip cost of roughly 13-14bps at every horizon
(`threshold_bps_by_asset / 1.5`, weighted by each asset's n_trades) — h15
+43.90bps implied gross/trade, h30 +30.03bps, h60 +24.12bps. Hit rates run
below 50% at every horizon (49.35%, 48.69%, 47.17%), so the positive
net/trade is a right-skewed distribution (a minority of larger winners
funding a majority of smaller losers), the same shape run 2 found in its
(then cost-negative) per-trade arithmetic — here it clears cost on average,
just not by enough to clear a 2.0 annualized Sharpe against day-to-day
variance.

**Entries are rare because honest predictions rarely clear the cost
threshold.** The entry rule requires `|pred| > 1.5 × round_trip_bps`
(round trip ≈ 11-46bps by asset). Fold-level `pred_abs_p50` runs well under
1bp in most folds at every horizon (h15 folds 5-9, 15-18: 0.02-0.42bps
median |pred|; h30/h60 similar), and even `pred_abs_p99` — the top 1% of
predictions in a fold — frequently sits under 10bps (h15 folds 5, 8, 15-18
all p99 < 1.5bps; h60's p99 reaches into the teens/twenties in more folds,
consistent with h60 trading more total predictions into fewer, larger-move
opportunities). Only 4,144-5,892 of 2,143,834 pooled predictions (0.19%-
0.27%) ever clear their asset's threshold, across all three horizons
combined with zero-return days running 358-407 of 631 (57%-64%).

**ATR exits are mechanically most active at h60, where Sharpe is lowest.**
Stops+targets exceed time exits only at h60 (3,658 of 5,648 trades, 64.8%,
vs. 1,990 time exits) — at h15 and h30 the time exit still dominates (h15:
3,322 of 4,144, 80.2%; h30: 3,434 of 5,892, 58.3%). Mean bars held: h15
13.68, h30 23.73, h60 37.59 — all under each horizon's own bar cap (15, 30,
60), most sharply at h60 (37.59 of 60, 62.6% of the window). The stop/
target mechanism is doing the most work exactly where the annualized Sharpe
is lowest (0.5127) and the pooled rank IC is closest to zero (0.0002) — an
exit rule cannot manufacture ranking skill a fold's model didn't have; it
can only change how a given directional call gets realized.
