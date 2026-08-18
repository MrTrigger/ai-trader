# Crypto scalper — gate run 5 (Program 2: fs-5 tick order flow + maker entry, Amendment 5)

> **Bottom line up front: this run IS gate-eligible under Amendments 1–5, and
> it FAILS the gate at all three horizons — h15 Sharpe 0.3649, h30
> 0.1883, h60 0.4321** — on the first full run, and the
> §5 one-allowed-drop re-run (four assets negative at every horizon, exercised
> for the first time in the project's history — see "Drop-rule re-run" below)
> also FAILS at all three horizons — h15 0.1508, h30 0.1331, h60 -0.2510.
> Program 2 asked two questions and got two answers: **the twelve tick
> order-flow features add no measurable ranking skill** (pooled IC h15
> 0.0368 vs run 4's 0.0366; h30 0.0105 vs 0.0216; h60 0.0133 vs
> 0.0148; 8.1% / 4.5% / 3.1% of model gain), and **the maker
> entry, priced honestly, does not rescue the economics**: the strict
> trade-through fill rule fills 89.3% of orders (h15) precisely because
> price has just moved against the signal, and net-per-trade falls from run
> 4's +30.14 bps (taker) to +5.71 bps (maker) even though the round trip
> is ~40% cheaper. Under Amendment 5 §5.5 the FAIL branch is invoked; no
> window, latency, rest time, fill rule, exit parameter, feature or cost
> term is revisited against these numbers. See "VERDICT".

**Run date:** 2026-08-18
**Repo:** `/home/magnus/dev/magnus/ai-trader`, branch `main`
**Protocol:** `docs/scalper-research.md` §1–6, Amendments 1–4, Amendment 5
(`2876108`, clarified `9a15f56`, `93be7c6`, `4ec8098`) — all committed before
any number below existed.
**Code:** tape store `863c129`, fs-5 `2b322c7`, maker gate `0d22833`.
**Backfill:** the raw aggTrades tape only (§5.1: `pull-binance-tape`,
`--start 2024-08-01 --end 2026-08-15`, 16,390 day files, 1,466 404 days,
6,869,720,090 trades, 33 GB, manifest at `data/binance-micro/tape/manifest.json`).
Every other store — `data/perp`, `data/binance-micro/{book,flow,metrics,funding}`,
`data/scalper-universe.json`, `data/binance-micro/costs-daily-3b.json` — is
byte-for-byte what run 4 used; no pull, backfill or universe refresh of any
other kind. The 404 days are pre-listing days (FARTCOIN first tape day
2024-12-20, VIRTUAL 2024-12-10, KAITO 2025-02-20, PUMP 2025-04-12, XPL
2025-08-22) plus LIT's mid-span gaps (325 days without a published archive
between 2024-08-01 and 2026-08-14); the manifest lists every one.

---

## What this run is

Program 1 (fs-1 … fs-4, taker entry) closed on run 4's FAIL. Amendment 5
opened Program 2 with two pre-registered changes and nothing else:

1. **fs-5** = fs-4's 38 features, byte-for-byte on the same code path, plus
   twelve tick order-flow features computed from the raw aggTrades tape in
   windows strictly before each bar's close: `tk_imb_10s`, `tk_imb_30s`,
   `tk_imb_5m`, `tk_large_imb_5m`, `tk_run`, `tk_ret_10s`, `tk_ret_30s`,
   `tk_vwap_dev_30s`, `tk_intensity_10s`, `tk_notional_ratio_30s`,
   `tk_impact_5m`, `tk_size_med_5m_log` (definitions: Amendment 5 §5.2;
   implementation `features-scalper/src/tick.rs`).
2. **Maker entry** (`gate --entry maker`): a post-only limit at the signal
   bar's close, resting `[C+1 s, C+60 s)`, filled only if a tape trade prints
   *strictly through* the price (a print AT our price never fills — no
   queue-priority claim), fill minute resolved on the tape from the fill
   trade onward, later bars OHLC exactly as Amendment 3 (`k=4`, R:R 1.2,
   stop-wins-ties, time exit at `C + H·60`), all exits taker. Round trip
   `fee_maker + fee_taker + spread_p75/2 + impact` (1.80 + 4.50 bps VIP0+BNB,
   Amendment 3b impact) instead of the taker path's
   `2·fee_taker + spread_p75 + 2·impact`.

Unchanged from run 4: venue, universe (24 mapped assets), matrix span and
stride, fold schedule (anchored expanding, 90/30/30, `MIN_ROWS = 50,000`),
horizons, label, `--threshold-mult 1.5`, `--notional 5000`, gate = OOS
annualized Sharpe > 2.0 on daily net returns, drop rule, fee fixed-point.

The taker path is untouched: with the maker-capable binary,
`gate --exit atr` on run 4's matrix/folds/costs reproduces
`data/reports/gate-run-4-h15.json` byte-for-byte (except `generated_utc`)
— checked before and after step 3 landed. fs-5 with no tape reproduces
run 4's first 38 features exactly (asserted in `features-scalper` tests and
checked on real ZEC rows against `gate-run-4.jsonl`).

---

## Eligibility checklist (Amendments 1–5)

| # | condition | status | evidence |
|---|---|---|---|
| 1 | ≥18 months of matrix span | **MET** | Matrix streamed once (`ts`/`asset` fields): 2024-08-01 to 2026-08-13, **24.4 months measured** — identical to run 4's span. 21 of 24 assets independently clear 18 months from their own first kept row; the same three short of it for listing-date reasons as runs 2 and 4 — PUMP (16.0 mo), KAITO (17.7 mo), XPL (11.6 mo). Per-asset table under "Matrix". |
| 2 | Every mapped, 90-day-eligible candidate in the matrix | **MET** | 24 of 24 mapped assets have rows; `training-matrix` printed no "skipped (no bars)" line. |
| 3 | fs-5 end to end | **MET** | Matrix manifest `feature_set_version: fs-rust-scalper-5`, 50 features; every fold artifact carries the same version (the gate's cross-check refuses anything else). |
| 4 | Time-varying costs via `--binance-costs` | **MET** | `data/binance-micro/costs-daily-3b.json`, frozen from run 4; maker round trip per Amendment 5 §5.4. |
| 5 | All three horizons, every run | **MET** | 15/30/60 fitted and gated in that order by one driver script; all three reported below. |
| 6 | Tape pulled for the §5.1 span before the matrix, other stores frozen | **MET** | Manifest: every symbol's last stored day is 2026-08-14 (the flow store's last day); no other store touched (git status clean, `data/` mtimes unchanged). |
| 7 | `--entry maker --exit atr`, `--tape-root`, `--universe` | **MET** | Recorded in each report (`overall.entry_mode = "maker"`, `overall.exit_mode = "atr"`, `tape_root`). |

Costed span: `days_without_costs` (per asset, calendar days among its
predictions with no resolvable round trip after the 14-day lookback) is
small and identical across horizons — AAVE 5, CRV 4, ETHFI 5, FARTCOIN 5, LINK 5, NEAR 5, PUMP 1, SOL 1, SUI 1, TAO 6, UNI 1, VIRTUAL 1, WLD 5, XMR 2, XRP 5, ZEC 7 —
i.e. the maker cost path is tradeable across the full OOS window
(631 days, fold-0's test start 2024-10-30 through fold-20's test end
2026-07-22, the same window as run 4).

---

## Commands run, in order

```bash
# step 1-3 (code) landed and tested first: 863c129, 2b322c7, 0d22833
service/target/release/scalper-data pull-binance-tape \
  --data-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --concurrency 12
service/target/release/scalper-data training-matrix \
  --data-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --out data/matrices/gate-run-5.jsonl \
  --stride 5 --micro-root data --tape-root data
for H in 15 30 60; do
  (cd training && uv run python walk_forward_scalper.py \
     --matrix ../data/matrices/gate-run-5.jsonl --horizon $H \
     --out-dir ../data/models/gate-run-5-h$H)
  service/target/release/scalper-data gate \
    --matrix data/matrices/gate-run-5.jsonl \
    --folds data/models/gate-run-5-h$H/folds.json \
    --binance-costs data/binance-micro/costs-daily-3b.json \
    --fee-taker-bps 4.5 --fee-maker-bps 1.8 --notional 5000 \
    --exit atr --entry maker --data-root data --tape-root data \
    --universe data/scalper-universe.json \
    --out data/reports/gate-run-5-h$H.json
done
```

Timings on this machine (32 cores / 62 GB): tape pull ~2 h (network-bound);
matrix 23.6 min; each fit ~1 min; each maker gate ~1.5 min.

---

## Matrix

`data/matrices/gate-run-5.jsonl`: **2,446,102 rows**, 24 assets, 50 features
(run 4: 2,143,834 in-test-window predictions; this run: 2,143,486 — the
tick features cost almost no rows on liquid names and a modest number on
thin ones, and the dominant row-droppers remain the pre-existing
`spread_z_60` / `spread_bps` on the thin assets, exactly as in run 4;
`training-matrix` now prints per-asset None counts by feature so this can be
read directly).

| asset | rows | first kept | last kept | months |
|---|---:|---|---|---:|

| ETH | 203,385 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| BTC | 203,301 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| SOL | 201,078 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| XRP | 196,883 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| DOGE | 190,775 | 2024-08-01 01:05 | 2026-08-13 17:15 | 24.4 |
| kPEPE | 185,481 | 2024-08-01 01:05 | 2026-08-13 20:35 | 24.4 |
| SUI | 170,451 | 2024-08-01 15:45 | 2026-08-13 17:40 | 24.4 |
| ENA | 121,165 | 2024-08-01 01:05 | 2026-08-13 17:10 | 24.4 |
| WLD | 117,183 | 2024-08-01 01:05 | 2026-08-13 17:50 | 24.4 |
| TAO | 104,223 | 2024-08-01 15:55 | 2026-08-13 20:10 | 24.4 |
| LINK | 99,756 | 2024-08-01 05:25 | 2026-08-13 17:10 | 24.4 |
| FARTCOIN | 97,985 | 2024-12-20 20:00 | 2026-07-15 14:25 | 18.8 |
| ZEC | 85,991 | 2024-08-01 01:05 | 2026-08-13 22:55 | 24.4 |
| AAVE | 83,695 | 2024-08-01 01:05 | 2026-08-13 16:55 | 24.4 |
| VIRTUAL | 69,187 | 2024-12-10 16:00 | 2026-08-12 20:15 | 20.0 |
| UNI | 53,647 | 2024-08-01 05:25 | 2026-08-13 13:10 | 24.4 |
| NEAR | 49,642 | 2024-08-01 05:25 | 2026-08-13 16:50 | 24.4 |
| ETHFI | 48,256 | 2024-08-01 15:25 | 2026-08-13 22:55 | 24.4 |
| PUMP | 45,266 | 2025-04-12 16:00 | 2026-08-13 20:50 | 16.0 |
| KAITO | 37,198 | 2025-02-20 16:00 | 2026-08-13 21:50 | 17.7 |
| XPL | 29,516 | 2025-08-22 12:00 | 2026-08-11 16:30 | 11.6 |
| CRV | 20,108 | 2024-08-01 09:20 | 2026-08-12 08:45 | 24.3 |
| LIT | 18,134 | 2024-10-21 15:25 | 2026-08-13 22:35 | 21.7 |
| XMR | 13,796 | 2024-08-02 15:15 | 2026-08-13 22:55 | 24.4 |

---

## Per-horizon results (first run, full universe)

| horizon | n_trades | sharpe_annualized | gate | ic (pooled) | rank_ic (pooled) | orders / fills / misses | fill rate | mean fill delay | exits stops/targets/time (of which in fill minute) | mean bars held | net total bps | net bps/trade | hit rate | zero-return days | folds traded/21 (positive Sharpe) |
|---:|---:|---:|---|---:|---:|---|---:|---:|---|---:|---:|---:|---:|---:|---|
| 15 | 8,931 | 0.3649 | FAIL | 0.0368 | 0.0163 | 9,998 / 8,931 / 1,067 | 0.893 | 4.55 s | 1,193/652/7,086 (60/14) | 13.56 | +50,992.9 | +5.71 | 0.4771 | 250/631 | 17 (9) |
| 30 | 7,809 | 0.1883 | FAIL | 0.0105 | 0.0054 | 8,775 / 7,809 / 966 | 0.890 | 4.43 s | 1,905/1,319/4,585 (53/25) | 23.52 | +18,366.7 | +2.35 | 0.4733 | 237/631 | 17 (8) |
| 60 | 8,080 | 0.4321 | FAIL | 0.0133 | 0.0033 | 9,060 / 8,080 / 980 | 0.892 | 4.65 s | 2,973/2,274/2,833 (65/14) | 37.16 | +37,125.8 | +4.59 | 0.4729 | 327/631 | 13 (7) |

`n_preds` = 2,143,486 at every horizon (same folds, same rows). Orders
placed = accepted signals with a free slot, a cost, an ATR and tape coverage
for the fill minute (`skipped_no_tape` = 0, `skipped_no_atr` =
0, `skipped_no_end_bar` = 0 at h15; the same zeros at h30/h60).
Mean maker threshold across assets (`1.5 × mean maker round trip`) is
13.48 bps — vs ~20 bps under the taker formula in run 4 — and orders are
still only 0.47% / 0.41% / 0.42% of predictions.

### Run 4 (taker, fs-4) vs run 5 (maker, fs-5), same folds and window

| | h15 run 4 | h15 run 5 | h30 run 4 | h30 run 5 | h60 run 4 | h60 run 5 |
|---|---:|---:|---:|---:|---:|---:|
| Sharpe | 0.7567 | 0.3649 | 0.6869 | 0.1883 | 0.5127 | 0.4321 |
| pooled IC | 0.0366 | 0.0368 | 0.0216 | 0.0105 | 0.0148 | 0.0133 |
| pooled rank IC | 0.0144 | 0.0163 | 0.0046 | 0.0054 | 0.0002 | 0.0033 |
| trades | 4,144 | 8,931 | 5,892 | 7,809 | 5,648 | 8,080 |
| net bps/trade | +30.14 | +5.71 | +16.77 | +2.35 | +10.61 | +4.59 |

---

## Fold-level detail

### h15
| fold | test window | n_preds | orders | fills | trades | sharpe | ic | rank_ic | pred_abs p50/p90/p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | 2024-10-30..2024-11-29 | 107,037 | 402 | 367 | 367 | 0.66 | 0.0274 | 0.0325 | 0.75/3.95/10.90 |
| 1 | 2024-11-29..2024-12-29 | 133,006 | 737 | 659 | 659 | 2.31 | 0.0062 | 0.0039 | 0.69/4.62/11.01 |
| 2 | 2024-12-29..2025-01-28 | 118,764 | 1071 | 969 | 969 | -1.75 | -0.0028 | 0.0012 | 0.62/3.78/14.86 |
| 3 | 2025-01-28..2025-02-27 | 118,133 | 547 | 485 | 485 | 4.67 | 0.0445 | 0.0076 | 0.31/2.01/9.26 |
| 4 | 2025-02-27..2025-03-29 | 108,688 | 1007 | 930 | 930 | -2.52 | 0.0020 | 0.0071 | 0.55/3.17/13.90 |
| 5 | 2025-03-29..2025-04-28 | 99,318 | 0 | 0 | 0 | n/a | 0.0215 | 0.0142 | 0.04/0.19/1.37 |
| 6 | 2025-04-28..2025-05-28 | 120,869 | 0 | 0 | 0 | n/a | -0.0004 | 0.0195 | 0.10/0.20/1.08 |
| 7 | 2025-05-28..2025-06-27 | 116,489 | 182 | 161 | 161 | -3.97 | -0.0049 | 0.0164 | 0.37/1.26/5.21 |
| 8 | 2025-06-27..2025-07-27 | 110,212 | 25 | 23 | 23 | 2.51 | 0.0494 | 0.0231 | 0.11/0.46/2.63 |
| 9 | 2025-07-27..2025-08-26 | 114,727 | 705 | 648 | 648 | -2.74 | 0.0434 | 0.0336 | 0.68/2.74/11.58 |
| 10 | 2025-08-26..2025-09-25 | 107,146 | 965 | 873 | 873 | 0.37 | 0.0376 | 0.0378 | 0.70/3.03/15.57 |
| 11 | 2025-09-25..2025-10-25 | 123,628 | 1804 | 1597 | 1597 | 2.58 | 0.0868 | 0.0188 | 0.85/5.25/24.67 |
| 12 | 2025-10-25..2025-11-24 | 122,693 | 1825 | 1585 | 1585 | -2.49 | 0.0422 | 0.0204 | 0.86/4.66/17.88 |
| 13 | 2025-11-24..2025-12-24 | 93,546 | 265 | 238 | 238 | -4.90 | 0.0081 | 0.0239 | 0.42/1.97/7.57 |
| 14 | 2025-12-24..2026-01-23 | 87,061 | 165 | 149 | 149 | -4.33 | -0.0287 | 0.0109 | 0.41/1.50/6.81 |
| 15 | 2026-01-23..2026-02-22 | 86,120 | 0 | 0 | 0 | n/a | 0.0036 | 0.0239 | 0.09/0.14/1.80 |
| 16 | 2026-02-22..2026-03-24 | 70,222 | 0 | 0 | 0 | n/a | 0.0086 | 0.0260 | 0.14/0.20/1.15 |
| 17 | 2026-03-24..2026-04-23 | 59,266 | 1 | 1 | 1 | 3.43 | 0.0157 | 0.0266 | 0.18/0.41/1.34 |
| 18 | 2026-04-23..2026-05-23 | 66,912 | 19 | 14 | 14 | 2.08 | 0.0431 | 0.0389 | 0.33/0.98/3.26 |
| 19 | 2026-05-23..2026-06-22 | 98,193 | 166 | 129 | 129 | 4.59 | 0.0401 | 0.0366 | 0.37/1.16/5.84 |
| 20 | 2026-06-22..2026-07-22 | 81,456 | 112 | 103 | 103 | -4.41 | 0.0081 | 0.0236 | 0.47/1.45/5.79 |

### h30
| fold | test window | n_preds | orders | fills | trades | sharpe | ic | rank_ic | pred_abs p50/p90/p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | 2024-10-30..2024-11-29 | 107,037 | 0 | 0 | 0 | n/a | 0.0181 | 0.0044 | 0.50/1.10/3.35 |
| 1 | 2024-11-29..2024-12-29 | 133,006 | 2635 | 2366 | 2366 | -6.67 | 0.0068 | 0.0061 | 1.61/7.80/20.84 |
| 2 | 2024-12-29..2025-01-28 | 118,764 | 1381 | 1239 | 1239 | -2.64 | -0.0072 | -0.0090 | 1.29/4.90/19.95 |
| 3 | 2025-01-28..2025-02-27 | 118,133 | 60 | 48 | 48 | 6.72 | 0.0258 | -0.0030 | 0.76/1.65/6.72 |
| 4 | 2025-02-27..2025-03-29 | 108,688 | 845 | 803 | 803 | -7.42 | -0.0427 | -0.0044 | 0.34/2.41/12.82 |
| 5 | 2025-03-29..2025-04-28 | 99,318 | 0 | 0 | 0 | n/a | 0.0019 | 0.0310 | 0.10/0.23/1.92 |
| 6 | 2025-04-28..2025-05-28 | 120,869 | 258 | 224 | 224 | -0.14 | 0.0207 | 0.0263 | 0.48/1.74/7.82 |
| 7 | 2025-05-28..2025-06-27 | 116,489 | 310 | 267 | 267 | -2.98 | -0.0306 | -0.0098 | 0.63/1.88/7.40 |
| 8 | 2025-06-27..2025-07-27 | 110,212 | 0 | 0 | 0 | n/a | 0.0064 | 0.0056 | 0.29/0.29/1.37 |
| 9 | 2025-07-27..2025-08-26 | 114,727 | 241 | 219 | 219 | 1.75 | 0.0198 | 0.0255 | 0.52/1.71/7.04 |
| 10 | 2025-08-26..2025-09-25 | 107,146 | 174 | 157 | 157 | -2.22 | 0.0205 | 0.0329 | 0.28/0.95/6.16 |
| 11 | 2025-09-25..2025-10-25 | 123,628 | 495 | 441 | 441 | 3.39 | 0.0445 | -0.0008 | 0.38/2.46/12.07 |
| 12 | 2025-10-25..2025-11-24 | 122,693 | 1003 | 845 | 845 | 4.28 | 0.0458 | 0.0142 | 0.74/3.46/14.74 |
| 13 | 2025-11-24..2025-12-24 | 93,546 | 147 | 136 | 136 | 2.12 | 0.0009 | 0.0099 | 0.26/1.55/6.69 |
| 14 | 2025-12-24..2026-01-23 | 87,061 | 351 | 324 | 324 | -4.51 | -0.0128 | 0.0128 | 0.65/2.34/11.69 |
| 15 | 2026-01-23..2026-02-22 | 86,120 | 0 | 0 | 0 | n/a | 0.0755 | 0.0247 | 0.17/0.32/1.81 |
| 16 | 2026-02-22..2026-03-24 | 70,222 | 294 | 258 | 258 | 2.93 | 0.0183 | 0.0048 | 0.74/2.52/8.31 |
| 17 | 2026-03-24..2026-04-23 | 59,266 | 149 | 128 | 128 | -0.63 | -0.0512 | -0.0358 | 0.75/2.11/7.12 |
| 18 | 2026-04-23..2026-05-23 | 66,912 | 147 | 105 | 105 | 0.90 | 0.0295 | 0.0138 | 0.65/2.02/6.42 |
| 19 | 2026-05-23..2026-06-22 | 98,193 | 171 | 147 | 147 | 0.84 | 0.0216 | 0.0101 | 0.53/1.77/7.52 |
| 20 | 2026-06-22..2026-07-22 | 81,456 | 114 | 102 | 102 | -1.74 | 0.0275 | 0.0183 | 0.59/1.65/5.09 |

### h60
| fold | test window | n_preds | orders | fills | trades | sharpe | ic | rank_ic | pred_abs p50/p90/p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---|
| 0 | 2024-10-30..2024-11-29 | 107,037 | 0 | 0 | 0 | n/a | 0.0137 | 0.0030 | 0.94/2.29/3.82 |
| 1 | 2024-11-29..2024-12-29 | 133,006 | 1844 | 1689 | 1689 | -1.32 | -0.0016 | -0.0140 | 2.64/7.99/20.08 |
| 2 | 2024-12-29..2025-01-28 | 118,764 | 3167 | 2858 | 2858 | -4.54 | -0.0041 | -0.0203 | 2.76/10.47/39.11 |
| 3 | 2025-01-28..2025-02-27 | 118,133 | 0 | 0 | 0 | n/a | -0.0063 | -0.0163 | 1.67/2.69/6.83 |
| 4 | 2025-02-27..2025-03-29 | 108,688 | 0 | 0 | 0 | n/a | -0.0197 | 0.0010 | 0.59/1.01/4.29 |
| 5 | 2025-03-29..2025-04-28 | 99,318 | 47 | 45 | 45 | 4.03 | -0.0116 | 0.0254 | 0.34/1.33/5.51 |
| 6 | 2025-04-28..2025-05-28 | 120,869 | 123 | 110 | 110 | 9.05 | 0.0178 | 0.0255 | 0.65/1.70/5.94 |
| 7 | 2025-05-28..2025-06-27 | 116,489 | 0 | 0 | 0 | n/a | -0.0039 | 0.0091 | 0.19/0.39/1.54 |
| 8 | 2025-06-27..2025-07-27 | 110,212 | 0 | 0 | 0 | n/a | 0.0023 | 0.0116 | 0.65/0.78/1.75 |
| 9 | 2025-07-27..2025-08-26 | 114,727 | 301 | 272 | 272 | 3.04 | 0.0173 | -0.0002 | 0.79/2.23/10.01 |
| 10 | 2025-08-26..2025-09-25 | 107,146 | 260 | 235 | 235 | -4.40 | 0.0126 | 0.0430 | 0.72/2.14/10.61 |
| 11 | 2025-09-25..2025-10-25 | 123,628 | 836 | 733 | 733 | 3.57 | 0.0582 | 0.0085 | 1.16/3.97/22.78 |
| 12 | 2025-10-25..2025-11-24 | 122,693 | 1107 | 930 | 930 | 3.22 | 0.0446 | 0.0291 | 1.25/4.17/18.68 |
| 13 | 2025-11-24..2025-12-24 | 93,546 | 0 | 0 | 0 | n/a | -0.0068 | -0.0021 | 0.16/0.42/1.65 |
| 14 | 2025-12-24..2026-01-23 | 87,061 | 758 | 681 | 681 | -5.02 | 0.0196 | 0.0184 | 1.54/5.85/24.20 |
| 15 | 2026-01-23..2026-02-22 | 86,120 | 0 | 0 | 0 | n/a | 0.0666 | -0.0016 | 0.37/0.58/2.36 |
| 16 | 2026-02-22..2026-03-24 | 70,222 | 230 | 201 | 201 | 2.00 | 0.0116 | 0.0182 | 1.36/3.16/9.37 |
| 17 | 2026-03-24..2026-04-23 | 59,266 | 179 | 156 | 156 | -7.75 | -0.0693 | -0.0631 | 1.18/3.27/10.79 |
| 18 | 2026-04-23..2026-05-23 | 66,912 | 151 | 125 | 125 | -1.13 | -0.0013 | -0.0019 | 1.18/3.41/10.60 |
| 19 | 2026-05-23..2026-06-22 | 98,193 | 0 | 0 | 0 | n/a | 0.0503 | 0.0210 | 0.69/1.03/2.53 |
| 20 | 2026-06-22..2026-07-22 | 81,456 | 57 | 45 | 45 | 3.32 | 0.0506 | 0.0135 | 0.84/1.92/6.25 |

---

## Per-asset breakdown (first run)

### h15
| asset | orders | fills | fill rate | trades | total_net_bps | net/trade | hit rate | mean threshold bps (1.5 × maker RT) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SUI | 395 | 360 | 0.911 | 360 | +16,465.5 | +45.74 | 0.489 | 10.52 |
| LINK | 325 | 288 | 0.886 | 288 | +10,712.5 | +37.20 | 0.510 | 10.39 |
| AAVE | 292 | 267 | 0.914 | 267 | +8,681.0 | +32.51 | 0.491 | 10.69 |
| UNI | 283 | 257 | 0.908 | 257 | +8,469.6 | +32.96 | 0.502 | 11.73 |
| kPEPE | 527 | 492 | 0.934 | 492 | +6,775.2 | +13.77 | 0.474 | 10.43 |
| XRP | 381 | 348 | 0.913 | 348 | +6,732.9 | +19.35 | 0.471 | 10.01 |
| WLD | 522 | 463 | 0.887 | 463 | +6,618.1 | +14.29 | 0.469 | 11.71 |
| DOGE | 479 | 441 | 0.921 | 441 | +5,487.1 | +12.44 | 0.508 | 10.06 |
| VIRTUAL | 511 | 464 | 0.908 | 464 | +3,640.3 | +7.85 | 0.440 | 12.71 |
| SOL | 275 | 249 | 0.905 | 249 | +2,045.1 | +8.21 | 0.474 | 10.18 |
| PUMP | 578 | 506 | 0.875 | 506 | +1,588.0 | +3.14 | 0.512 | 19.16 |
| ETHFI | 322 | 279 | 0.866 | 279 | +853.5 | +3.06 | 0.491 | 13.69 |
| ENA | 557 | 495 | 0.889 | 495 | +663.8 | +1.34 | 0.475 | 13.38 |
| TAO | 334 | 289 | 0.865 | 289 | +410.5 | +1.42 | 0.446 | 11.21 |
| NEAR | 195 | 163 | 0.836 | 163 | +361.1 | +2.22 | 0.436 | 13.73 |
| BTC | 110 | 103 | 0.936 | 103 | -69.7 | -0.68 | 0.476 | 9.51 |
| XMR | 47 | 40 | 0.851 | 40 | -193.0 | -4.82 | 0.525 | 13.23 |
| CRV | 20 | 18 | 0.900 | 18 | -695.1 | -38.62 | 0.333 | 25.86 |
| LIT | 39 | 36 | 0.923 | 36 | -1,217.8 | -33.83 | 0.472 | 23.67 |
| ETH | 215 | 195 | 0.907 | 195 | -1,770.0 | -9.08 | 0.441 | 9.67 |
| KAITO | 325 | 301 | 0.926 | 301 | -4,239.6 | -14.08 | 0.472 | 16.56 |
| XPL | 918 | 816 | 0.889 | 816 | -6,573.1 | -8.06 | 0.483 | 16.94 |
| FARTCOIN | 1424 | 1258 | 0.883 | 1258 | -6,667.2 | -5.30 | 0.466 | 13.73 |
| ZEC | 924 | 803 | 0.869 | 803 | -7,086.0 | -8.82 | 0.481 | 14.83 |

### h30
| asset | orders | fills | fill rate | trades | total_net_bps | net/trade | hit rate | mean threshold bps (1.5 × maker RT) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SUI | 408 | 366 | 0.897 | 366 | +15,427.9 | +42.15 | 0.492 | 10.52 |
| VIRTUAL | 536 | 479 | 0.894 | 479 | +8,613.9 | +17.98 | 0.482 | 12.71 |
| kPEPE | 551 | 491 | 0.891 | 491 | +6,552.8 | +13.35 | 0.505 | 10.43 |
| DOGE | 394 | 358 | 0.909 | 358 | +5,546.1 | +15.49 | 0.511 | 10.06 |
| XRP | 443 | 388 | 0.876 | 388 | +4,299.0 | +11.08 | 0.487 | 10.01 |
| AAVE | 398 | 363 | 0.912 | 363 | +3,907.5 | +10.76 | 0.424 | 10.69 |
| XPL | 278 | 241 | 0.867 | 241 | +3,142.9 | +13.04 | 0.469 | 16.94 |
| SOL | 313 | 280 | 0.895 | 280 | +2,313.0 | +8.26 | 0.514 | 10.18 |
| LINK | 413 | 375 | 0.908 | 375 | +1,918.6 | +5.12 | 0.456 | 10.39 |
| ZEC | 622 | 541 | 0.870 | 541 | +1,594.4 | +2.95 | 0.482 | 14.83 |
| ETH | 235 | 214 | 0.911 | 214 | +904.3 | +4.23 | 0.556 | 9.67 |
| XMR | 87 | 75 | 0.862 | 75 | -46.3 | -0.62 | 0.453 | 13.23 |
| BTC | 179 | 164 | 0.916 | 164 | -166.8 | -1.02 | 0.500 | 9.51 |
| LIT | 109 | 95 | 0.872 | 95 | -203.0 | -2.14 | 0.453 | 23.67 |
| PUMP | 183 | 156 | 0.852 | 156 | -2,161.1 | -13.85 | 0.462 | 19.16 |
| ETHFI | 304 | 273 | 0.898 | 273 | -2,269.9 | -8.31 | 0.436 | 13.69 |
| TAO | 375 | 336 | 0.896 | 336 | -2,277.3 | -6.78 | 0.476 | 11.21 |
| UNI | 340 | 303 | 0.891 | 303 | -2,284.0 | -7.54 | 0.442 | 11.73 |
| KAITO | 170 | 158 | 0.929 | 158 | -2,443.2 | -15.46 | 0.462 | 16.56 |
| NEAR | 243 | 213 | 0.877 | 213 | -2,907.2 | -13.65 | 0.460 | 13.73 |
| ENA | 491 | 444 | 0.904 | 444 | -2,948.7 | -6.64 | 0.491 | 13.38 |
| CRV | 120 | 104 | 0.867 | 104 | -4,820.3 | -46.35 | 0.394 | 25.86 |
| FARTCOIN | 1048 | 921 | 0.879 | 921 | -6,385.4 | -6.93 | 0.448 | 13.73 |
| WLD | 535 | 471 | 0.880 | 471 | -6,940.6 | -14.74 | 0.459 | 11.71 |

### h60
| asset | orders | fills | fill rate | trades | total_net_bps | net/trade | hit rate | mean threshold bps (1.5 × maker RT) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| VIRTUAL | 736 | 659 | 0.895 | 659 | +14,977.1 | +22.73 | 0.490 | 12.71 |
| SUI | 396 | 362 | 0.914 | 362 | +12,944.7 | +35.76 | 0.453 | 10.52 |
| ZEC | 722 | 638 | 0.884 | 638 | +9,316.2 | +14.60 | 0.522 | 14.83 |
| DOGE | 365 | 336 | 0.921 | 336 | +6,069.9 | +18.07 | 0.512 | 10.06 |
| LINK | 362 | 333 | 0.920 | 333 | +5,694.3 | +17.10 | 0.474 | 10.39 |
| AAVE | 371 | 335 | 0.903 | 335 | +5,606.8 | +16.74 | 0.451 | 10.69 |
| XRP | 396 | 362 | 0.914 | 362 | +4,642.8 | +12.83 | 0.489 | 10.01 |
| kPEPE | 493 | 452 | 0.917 | 452 | +4,528.6 | +10.02 | 0.469 | 10.43 |
| ETHFI | 307 | 276 | 0.899 | 276 | +4,269.2 | +15.47 | 0.493 | 13.69 |
| XPL | 396 | 336 | 0.848 | 336 | +2,969.9 | +8.84 | 0.470 | 16.94 |
| UNI | 291 | 260 | 0.893 | 260 | +2,253.9 | +8.67 | 0.473 | 11.73 |
| SOL | 287 | 254 | 0.885 | 254 | +1,997.9 | +7.87 | 0.496 | 10.18 |
| BTC | 135 | 119 | 0.881 | 119 | +1,124.5 | +9.45 | 0.504 | 9.51 |
| ETH | 181 | 163 | 0.901 | 163 | +988.2 | +6.06 | 0.509 | 9.67 |
| KAITO | 198 | 172 | 0.869 | 172 | +465.5 | +2.71 | 0.477 | 16.56 |
| ENA | 525 | 474 | 0.903 | 474 | +318.3 | +0.67 | 0.466 | 13.38 |
| XMR | 106 | 90 | 0.849 | 90 | -690.7 | -7.67 | 0.478 | 13.23 |
| NEAR | 225 | 195 | 0.867 | 195 | -2,363.8 | -12.12 | 0.462 | 13.73 |
| TAO | 384 | 336 | 0.875 | 336 | -2,701.3 | -8.04 | 0.461 | 11.21 |
| PUMP | 290 | 242 | 0.834 | 242 | -3,978.7 | -16.44 | 0.467 | 19.16 |
| CRV | 101 | 79 | 0.782 | 79 | -4,489.9 | -56.83 | 0.354 | 25.86 |
| FARTCOIN | 1069 | 967 | 0.905 | 967 | -8,497.3 | -8.79 | 0.447 | 13.73 |
| LIT | 239 | 210 | 0.879 | 210 | -8,844.0 | -42.11 | 0.443 | 23.67 |
| WLD | 485 | 430 | 0.887 | 430 | -9,476.2 | -22.04 | 0.437 | 11.71 |

---

## Feature gain: what the tick features contributed

Split-gain share summed over all 21 fold models per horizon (LightGBM
`split_gain` from the fold artifacts; the same measure the run-4 record used):

| horizon | all 12 `tk_*` together | top tick features | top features overall |
|---:|---:|---|---|

| 15 | 8.1% | tk_ret_10s 1.6%, tk_ret_30s 1.4%, tk_vwap_dev_30s 1.3%, tk_intensity_10s 0.7% | btc_ret_5 10.6%, tod_sin 10.1%, tod_cos 9.0%, vol_60 5.6%, vol_15 4.9% |
| 30 | 4.5% | tk_ret_10s 1.0%, tk_ret_30s 0.8%, tk_size_med_5m_log 0.5%, tk_intensity_10s 0.5% | tod_sin 13.0%, tod_cos 10.4%, btc_ret_5 8.6%, dow 8.3%, vol_60 8.2% |
| 60 | 3.1% | tk_ret_30s 0.8%, tk_size_med_5m_log 0.6%, tk_intensity_10s 0.4%, tk_vwap_dev_30s 0.3% | tod_sin 13.6%, dow 12.3%, tod_cos 10.8%, vol_60 9.0%, ret_60 7.9% |

The models lean on the same things run 4's did — time of day, day of week,
BTC context, realized volatility, 60-minute return — and the tick features
take 8.1% of gain at h15 falling to 3.1% at h60. The sub-minute
returns (`tk_ret_10s`, `tk_ret_30s`) and vwap deviation are the only tick
features that register at all; the flow-imbalance family (`tk_imb_*`,
`tk_large_imb_5m`, `tk_run`) is essentially unused. Pooled IC is unchanged
at h15 and lower at h30/h60 than run 4's — within what fold-to-fold noise
would produce, but in no reading an improvement.

---

## Fee fixed-point step (Amendment 1, unchanged by Amendments 2–5)

Run 5 charges VIP0 + BNB maker/taker (1.80/4.50 bps) as pre-registered.
`overall.projected_30d_volume_usd`: h15 $4,246,117.27, h30 $3,712,678.29,
h60 $3,841,521.39 — roughly double run 4's ($1.97M–$2.80M), because the
maker path places about twice as many orders. Mapping by the volume
criterion only against `docs/binance-um-fee-table-2026-08.md`: as in run 4,
every horizon's figure sits **inside** the disputed VIP1 candidate band
($250k / $1M / $5M / $15M, none Binance-authored) — above the two lowest
candidates, below the two highest — so the tier cannot be confidently
mapped to VIP0 and no VIP1–8 fee is invented to test at.

**Outcome: STOPPED at this step, verdict PROVISIONAL pending an
authenticated fee fetch — the same disposition as runs 2 and 4.** As a
numeric observation only: VIP1+ fees can only be lower than the VIP0 rates
charged here, and every horizon fails by 1.57–1.81 points; the maker
fee (1.8 bps) is already the smaller leg of the round trip, so a cheaper
schedule cannot plausibly close a gap of that size. That expectation is not a
substitute for the required re-run.

---

## Drop-rule re-run (Amendment 1 / §5: one drop, one re-run — exercised)

§5's one allowed universe iteration: a symbol may be dropped only if its
`total_net_bps` is negative in **all three** horizon reports of the first
run. This is the first run in the project's history where any symbol
qualifies: **CRV, FARTCOIN, LIT, XMR** (4 of 24). (Negative at h15 only:
BTC, CRV, ETH, FARTCOIN, KAITO, LIT, XMR, XPL, ZEC; h30: BTC, CRV, ENA, ETHFI, FARTCOIN, KAITO, LIT, NEAR, PUMP, TAO, UNI, WLD, XMR; h60:
CRV, FARTCOIN, LIT, NEAR, PUMP, TAO, WLD, XMR.) Run 4 had zero qualifiers and did not exercise the
step; here it is exercised exactly as written — the same exclude list feeds
all three horizons, one rebuild, one refit, one re-run, and **the second
run's verdict is the verdict**.

Mechanics: the frozen `data/scalper-universe.json` is not touched (running
`universe` would re-fetch live volume ranks and violate the frozen-data
rule); the exclude is applied as a filtered copy,
`data/scalper-universe-run5b.json` (25 → 21 entries, 20 mapped), and the
matrix is rebuilt from it — `data/matrices/gate-run-5b.jsonl` — with every
other flag identical to the first run; fits to `data/models/gate-run-5b-h*`,
reports to `data/reports/gate-run-5b-h*.json`.


### Re-run results (20 assets)

| horizon | n_trades | sharpe_annualized | gate | ic (pooled) | rank_ic (pooled) | orders / fills / misses | fill rate | mean fill delay | exits stops/targets/time (of which in fill minute) | mean bars held | net total bps | net bps/trade | hit rate | zero-return days | folds traded/21 (positive Sharpe) |
|---:|---:|---:|---|---:|---:|---|---:|---:|---|---:|---:|---:|---:|---:|---|
| 15 | 10,780 | 0.1508 | FAIL | 0.0277 | 0.0174 | 12,018 / 10,780 / 1,238 | 0.897 | 4.54 s | 1,409/750/8,621 (62/10) | 13.64 | +18,141.6 | +1.68 | 0.4798 | 294/631 | 18 (6) |
| 30 | 9,164 | 0.1331 | FAIL | 0.0239 | 0.0072 | 10,227 / 9,164 / 1,063 | 0.896 | 4.47 s | 2,321/1,408/5,435 (62/9) | 24.00 | +16,652.1 | +1.82 | 0.4664 | 307/631 | 17 (9) |
| 60 | 12,272 | -0.2510 | FAIL | 0.0218 | 0.0038 | 13,572 / 12,272 / 1,300 | 0.904 | 4.57 s | 4,751/3,467/4,054 (63/10) | 36.71 | -25,096.3 | -2.05 | 0.4594 | 286/631 | 15 (7) |

Note the equal-weight capital denominator is now 20 slots, not 24, so per-day
returns are mechanically larger per trade; the Sharpe is what the rule reads.

| | h15 first | h15 re-run | h30 first | h30 re-run | h60 first | h60 re-run |
|---|---:|---:|---:|---:|---:|---:|
| Sharpe | 0.3649 | 0.1508 | 0.1883 | 0.1331 | 0.4321 | -0.2510 |
| pooled IC | 0.0368 | 0.0277 | 0.0105 | 0.0239 | 0.0133 | 0.0218 |
| trades | 8,931 | 10,780 | 7,809 | 9,164 | 8,080 | 12,272 |
| net bps/trade | +5.71 | +1.68 | +2.35 | +1.82 | +4.59 | -2.05 |

Fold detail and per-asset tables for the re-run are in the reports; the
per-asset tables are reproduced here for the record.

#### h15 (re-run)
| asset | orders | fills | fill rate | trades | total_net_bps | net/trade | hit rate | mean threshold bps (1.5 × maker RT) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SUI | 675 | 617 | 0.914 | 617 | +10,798.9 | +17.50 | 0.481 | 10.52 |
| WLD | 969 | 868 | 0.896 | 868 | +9,094.7 | +10.48 | 0.497 | 11.71 |
| AAVE | 504 | 454 | 0.901 | 454 | +4,809.8 | +10.59 | 0.478 | 10.69 |
| VIRTUAL | 935 | 850 | 0.909 | 850 | +4,373.0 | +5.14 | 0.491 | 12.71 |
| LINK | 531 | 478 | 0.900 | 478 | +2,545.0 | +5.32 | 0.460 | 10.39 |
| XPL | 722 | 634 | 0.878 | 634 | +2,240.3 | +3.53 | 0.503 | 16.94 |
| UNI | 396 | 358 | 0.904 | 358 | +1,786.8 | +4.99 | 0.461 | 11.73 |
| kPEPE | 862 | 783 | 0.908 | 783 | +1,153.5 | +1.47 | 0.481 | 10.43 |
| XRP | 633 | 581 | 0.918 | 581 | +856.4 | +1.47 | 0.453 | 10.01 |
| TAO | 580 | 519 | 0.895 | 519 | +772.0 | +1.49 | 0.489 | 11.21 |
| ENA | 985 | 855 | 0.868 | 855 | +713.6 | +0.83 | 0.489 | 13.38 |
| SOL | 457 | 409 | 0.895 | 409 | -403.0 | -0.99 | 0.474 | 10.18 |
| BTC | 167 | 159 | 0.952 | 159 | -1,025.4 | -6.45 | 0.434 | 9.51 |
| NEAR | 377 | 325 | 0.862 | 325 | -1,299.7 | -4.00 | 0.446 | 13.73 |
| ETHFI | 467 | 416 | 0.891 | 416 | -1,390.7 | -3.34 | 0.505 | 13.69 |
| KAITO | 409 | 377 | 0.922 | 377 | -2,484.1 | -6.59 | 0.488 | 16.56 |
| PUMP | 383 | 333 | 0.869 | 333 | -2,532.9 | -7.61 | 0.474 | 19.16 |
| DOGE | 769 | 704 | 0.915 | 704 | -3,000.4 | -4.26 | 0.474 | 10.06 |
| ETH | 278 | 256 | 0.921 | 256 | -3,432.8 | -13.41 | 0.453 | 9.67 |
| ZEC | 919 | 804 | 0.875 | 804 | -5,433.5 | -6.76 | 0.478 | 14.83 |

#### h30 (re-run)
| asset | orders | fills | fill rate | trades | total_net_bps | net/trade | hit rate | mean threshold bps (1.5 × maker RT) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SUI | 562 | 502 | 0.893 | 502 | +17,490.2 | +34.84 | 0.496 | 10.52 |
| WLD | 635 | 576 | 0.907 | 576 | +6,708.0 | +11.65 | 0.467 | 11.71 |
| UNI | 326 | 295 | 0.905 | 295 | +5,398.0 | +18.30 | 0.464 | 11.73 |
| VIRTUAL | 945 | 853 | 0.903 | 853 | +4,221.3 | +4.95 | 0.458 | 12.71 |
| kPEPE | 710 | 631 | 0.889 | 631 | +3,855.7 | +6.11 | 0.464 | 10.43 |
| AAVE | 423 | 385 | 0.910 | 385 | +3,640.8 | +9.46 | 0.442 | 10.69 |
| XRP | 528 | 474 | 0.898 | 474 | +2,742.0 | +5.78 | 0.460 | 10.01 |
| SOL | 382 | 344 | 0.901 | 344 | +2,072.0 | +6.02 | 0.509 | 10.18 |
| ZEC | 897 | 790 | 0.881 | 790 | +1,450.4 | +1.84 | 0.492 | 14.83 |
| DOGE | 575 | 523 | 0.910 | 523 | +193.8 | +0.37 | 0.463 | 10.06 |
| LINK | 452 | 405 | 0.896 | 405 | -108.6 | -0.27 | 0.435 | 10.39 |
| ETHFI | 439 | 393 | 0.895 | 393 | -1,267.3 | -3.22 | 0.455 | 13.69 |
| BTC | 162 | 152 | 0.938 | 152 | -1,319.8 | -8.68 | 0.447 | 9.51 |
| PUMP | 353 | 301 | 0.853 | 301 | -1,490.0 | -4.95 | 0.452 | 19.16 |
| NEAR | 263 | 228 | 0.867 | 228 | -1,873.3 | -8.22 | 0.456 | 13.73 |
| ETH | 229 | 205 | 0.895 | 205 | -1,939.5 | -9.46 | 0.488 | 9.67 |
| TAO | 523 | 470 | 0.899 | 470 | -3,011.3 | -6.41 | 0.468 | 11.21 |
| ENA | 822 | 738 | 0.898 | 738 | -4,793.5 | -6.50 | 0.462 | 13.38 |
| KAITO | 293 | 270 | 0.922 | 270 | -7,230.8 | -26.78 | 0.441 | 16.56 |
| XPL | 708 | 629 | 0.888 | 629 | -8,086.2 | -12.86 | 0.474 | 16.94 |

#### h60 (re-run)
| asset | orders | fills | fill rate | trades | total_net_bps | net/trade | hit rate | mean threshold bps (1.5 × maker RT) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SUI | 723 | 664 | 0.918 | 664 | +13,867.1 | +20.88 | 0.471 | 10.52 |
| ZEC | 1257 | 1136 | 0.904 | 1136 | +9,220.4 | +8.12 | 0.496 | 14.83 |
| VIRTUAL | 1172 | 1049 | 0.895 | 1049 | +4,368.8 | +4.16 | 0.454 | 12.71 |
| LINK | 719 | 650 | 0.904 | 650 | +2,605.9 | +4.01 | 0.449 | 10.39 |
| XPL | 790 | 708 | 0.896 | 708 | +830.9 | +1.17 | 0.466 | 16.94 |
| AAVE | 704 | 641 | 0.911 | 641 | +651.7 | +1.02 | 0.432 | 10.69 |
| DOGE | 617 | 566 | 0.917 | 566 | +187.3 | +0.33 | 0.465 | 10.06 |
| ETHFI | 597 | 540 | 0.905 | 540 | -408.7 | -0.76 | 0.487 | 13.69 |
| SOL | 458 | 412 | 0.900 | 412 | -725.6 | -1.76 | 0.476 | 10.18 |
| BTC | 184 | 171 | 0.929 | 171 | -1,066.4 | -6.24 | 0.456 | 9.51 |
| UNI | 604 | 544 | 0.901 | 544 | -1,761.1 | -3.24 | 0.447 | 11.73 |
| PUMP | 497 | 431 | 0.867 | 431 | -1,762.0 | -4.09 | 0.476 | 19.16 |
| XRP | 657 | 605 | 0.921 | 605 | -1,936.8 | -3.20 | 0.463 | 10.01 |
| ETH | 349 | 316 | 0.905 | 316 | -2,144.1 | -6.78 | 0.443 | 9.67 |
| kPEPE | 860 | 792 | 0.921 | 792 | -3,816.1 | -4.82 | 0.453 | 10.43 |
| TAO | 735 | 660 | 0.898 | 660 | -6,845.6 | -10.37 | 0.453 | 11.21 |
| NEAR | 474 | 421 | 0.888 | 421 | -6,977.4 | -16.57 | 0.439 | 13.73 |
| KAITO | 366 | 329 | 0.899 | 329 | -7,024.6 | -21.35 | 0.426 | 16.56 |
| ENA | 974 | 880 | 0.903 | 880 | -8,561.7 | -9.73 | 0.451 | 13.38 |
| WLD | 835 | 757 | 0.907 | 757 | -13,798.5 | -18.23 | 0.448 | 11.71 |

No second round of dropping, no other change: per §5, "one drop, one re-run,
then the number is the number."


---

## VERDICT

Read from the re-run (20 assets; the verdict per §5):

**h15: FAIL** (sharpe_annualized 0.150806, 1.85 points below the 2.0 gate threshold).
**h30: FAIL** (sharpe_annualized 0.133146, 1.87 points below).
**h60: FAIL** (sharpe_annualized -0.251016, 2.25 points below).

First run (24 assets): h15 0.3649 / h30 0.1883 / h60 0.4321 — also FAIL at
every horizon. Neither run passes any horizon; the drop step, exercised
for the first time, does not change the outcome. Fee tier PROVISIONAL as
above; a lower tier cannot plausibly close the gap and is not tested with an
unverified number.

**Amendment 5 §5.5's FAIL branch is invoked.** No window, threshold, latency,
rest time, fill rule, exit parameter, feature or cost term is revisited
against these numbers. Program 2 is closed under this feature set and this
execution model. Any continuation is a further *new* pre-registered
program, on its own terms, exactly as this one was — or stopping.

---

## What the diagnostics say

**The tick features did not add signal.** Pooled IC at h15 is 0.0368 against
run 4's 0.0366 on the same folds and window; at h30/h60 it is lower
(0.0105/0.0133 vs 0.0216/0.0148). The twelve features take
8.1% → 3.1% of model gain, almost all of it via the sub-minute return
and vwap-deviation features (which are near-duplicates of `ret_1` at finer
resolution), while the order-flow imbalance family the program was built
around is essentially unused by the trees. Whatever predictability there is
in Binance UM 1-minute forward returns at these horizons, minute-aggregated
inputs already captured it; the tape's imbalance/run/large-print signals do
not add to it at IC ≈ 0.04.

**The maker fill rule fills often — because it fills adversely.** 89.3% of
h15 orders fill, mean 4.6 s after the close. Under the strict
trade-through rule that is by construction a fill after price has moved
through our level against the signal, and the economics show it: run 4's
taker entry paid ~13–14 bps round trip and made +30.14 bps/trade at h15;
this run pays ~9 bps and makes +5.71 bps/trade — the ~5 bps saved on the
round trip is more than eaten by ~29 bps of adverse selection at entry. Hit
rates fall to 47.7% / 47.3% / 47.3% (run 4: 49.35 / 48.69 / 47.17).
Trade counts roughly double (the cheaper threshold admits ~2× the orders,
and 89% fill), so daily returns are somewhat less lumpy than run 4's — and
Sharpe is still lower at every horizon because the per-trade edge is gone.

**Entries remain rare.** Even at a ~9 bps round trip, only ~0.4–0.5% of
predictions clear `1.5 × round trip`; fold-level `pred_abs_p50` is well
under 1 bp in most folds and `p99` under 10–15 bps in most. The binding
constraint of Program 1 — prediction magnitude, i.e. IC — is the binding
constraint of Program 2. Cheaper execution cannot manufacture it, and a
pessimistic fill model charges for trying.

**Exits.** Time exits still dominate at h15 (7,086 of 8,931); stops+targets
exceed time exits only at h60, as in run 4. Fill-minute resolutions on the
tape are rare (60/14 at h15), so the OHLC path from the next bar
on is what determined nearly every exit; the clarification in Amendment 5
§5.3 was applied but was not consequential.

**Volatility of fold Sharpes.** Fold Sharpes range roughly −5 to +5 at
every horizon with 8–9 positive of 17 traded (h15/h30) and 7 of 13 (h60);
several folds place no orders at all (fold-level `pred_abs_p99` under
~2 bps). This is the same "genuine but order-of-magnitude-too-small
signal" shape run 4 recorded, now measured under a second execution model
and a richer feature set, with the same result.

---

## Provenance

- Reports: `data/reports/gate-run-5-h{15,30,60}.json`, `data/reports/gate-run-5b-h{15,30,60}.json` (byte-reproducible from the matrix, folds, costs and tape with the commands above; `generated_utc` aside).
- Folds: `data/models/gate-run-5-h*/`, `data/models/gate-run-5b-h*/` (21 folds each; schedule identical to run 4's — fold-0 tests 2024-10-30..2024-11-29, fold-20 tests 2026-06-22..2026-07-22).
- Matrices: `data/matrices/gate-run-5.jsonl` (24 assets), `data/matrices/gate-run-5b.jsonl` (20 assets).
- Tape: `data/binance-micro/tape/` + `manifest.json`.
- Runs 1–4 stay on file exactly as written; run 4 remains the record for Program 1.
