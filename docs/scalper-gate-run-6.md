# Crypto scalper — gate run 6 (Program 3: fs-6 cross-asset tick context, taker, Amendment 6)

> **Bottom line up front: this run IS gate-eligible under Amendments 1–6 and
> it FAILS the gate at all three horizons — h15 Sharpe 0.6007, h30
> 0.7272, h60 0.6718** (first run, 24 assets; run 4, the same
> taker setup on fs-4, was 0.7567 / 0.6869 / 0.5127).
> Drop-rule qualifiers: CRV, LIT, WLD; the §5 re-run also FAILS at all three horizons — 0.6671 / 0.4368 / 0.5586.
> The six BTC tick-context features take 31.3% / 24.1% / 15.7% of model gain
> and pooled IC is 0.0258 / 0.0197 / 0.0214 (run 4: 0.0366 / 0.0216 / 0.0148).
> Under Amendment 6 §6.2 the FAIL branch is invoked; nothing is revisited against these numbers. See "VERDICT".

**Run date:** 2026-08-18. **Repo:** `/home/magnus/dev/magnus/ai-trader`, `main`.
**Protocol:** Amendment 6 (`242b3b0`), on top of §1–6 and Amendments 1–5.
**Code:** fs-6 `a933a71` (`features-scalper::btc_tick_context` + `compute`; `training-matrix` computes BTC's context once).
**Data:** byte-frozen as run 5 left it, tape included; no pull of any kind.

---

## What this run is

fs-6 = fs-5's 50 features (fs-4's 38 + Program 2's 12 own-asset tick
features), byte-for-byte on the same code path, plus six BTC-context tick
features evaluated on `BTCUSDT`'s tape at the same close as each asset's
bar — `btc_tk_ret_10s`, `btc_tk_ret_30s`, `btc_tk_imb_30s`, `btc_tk_imb_5m`,
`btc_tk_intensity_10s`, `rel_tk_ret_30s` (own − BTC). BTC's context is
computed once over BTC's own bar sequence with BTC's own coverage discipline
and joined by timestamp; for BTC itself the six equal the own features and
`rel` is 0. Execution is run 4's exact taker setup (`gate --exit atr`,
Amendment 3 exits, 3b costs, 4.50/1.80 bps, `--threshold-mult 1.5`,
`--notional 5000`); the label, folds, horizons, universe, gate, drop rule
and fee fixed-point are unchanged. The comparison this program makes is
run 4 → run 6: same folds, window, execution; only the feature set differs.

Real-data parity before the run: on 2,039 BTC + ZEC rows in a 7-day window,
the fs-6 matrix's first 50 features and labels matched `gate-run-5.jsonl`
row for row; BTC rows had `rel_tk_ret_30s = 0` and `btc_tk_* = tk_*`.

## Eligibility checklist (Amendments 1–6)

| # | condition | status | evidence |
|---|---|---|---|
| 1 | ≥18 months of matrix span | **MET** | 2024-08-01 to 2026-08-13, **24.4 months** — identical to runs 4/5. 21 of 24 assets clear 18 months individually; PUMP, KAITO, XPL short for listing-date reasons, as before. |
| 2 | Full mapped universe | **MET** | 24 of 24 assets have rows. |
| 3 | fs-6 end to end | **MET** | manifest `fs-rust-scalper-6`, 56 features; fold artifacts carry the same version. |
| 4 | `--binance-costs` | **MET** | `costs-daily-3b.json`, frozen. |
| 5 | Three horizons every run | **MET** | 15/30/60 fitted and gated in order. |
| 6 | Data frozen, tape unchanged | **MET** | no pull; `git status` clean; BTC tick context: 1069920 minute(s), 1069821 fully covered. |

Costed span: `days_without_costs` = AAVE 5, CRV 4, ETHFI 5, FARTCOIN 5, LINK 5, NEAR 5, PUMP 1, SOL 1, SUI 1, TAO 6, UNI 1, VIRTUAL 1, WLD 5, XMR 2, XRP 5, ZEC 7 — same as runs 4/5; OOS window 631 days (2024-10-30..2026-07-22).

## Commands run, in order

```bash
service/target/release/scalper-data training-matrix --data-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --out data/matrices/gate-run-6.jsonl --stride 5 --micro-root data --tape-root data
for H in 15 30 60; do
  (cd training && uv run python walk_forward_scalper.py --matrix ../data/matrices/gate-run-6.jsonl --horizon $H --out-dir ../data/models/gate-run-6-h$H)
  service/target/release/scalper-data gate --matrix data/matrices/gate-run-6.jsonl --folds data/models/gate-run-6-h$H/folds.json \
    --binance-costs data/binance-micro/costs-daily-3b.json --fee-taker-bps 4.5 --fee-maker-bps 1.8 --notional 5000 \
    --exit atr --data-root data --out data/reports/gate-run-6-h$H.json
done
```

## Matrix

`data/matrices/gate-run-6.jsonl`: **2,446,102 rows**, 24 assets, 56 features; 2,143,486 in-test-window predictions per horizon.

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

## Per-horizon results (first run, 24 assets)

| horizon | n_trades | sharpe_annualized | gate | ic (pooled) | rank_ic (pooled) | exits stops/targets/time | mean bars held | net total bps | net bps/trade | hit rate | zero-return days | folds traded/21 (positive Sharpe) |
|---:|---:|---:|---|---:|---:|---|---:|---:|---:|---:|---:|---|
| 15 | 5,350 | 0.6007 | FAIL | 0.0258 | 0.0157 | 674/438/4,238 | 13.56 | +86,113.7 | +16.10 | 0.4731 | 360/631 | 18 (11) |
| 30 | 7,094 | 0.7272 | FAIL | 0.0197 | 0.0045 | 1,678/1,239/4,177 | 23.87 | +115,138.0 | +16.23 | 0.4899 | 325/631 | 16 (8) |
| 60 | 8,836 | 0.6718 | FAIL | 0.0214 | -0.0006 | 3,186/2,640/3,010 | 36.86 | +97,661.1 | +11.05 | 0.4783 | 325/631 | 18 (11) |

### Runs 4 → 6 (same taker execution, same folds; run 5's maker numbers alongside)

| | h15 r4 | h15 r6 | h30 r4 | h30 r6 | h60 r4 | h60 r6 | (r5 maker h15/h30/h60) |
|---|---:|---:|---:|---:|---:|---:|---|
| Sharpe | 0.7567 | 0.6007 | 0.6869 | 0.7272 | 0.5127 | 0.6718 | 0.3649 / 0.1883 / 0.4321 |
| pooled IC | 0.0366 | 0.0258 | 0.0216 | 0.0197 | 0.0148 | 0.0214 | 0.0368 / 0.0105 / 0.0133 |
| pooled rank IC | 0.0144 | 0.0157 | 0.0046 | 0.0045 | 0.0002 | -0.0006 | |
| trades | 4,144 | 5,350 | 5,892 | 7,094 | 5,648 | 8,836 | |
| net bps/trade | +30.14 | +16.10 | +16.77 | +16.23 | +10.61 | +11.05 | |
| hit rate | 0.4935 | 0.4731 | 0.4869 | 0.4899 | 0.4717 | 0.4783 | |

## Fold-level detail

### h15
| fold | test window | n_preds | trades | sharpe | ic | rank_ic | pred_abs p50/p90/p99 |
|---:|---|---:|---:|---:|---:|---:|---|
| 0 | 2024-10-30..2024-11-29 | 107,037 | 251 | 2.52 | 0.0224 | 0.0312 | 0.72/3.67/9.46 |
| 1 | 2024-11-29..2024-12-29 | 133,006 | 51 | 4.36 | 0.0076 | 0.0013 | 0.50/3.07/7.92 |
| 2 | 2024-12-29..2025-01-28 | 118,764 | 1874 | -4.94 | 0.0083 | 0.0029 | 1.13/7.01/27.43 |
| 3 | 2025-01-28..2025-02-27 | 118,133 | 1 | 3.43 | 0.0358 | 0.0155 | 0.32/1.58/6.13 |
| 4 | 2025-02-27..2025-03-29 | 108,688 | 291 | -2.53 | -0.0118 | -0.0039 | 0.39/2.73/11.06 |
| 5 | 2025-03-29..2025-04-28 | 99,318 | 0 | n/a | 0.0144 | 0.0162 | 0.04/0.04/1.55 |
| 6 | 2025-04-28..2025-05-28 | 120,869 | 82 | 2.54 | 0.0194 | 0.0186 | 0.29/0.83/5.38 |
| 7 | 2025-05-28..2025-06-27 | 116,489 | 234 | -4.89 | -0.0089 | 0.0113 | 0.57/1.95/8.92 |
| 8 | 2025-06-27..2025-07-27 | 110,212 | 0 | n/a | 0.0355 | 0.0114 | 0.07/0.18/1.74 |
| 9 | 2025-07-27..2025-08-26 | 114,727 | 245 | 2.19 | 0.0432 | 0.0353 | 0.66/2.28/9.64 |
| 10 | 2025-08-26..2025-09-25 | 107,146 | 340 | -2.20 | 0.0257 | 0.0236 | 0.69/2.45/12.05 |
| 11 | 2025-09-25..2025-10-25 | 123,628 | 513 | 3.49 | 0.0808 | 0.0315 | 0.55/3.01/15.66 |
| 12 | 2025-10-25..2025-11-24 | 122,693 | 1112 | 0.84 | 0.0394 | 0.0241 | 0.88/4.63/18.29 |
| 13 | 2025-11-24..2025-12-24 | 93,546 | 173 | -5.08 | 0.0009 | 0.0206 | 0.47/2.13/9.22 |
| 14 | 2025-12-24..2026-01-23 | 87,061 | 101 | -3.53 | -0.0304 | 0.0156 | 0.39/1.42/7.04 |
| 15 | 2026-01-23..2026-02-22 | 86,120 | 0 | n/a | -0.0242 | -0.0006 | 0.09/0.30/2.22 |
| 16 | 2026-02-22..2026-03-24 | 70,222 | 11 | 3.43 | 0.0128 | 0.0212 | 0.28/1.06/4.44 |
| 17 | 2026-03-24..2026-04-23 | 59,266 | 2 | -3.43 | 0.0327 | 0.0312 | 0.26/0.71/2.71 |
| 18 | 2026-04-23..2026-05-23 | 66,912 | 4 | 3.43 | 0.0218 | 0.0353 | 0.33/0.94/2.87 |
| 19 | 2026-05-23..2026-06-22 | 98,193 | 12 | 5.44 | 0.0356 | 0.0258 | 0.32/1.00/4.31 |
| 20 | 2026-06-22..2026-07-22 | 81,456 | 53 | 0.74 | 0.0090 | 0.0198 | 0.49/1.34/5.60 |

### h30
| fold | test window | n_preds | trades | sharpe | ic | rank_ic | pred_abs p50/p90/p99 |
|---:|---|---:|---:|---:|---:|---:|---|
| 0 | 2024-10-30..2024-11-29 | 107,037 | 123 | 3.44 | 0.0206 | 0.0121 | 0.71/3.02/10.04 |
| 1 | 2024-11-29..2024-12-29 | 133,006 | 2413 | -3.74 | 0.0169 | 0.0081 | 2.25/11.12/29.46 |
| 2 | 2024-12-29..2025-01-28 | 118,764 | 1735 | -2.87 | -0.0086 | -0.0043 | 1.45/8.38/35.34 |
| 3 | 2025-01-28..2025-02-27 | 118,133 | 0 | n/a | 0.0248 | -0.0127 | 0.76/1.70/7.52 |
| 4 | 2025-02-27..2025-03-29 | 108,688 | 157 | -2.12 | -0.0261 | 0.0003 | 0.38/2.48/9.53 |
| 5 | 2025-03-29..2025-04-28 | 99,318 | 0 | n/a | 0.0014 | -0.0025 | 0.09/0.39/2.46 |
| 6 | 2025-04-28..2025-05-28 | 120,869 | 33 | 4.31 | 0.0168 | 0.0202 | 0.30/0.84/4.91 |
| 7 | 2025-05-28..2025-06-27 | 116,489 | 141 | -1.79 | -0.0337 | -0.0191 | 0.49/1.71/6.28 |
| 8 | 2025-06-27..2025-07-27 | 110,212 | 0 | n/a | 0.0231 | 0.0115 | 0.23/0.36/0.88 |
| 9 | 2025-07-27..2025-08-26 | 114,727 | 118 | 0.05 | 0.0295 | 0.0209 | 0.54/1.69/7.53 |
| 10 | 2025-08-26..2025-09-25 | 107,146 | 215 | -3.20 | 0.0146 | 0.0354 | 0.62/1.99/11.22 |
| 11 | 2025-09-25..2025-10-25 | 123,628 | 519 | 3.44 | 0.0811 | 0.0085 | 0.64/3.75/19.09 |
| 12 | 2025-10-25..2025-11-24 | 122,693 | 1028 | 7.98 | 0.0475 | 0.0058 | 0.88/4.84/20.91 |
| 13 | 2025-11-24..2025-12-24 | 93,546 | 3 | -1.18 | -0.0182 | 0.0050 | 0.20/1.30/5.08 |
| 14 | 2025-12-24..2026-01-23 | 87,061 | 207 | -3.28 | 0.0006 | 0.0164 | 0.67/2.70/12.46 |
| 15 | 2026-01-23..2026-02-22 | 86,120 | 0 | n/a | 0.0287 | -0.0113 | 0.16/0.41/2.71 |
| 16 | 2026-02-22..2026-03-24 | 70,222 | 119 | 2.12 | -0.0051 | 0.0022 | 0.49/1.91/8.35 |
| 17 | 2026-03-24..2026-04-23 | 59,266 | 45 | -2.97 | -0.0424 | -0.0169 | 0.56/1.60/5.81 |
| 18 | 2026-04-23..2026-05-23 | 66,912 | 52 | 3.41 | 0.0217 | -0.0006 | 0.60/1.81/5.57 |
| 19 | 2026-05-23..2026-06-22 | 98,193 | 0 | n/a | 0.0179 | 0.0019 | 0.32/0.53/1.89 |
| 20 | 2026-06-22..2026-07-22 | 81,456 | 186 | 0.23 | 0.0455 | 0.0304 | 0.89/3.48/11.61 |

### h60
| fold | test window | n_preds | trades | sharpe | ic | rank_ic | pred_abs p50/p90/p99 |
|---:|---|---:|---:|---:|---:|---:|---|
| 0 | 2024-10-30..2024-11-29 | 107,037 | 109 | 3.87 | 0.0255 | 0.0147 | 1.19/4.08/10.98 |
| 1 | 2024-11-29..2024-12-29 | 133,006 | 4213 | -4.75 | 0.0042 | -0.0161 | 4.51/17.08/38.06 |
| 2 | 2024-12-29..2025-01-28 | 118,764 | 388 | 1.93 | 0.0203 | -0.0131 | 2.72/6.14/18.02 |
| 3 | 2025-01-28..2025-02-27 | 118,133 | 0 | n/a | 0.0408 | -0.0117 | 1.74/2.39/9.15 |
| 4 | 2025-02-27..2025-03-29 | 108,688 | 142 | -7.86 | -0.0544 | 0.0014 | 0.77/2.44/9.00 |
| 5 | 2025-03-29..2025-04-28 | 99,318 | 15 | 2.24 | -0.0108 | 0.0097 | 0.44/1.69/5.49 |
| 6 | 2025-04-28..2025-05-28 | 120,869 | 1 | -3.43 | -0.0042 | 0.0011 | 0.46/1.55/5.02 |
| 7 | 2025-05-28..2025-06-27 | 116,489 | 80 | -1.77 | -0.0467 | -0.0363 | 0.60/1.66/5.58 |
| 8 | 2025-06-27..2025-07-27 | 110,212 | 0 | n/a | 0.0074 | 0.0034 | 0.56/1.08/2.62 |
| 9 | 2025-07-27..2025-08-26 | 114,727 | 10 | 3.43 | 0.0325 | -0.0054 | 0.26/0.82/4.22 |
| 10 | 2025-08-26..2025-09-25 | 107,146 | 473 | -2.84 | 0.0248 | 0.0467 | 1.77/4.98/22.43 |
| 11 | 2025-09-25..2025-10-25 | 123,628 | 870 | 3.47 | 0.0710 | 0.0074 | 1.95/6.91/32.42 |
| 12 | 2025-10-25..2025-11-24 | 122,693 | 1348 | 4.93 | 0.0398 | 0.0191 | 2.32/7.63/31.81 |
| 13 | 2025-11-24..2025-12-24 | 93,546 | 0 | n/a | 0.0108 | -0.0084 | 0.17/0.33/2.36 |
| 14 | 2025-12-24..2026-01-23 | 87,061 | 601 | -2.45 | 0.0156 | 0.0124 | 1.81/6.72/29.39 |
| 15 | 2026-01-23..2026-02-22 | 86,120 | 225 | 4.60 | 0.0938 | 0.0218 | 0.96/3.80/12.89 |
| 16 | 2026-02-22..2026-03-24 | 70,222 | 180 | 1.69 | 0.0044 | 0.0182 | 1.24/3.71/11.23 |
| 17 | 2026-03-24..2026-04-23 | 59,266 | 52 | -1.63 | -0.0595 | -0.0464 | 0.95/2.72/8.42 |
| 18 | 2026-04-23..2026-05-23 | 66,912 | 60 | 6.59 | -0.0116 | -0.0069 | 0.89/2.82/8.19 |
| 19 | 2026-05-23..2026-06-22 | 98,193 | 5 | 2.55 | 0.0229 | -0.0285 | 0.68/1.16/4.63 |
| 20 | 2026-06-22..2026-07-22 | 81,456 | 64 | 4.22 | 0.0529 | 0.0059 | 1.16/3.27/10.83 |

## Per-asset breakdown (first run)

### h15
| asset | trades | total_net_bps | net/trade | hit rate | mean threshold bps |
|---|---:|---:|---:|---:|---:|
| FARTCOIN | 936 | +19,190.7 | +20.50 | 0.494 | 22.05 |
| SUI | 232 | +12,510.5 | +53.92 | 0.453 | 15.63 |
| PUMP | 174 | +11,306.6 | +64.98 | 0.506 | 32.92 |
| ENA | 284 | +9,156.8 | +32.24 | 0.489 | 21.35 |
| VIRTUAL | 308 | +9,116.9 | +29.60 | 0.477 | 20.02 |
| kPEPE | 345 | +7,797.7 | +22.60 | 0.487 | 15.45 |
| AAVE | 192 | +6,979.4 | +36.35 | 0.458 | 15.98 |
| LINK | 221 | +4,232.5 | +19.15 | 0.421 | 15.37 |
| UNI | 171 | +4,061.7 | +23.75 | 0.409 | 18.06 |
| DOGE | 328 | +3,999.8 | +12.19 | 0.488 | 14.72 |
| TAO | 167 | +1,772.2 | +10.61 | 0.467 | 17.03 |
| ETHFI | 143 | +1,596.8 | +11.17 | 0.503 | 21.98 |
| NEAR | 85 | +1,113.5 | +13.10 | 0.529 | 22.06 |
| ZEC | 374 | +927.9 | +2.48 | 0.489 | 24.27 |
| XRP | 251 | +625.6 | +2.49 | 0.398 | 14.62 |
| XMR | 16 | +297.3 | +18.58 | 0.562 | 21.05 |
| SOL | 225 | +89.7 | +0.40 | 0.529 | 14.95 |
| CRV | 18 | -253.0 | -14.06 | 0.556 | 46.31 |
| ETH | 152 | -929.0 | -6.11 | 0.474 | 13.93 |
| XPL | 242 | -1,080.5 | -4.46 | 0.471 | 28.48 |
| KAITO | 78 | -1,084.6 | -13.91 | 0.436 | 27.71 |
| BTC | 107 | -1,134.4 | -10.60 | 0.308 | 13.62 |
| LIT | 9 | -1,187.7 | -131.97 | 0.333 | 41.94 |
| WLD | 292 | -2,992.9 | -10.25 | 0.476 | 18.02 |

### h30
| asset | trades | total_net_bps | net/trade | hit rate | mean threshold bps |
|---|---:|---:|---:|---:|---:|
| SUI | 344 | +18,292.5 | +53.18 | 0.465 | 15.63 |
| FARTCOIN | 861 | +16,518.5 | +19.19 | 0.467 | 22.05 |
| VIRTUAL | 412 | +11,484.9 | +27.88 | 0.483 | 20.02 |
| ENA | 408 | +10,403.8 | +25.50 | 0.515 | 21.35 |
| kPEPE | 493 | +9,819.0 | +19.92 | 0.491 | 15.45 |
| AAVE | 341 | +8,356.0 | +24.50 | 0.472 | 15.98 |
| PUMP | 142 | +8,154.9 | +57.43 | 0.479 | 32.92 |
| ZEC | 412 | +7,264.2 | +17.63 | 0.522 | 24.27 |
| DOGE | 391 | +5,331.3 | +13.64 | 0.491 | 14.72 |
| UNI | 286 | +4,956.7 | +17.33 | 0.451 | 18.06 |
| LINK | 369 | +4,527.2 | +12.27 | 0.474 | 15.37 |
| XPL | 271 | +3,739.9 | +13.80 | 0.524 | 28.48 |
| ETH | 183 | +2,909.0 | +15.90 | 0.579 | 13.93 |
| XRP | 412 | +2,674.7 | +6.49 | 0.493 | 14.62 |
| SOL | 266 | +2,576.8 | +9.69 | 0.523 | 14.95 |
| XMR | 47 | +1,615.8 | +34.38 | 0.638 | 21.05 |
| BTC | 133 | +1,538.3 | +11.57 | 0.519 | 13.62 |
| KAITO | 74 | +1,072.7 | +14.50 | 0.500 | 27.71 |
| NEAR | 184 | +996.4 | +5.42 | 0.511 | 22.06 |
| ETHFI | 215 | +919.0 | +4.27 | 0.460 | 21.98 |
| TAO | 292 | +779.2 | +2.67 | 0.493 | 17.03 |
| LIT | 37 | -1,987.3 | -53.71 | 0.459 | 41.94 |
| CRV | 73 | -2,701.7 | -37.01 | 0.452 | 46.31 |
| WLD | 448 | -4,103.8 | -9.16 | 0.467 | 18.02 |

### h60
| asset | trades | total_net_bps | net/trade | hit rate | mean threshold bps |
|---|---:|---:|---:|---:|---:|
| FARTCOIN | 478 | +24,453.5 | +51.16 | 0.490 | 22.05 |
| VIRTUAL | 467 | +12,771.5 | +27.35 | 0.501 | 20.02 |
| SUI | 435 | +12,172.9 | +27.98 | 0.478 | 15.63 |
| ENA | 467 | +10,692.0 | +22.90 | 0.469 | 21.35 |
| kPEPE | 596 | +10,685.1 | +17.93 | 0.482 | 15.45 |
| ZEC | 706 | +9,767.2 | +13.83 | 0.518 | 24.27 |
| PUMP | 249 | +9,556.2 | +38.38 | 0.486 | 32.92 |
| DOGE | 492 | +7,564.9 | +15.38 | 0.494 | 14.72 |
| AAVE | 444 | +5,899.7 | +13.29 | 0.446 | 15.98 |
| LINK | 465 | +5,556.4 | +11.95 | 0.465 | 15.37 |
| UNI | 442 | +3,226.4 | +7.30 | 0.457 | 18.06 |
| XRP | 484 | +2,325.0 | +4.80 | 0.479 | 14.62 |
| NEAR | 272 | +2,173.1 | +7.99 | 0.463 | 22.06 |
| ETH | 244 | +1,176.0 | +4.82 | 0.500 | 13.93 |
| SOL | 305 | +566.3 | +1.86 | 0.466 | 14.95 |
| TAO | 351 | +57.6 | +0.16 | 0.464 | 17.03 |
| ETHFI | 339 | -66.3 | -0.20 | 0.475 | 21.98 |
| KAITO | 169 | -630.3 | -3.73 | 0.497 | 27.71 |
| BTC | 173 | -876.1 | -5.06 | 0.457 | 13.62 |
| XMR | 121 | -1,450.5 | -11.99 | 0.512 | 21.05 |
| XPL | 428 | -1,981.6 | -4.63 | 0.479 | 28.48 |
| LIT | 131 | -3,876.6 | -29.59 | 0.473 | 41.94 |
| WLD | 500 | -5,620.6 | -11.24 | 0.468 | 18.02 |
| CRV | 78 | -6,480.6 | -83.08 | 0.333 | 46.31 |

## Feature gain: what the BTC context contributed

| horizon | six BTC-context features together | each | own-asset `tk_*` (Program 2's twelve) | top features overall |
|---:|---:|---|---:|---|

| 15 | 31.3% | btc_tk_ret_30s 6.9%, btc_tk_imb_5m 6.3%, btc_tk_ret_10s 6.2%, btc_tk_intensity_10s 5.6%, btc_tk_imb_30s 5.6%, rel_tk_ret_30s 0.7% | 3.7% | btc_ret_5 8.5%, tod_sin 8.0%, tod_cos 7.0%, btc_tk_ret_30s 6.9%, btc_tk_imb_5m 6.3% |
| 30 | 24.1% | btc_tk_ret_30s 6.2%, btc_tk_ret_10s 5.1%, btc_tk_intensity_10s 4.5%, btc_tk_imb_5m 4.1%, btc_tk_imb_30s 3.8%, rel_tk_ret_30s 0.3% | 2.4% | tod_sin 10.4%, tod_cos 9.4%, vol_60 6.8%, dow 6.4%, btc_ret_5 6.3% |
| 60 | 15.7% | btc_tk_ret_30s 4.8%, btc_tk_ret_10s 3.5%, btc_tk_intensity_10s 2.9%, btc_tk_imb_5m 2.4%, btc_tk_imb_30s 2.0%, rel_tk_ret_30s 0.2% | 2.7% | tod_sin 11.5%, dow 10.6%, tod_cos 10.5%, vol_60 8.4%, ret_60 6.3% |

## Fee fixed-point step

Projected 30-day volume: h15 $2,543,581.62, h30 $3,372,741.68, h60 $4,200,950.87 —
inside the disputed VIP1 candidate band as in runs 2, 4 and 5; tier cannot be
confidently mapped; no unverified fee is tested. **PROVISIONAL, same
disposition.** A lower tier cannot plausibly close a gap of this size.

## Drop-rule step (§5)

Negative `total_net_bps` in all three horizon reports: **CRV, LIT, WLD**.
(h15-negative: BTC, CRV, ETH, KAITO, LIT, WLD, XPL; h30: CRV, LIT, WLD; h60: BTC, CRV, ETHFI, KAITO, LIT, WLD, XMR, XPL.)

The step is exercised exactly as written (frozen universe untouched; filtered copy `data/scalper-universe-run6b.json`; one rebuild, one refit, one re-run; the second run's verdict is the verdict):

| horizon | n_trades | sharpe_annualized | gate | ic (pooled) | rank_ic (pooled) | exits stops/targets/time | mean bars held | net total bps | net bps/trade | hit rate | zero-return days | folds traded/21 (positive Sharpe) |
|---:|---:|---:|---|---:|---:|---|---:|---:|---:|---:|---:|---|
| 15 | 5,144 | 0.6671 | FAIL | 0.0278 | 0.0124 | 655/395/4,094 | 13.65 | +102,401.0 | +19.91 | 0.4817 | 356/631 | 16 (8) |
| 30 | 9,788 | 0.4368 | FAIL | 0.0177 | 0.0057 | 2,356/1,700/5,732 | 24.06 | +69,364.0 | +7.09 | 0.4764 | 314/631 | 16 (7) |
| 60 | 9,180 | 0.5586 | FAIL | 0.0215 | 0.0012 | 3,424/2,672/3,084 | 36.87 | +86,847.2 | +9.46 | 0.4702 | 342/631 | 16 (9) |

| | h15 first | h15 re-run | h30 first | h30 re-run | h60 first | h60 re-run |
|---|---:|---:|---:|---:|---:|---:|
| Sharpe | 0.6007 | 0.6671 | 0.7272 | 0.4368 | 0.6718 | 0.5586 |
| pooled IC | 0.0258 | 0.0278 | 0.0197 | 0.0177 | 0.0214 | 0.0215 |
| trades | 5,350 | 5,144 | 7,094 | 9,788 | 8,836 | 9,180 |

#### h15 (re-run)
| asset | trades | total_net_bps | net/trade | hit rate | mean threshold bps |
|---|---:|---:|---:|---:|---:|
| FARTCOIN | 722 | +22,183.3 | +30.72 | 0.488 | 22.05 |
| SUI | 270 | +17,628.5 | +65.29 | 0.481 | 15.63 |
| VIRTUAL | 216 | +10,529.6 | +48.75 | 0.509 | 20.02 |
| PUMP | 175 | +10,062.5 | +57.50 | 0.514 | 32.92 |
| ENA | 276 | +8,758.3 | +31.73 | 0.511 | 21.35 |
| kPEPE | 383 | +7,477.7 | +19.52 | 0.483 | 15.45 |
| UNI | 151 | +7,052.7 | +46.71 | 0.404 | 18.06 |
| AAVE | 160 | +7,043.3 | +44.02 | 0.438 | 15.98 |
| LINK | 211 | +6,159.2 | +29.19 | 0.436 | 15.37 |
| ZEC | 400 | +4,939.5 | +12.35 | 0.515 | 24.27 |
| SOL | 212 | +2,220.1 | +10.47 | 0.495 | 14.95 |
| DOGE | 399 | +2,156.8 | +5.41 | 0.491 | 14.72 |
| ETHFI | 176 | +1,607.3 | +9.13 | 0.506 | 21.98 |
| XRP | 246 | +984.9 | +4.00 | 0.459 | 14.62 |
| TAO | 166 | +933.7 | +5.62 | 0.476 | 17.03 |
| XMR | 7 | +226.7 | +32.38 | 0.429 | 21.05 |
| NEAR | 89 | -189.1 | -2.12 | 0.494 | 22.06 |
| BTC | 92 | -322.9 | -3.51 | 0.424 | 13.62 |
| ETH | 158 | -843.3 | -5.34 | 0.443 | 13.93 |
| XPL | 499 | -2,725.7 | -5.46 | 0.485 | 28.48 |
| KAITO | 136 | -3,482.1 | -25.60 | 0.449 | 27.71 |

#### h30 (re-run)
| asset | trades | total_net_bps | net/trade | hit rate | mean threshold bps |
|---|---:|---:|---:|---:|---:|
| SUI | 600 | +16,282.5 | +27.14 | 0.480 | 15.63 |
| FARTCOIN | 1131 | +13,982.3 | +12.36 | 0.476 | 22.05 |
| ENA | 602 | +8,770.2 | +14.57 | 0.475 | 21.35 |
| ZEC | 586 | +8,229.8 | +14.04 | 0.522 | 24.27 |
| VIRTUAL | 490 | +7,895.2 | +16.11 | 0.467 | 20.02 |
| PUMP | 193 | +7,806.5 | +40.45 | 0.503 | 32.92 |
| AAVE | 532 | +6,414.4 | +12.06 | 0.472 | 15.98 |
| DOGE | 656 | +3,544.3 | +5.40 | 0.483 | 14.72 |
| LINK | 567 | +3,218.5 | +5.68 | 0.457 | 15.37 |
| XMR | 62 | +2,650.7 | +42.75 | 0.597 | 21.05 |
| kPEPE | 755 | +2,427.9 | +3.22 | 0.470 | 15.45 |
| UNI | 436 | +2,243.3 | +5.15 | 0.459 | 18.06 |
| XPL | 358 | +639.3 | +1.79 | 0.480 | 28.48 |
| ETH | 288 | -43.2 | -0.15 | 0.507 | 13.93 |
| SOL | 391 | -220.7 | -0.56 | 0.486 | 14.95 |
| TAO | 396 | -920.8 | -2.33 | 0.470 | 17.03 |
| BTC | 217 | -1,014.9 | -4.68 | 0.433 | 13.62 |
| NEAR | 306 | -1,792.8 | -5.86 | 0.454 | 22.06 |
| XRP | 644 | -1,820.0 | -2.83 | 0.491 | 14.62 |
| ETHFI | 371 | -2,983.2 | -8.04 | 0.450 | 21.98 |
| KAITO | 207 | -5,945.0 | -28.72 | 0.435 | 27.71 |

#### h60 (re-run)
| asset | trades | total_net_bps | net/trade | hit rate | mean threshold bps |
|---|---:|---:|---:|---:|---:|
| SUI | 516 | +15,908.4 | +30.83 | 0.461 | 15.63 |
| FARTCOIN | 715 | +13,740.9 | +19.22 | 0.455 | 22.05 |
| kPEPE | 671 | +9,699.7 | +14.46 | 0.477 | 15.45 |
| VIRTUAL | 606 | +9,611.9 | +15.86 | 0.474 | 20.02 |
| ENA | 607 | +7,649.7 | +12.60 | 0.455 | 21.35 |
| DOGE | 517 | +6,741.2 | +13.04 | 0.482 | 14.72 |
| PUMP | 224 | +6,464.5 | +28.86 | 0.464 | 32.92 |
| AAVE | 519 | +6,346.7 | +12.23 | 0.449 | 15.98 |
| ZEC | 763 | +6,139.5 | +8.05 | 0.505 | 24.27 |
| LINK | 574 | +4,790.4 | +8.35 | 0.465 | 15.37 |
| XPL | 399 | +2,938.7 | +7.37 | 0.489 | 28.48 |
| UNI | 477 | +2,471.2 | +5.18 | 0.463 | 18.06 |
| XMR | 112 | +1,376.6 | +12.29 | 0.580 | 21.05 |
| ETH | 218 | +1,205.0 | +5.53 | 0.495 | 13.93 |
| SOL | 345 | +121.2 | +0.35 | 0.475 | 14.95 |
| KAITO | 136 | +19.3 | +0.14 | 0.507 | 27.71 |
| TAO | 411 | -296.2 | -0.72 | 0.472 | 17.03 |
| BTC | 144 | -825.3 | -5.73 | 0.444 | 13.62 |
| NEAR | 316 | -1,702.6 | -5.39 | 0.449 | 22.06 |
| XRP | 529 | -2,590.7 | -4.90 | 0.459 | 14.62 |
| ETHFI | 381 | -2,963.0 | -7.78 | 0.438 | 21.98 |


## VERDICT

Read from the §5 drop re-run (the verdict):

**h15: FAIL** (sharpe_annualized 0.667065, 1.33 below 2.0).
**h30: FAIL** (sharpe_annualized 0.436760, 1.56 below).
**h60: FAIL** (sharpe_annualized 0.558605, 1.44 below).

**Amendment 6 §6.2's FAIL branch is invoked.** No window, feature, exit, cost or threshold term is revisited against these numbers. Program 3 is closed. Every direction tried so far — Programs 1 (four feature sets, taker), 2 (own-asset tick flow + maker) and 3 (cross-asset tick context, taker) — has been run under pre-registration on the same clean 24-month record, and none clears the gate.

## What the diagnostics say

**BTC tick context: heavily used in-sample, no lift out-of-sample.** The
six features take 31.3% / 24.1% / 15.7% of split gain at h15/h30/h60 —
four of them (`btc_tk_ret_30s`, `btc_tk_imb_5m`, `btc_tk_ret_10s`,
`btc_tk_intensity_10s`) are among the top-six features at h15, above
everything but `btc_ret_5` and time-of-day, and Program 2's own-asset tick
features shrink to 2–4% beside them. Yet pooled out-of-sample IC is
0.0258 / 0.0197 / 0.0214 against run 4's 0.0366 / 0.0216 / 0.0148, and
Sharpe is 0.60 / 0.73 / 0.67 against 0.76 / 0.69 / 0.51 — different noise,
same level. The trees find sub-minute BTC flow and return highly
splittable and it does not generalize into ranking skill an hour later:
either the lead-lag is priced away by the time a 1-minute bar closes, or
it is already what `btc_ret_5` carried, or its in-sample structure is
regime-specific. On this record, cross-asset tick context is not the
missing edge either.

**Trade economics are run 4's.** Same execution, same threshold (21.56 bps
mean), 5,350 / 7,094 / 8,836 trades vs run 4's 4,144 / 5,892 / 5,648, net
+16.10 / +16.23 / +11.05 bps per trade vs +30.14 / +16.77 / +10.61. Entries remain
rare because predictions remain small.

## Provenance

Reports `data/reports/gate-run-6-h*.json`, `data/reports/gate-run-6b-h*.json`; folds `data/models/gate-run-6-h*/`; matrix `data/matrices/gate-run-6.jsonl`; tape `data/binance-micro/tape/`. Runs 1–5 stay on file as written.
