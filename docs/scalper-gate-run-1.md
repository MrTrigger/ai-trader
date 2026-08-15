# Crypto scalper — gate run 1 (Binance venue, Amendment 1)

> **Bottom line up front: this run is NOT gate-eligible.** All four steps
> of the pipeline ran cleanly and produced well-formed reports at every
> horizon, and the fee fixed-point step was carried out per the
> pre-registered rule — but Amendment 1's condition 1 (≥18 months of
> matrix span) is not met in substance. See "Eligibility checklist" below
> for why, and read every number in this document as diagnostic, not as
> the gate verdict that decides capital allocation.

**Run date:** 2026-08-15
**Worktree:** `.claude/worktrees/scalper-plan3c` (`data/` is gitignored, per-worktree, not committed)
**Backfill:** completed before this run (`var/live/scalper-backfill.log`,
`scalper-backfill: done (2024-08-01..2026-08-16, 24 mapped coins)`, exit
status 0) — no re-pull was performed in this session, per the runbook's
rule against deepening `data/perp`/`data/binance-micro` between a run and
its re-run.

---

## Eligibility checklist (Amendment 1)

| # | Condition | Met? | Evidence |
|---|---|---|---|
| 1 | ≥18 months of matrix span | **NOT MET (in substance)** | The `training-matrix` command was run with `--start 2024-08-01 --end 2026-08-15` (24.5 months requested — the literal command flags clear 18 months). But every one of the 24 assets' **kept** rows (the rows that actually survive fs-2's all-features-Some requirement) start on **2026-01-15**, not 2024-08-01 — see "Matrix" below. Actual per-asset matrix span is **~210 days (~6.9 months)**, not ≥18 months. Root cause: the ±0.2% percentage band first appears in the ingested `bookDepth` data at `data/binance-micro/book/BTCUSDT/2026-01-15.jsonl`, ts `1768460460` (2026-01-15 07:01 UTC) — the snapshot immediately before it (ts `1768460400`, 07:00 UTC, same file) still carries only the `-1.0`/`1.0` bands; every day before 2026-01-15 has only those two keys (confirmed by inspecting `data/binance-micro/book/BTCUSDT/2024-08-01.jsonl` through `2026-01-14.jsonl`). The ±0.2% level is simply not present in the ingested band set before that point — this may reflect a change in what Binance's archive publishes, or a level that only becomes representable once book depth data is dense enough; the snapshot doesn't establish which. Either way, fs-2's `depth_imb_02` needs `bid_02`/`ask_02`, so every row before 2026-01-15 07:01 UTC is dropped by `training-matrix`'s all-Some requirement (matrix rows in practice start 2026-01-15 08:00 UTC, the first full bar-close after that). This is a data-availability limitation in the ingested archive, not a bug in this codebase's matrix logic — flow/metrics/funding coverage for the same assets does reach back to 2024-08-01 (or each asset's own listing date), confirming the band gap is the sole bottleneck. |
| 2 | Every mapped, 90-day-eligible candidate is in the matrix | Met | 24 of 25 universe candidates are Binance-mapped (`HYPE` has `binance_um: null`, correctly excluded). All 24 appear in the matrix manifest's `assets` list and all 24 produced rows (see per-asset table below) — no candidate skipped with a "no bars" warning. |
| 3 | fs-2 features (`fs-rust-scalper-2`) | Met | Manifest line: `"feature_set_version":"fs-rust-scalper-2"`, 38 features listed (26 fs-1 + 12 fs-2), matching the spec. |
| 4 | Time-varying costs via `binance-costs` + `gate --binance-costs` | Met (mechanically) | `binance-costs` ran successfully for all 24 mapped assets; `gate` was invoked with `--binance-costs` (not `--costs`) for all three horizons. `days_without_costs` totals only 3 (all on `KAITO`) per horizon, so cost lookups never hard-fail inside the matrix-covered window — but that field undercounts thin-book days that still resolve via the 14-day fallback; see "Step 2 — Binance costs" below for the 78 genuinely thin in-window days this masks. |
| 5 | All three horizons (15/30/60) run, every run, all reported | Met | See per-horizon table below. |

**Conclusion: this run does not clear Amendment 1's eligibility bar.**
Condition 1 fails because the requested 24.5-month window is not the
window the data actually supports — real per-asset coverage is bounded to
~7 months by the ±0.2% band being absent from the ingested `bookDepth`
data before 2026-01-15, discovered during this run, not anticipated by
the runbook or the plan. The pipeline itself
worked correctly end to end (conditions 2-5 all met); the failure is a
data-availability ceiling, external to this codebase. **No PASS/FAIL
result below should be read as evidence for or against the strategy at
capital-allocation stakes** — exactly the caveat the smoke run
(`docs/scalper-research-smoke.md`) carried, for a different underlying
reason.

---

## Commands run, in order

### Step 0 — build

```
cargo build --release -p scalper-data
```

Run from `service/`, 2026-08-15 13:26:05 UTC. Completed in **~8s**
(`Finished \`release\` profile [optimized] target(s) in 7.97s`). No source
changes since the last build; this recompiled `scalper-data` only.

### Step 1 — training matrix (fs-2, full universe, full requested span)

```
service/target/release/scalper-data training-matrix \
  --data-root data --micro-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --out data/matrices/gate-run-1.jsonl --stride 5
```

Ran 13:26:39–13:30:39 UTC (**~4m00s**). Exit 0.

Per-asset `kept N of M (book from ..., flow from ..., metrics from ...,
funding from ...)` lines (all 24 mapped assets produced rows; none
skipped):

| asset | kept | of | book from | flow from | metrics from | funding from |
|---|---:|---:|---|---|---|---|
| BTC | 53,619 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| ETH | 53,674 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| SOL | 53,443 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| PUMP | 8,912 | 140,568 | 2026-01-15 | 2025-04-12 | 2025-04-12 | 2025-04-12 |
| ZEC | 52,396 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| XRP | 50,327 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| LIT | 15,066 | 213,774 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| CRV | 536 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| ETHFI | 1,197 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| KAITO | 8,777 | 155,364 | 2026-01-15 | 2025-02-20 | 2025-02-20 | 2025-02-20 |
| WLD | 19,887 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| XMR | 9,164 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| FARTCOIN | 3,618 | 173,151 | 2026-01-15 | 2024-12-20 | 2024-12-20 | 2024-12-20 |
| DOGE | 43,563 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| kPEPE | 39,289 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| SUI | 27,217 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| NEAR | 12,315 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| TAO | 30,129 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| UNI | 6,252 | 213,984 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| XPL | 9,965 | 102,702 | 2026-01-15 | 2025-08-22 | 2025-08-22 | 2025-08-22 |
| VIRTUAL | 7,280 | 176,106 | 2026-01-15 | 2024-12-10 | 2024-12-10 | 2024-12-10 |
| AAVE | 13,053 | 214,272 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| ENA | 14,799 | 214,272 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |
| LINK | 7,218 | 214,272 | 2026-01-15 | 2024-08-01 | 2024-08-01 | 2024-08-01 |

**Total: 541,696 rows across 24 assets**, written to
`data/matrices/gate-run-1.jsonl`. Manifest confirms
`feature_set_version = fs-rust-scalper-2`, 38 features, `horizons_min =
[15,30,60]`, `stride_min = 5`.

**Actual per-asset row span** (measured from the matrix file's own `ts`
values, not the requested command window): every asset's kept rows run
from **2026-01-15 to between 2026-07-15 and 2026-08-13** (asset-dependent
on how current its Binance listing/backfill is) — **~205–210 days
(~6.7–6.9 months)** per asset. This is the number that matters for
eligibility condition 1, not the 24.5-month `--start`/`--end` window.
This uniform ~7-month *date* span hides very uneven *row density*: kept
rows range from CRV's 536 and ETHFI's 1,197 up to BTC's 53,619 (a 100x
spread) — the low-count assets have long stretches of the ~7-month window
missing intermediate warm rows (thin trading, gaps in one of the four
joined sources, etc.), not just a shorter calendar span.

### Step 2 — Binance costs

```
service/target/release/scalper-data binance-costs \
  --data-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --notional 5000 \
  --out data/binance-micro/costs-daily.json
```

Ran 13:34:04–13:34:45 UTC (**~41s**). Exit 0. Per-asset day counts and thin
days (`impact_bps: null`) over the full requested 744-745-day window:

| asset | days | thin days |
|---|---:|---:|
| BTC | 744 | 532 |
| ETH | 744 | 532 |
| SOL | 744 | 532 |
| XRP | 744 | 532 |
| DOGE | 744 | 532 |
| kPEPE | 744 | 532 |
| ZEC | 744 | 532 |
| CRV | 744 | 601 |
| ETHFI | 744 | 532 |
| WLD | 744 | 532 |
| XMR | 744 | 532 |
| SUI | 744 | 532 |
| NEAR | 744 | 532 |
| TAO | 744 | 532 |
| UNI | 744 | 532 |
| AAVE | 744 | 532 |
| ENA | 744 | 532 |
| LINK | 744 | 532 |
| PUMP | 464 | 252 |
| LIT | 419 | 207 |
| KAITO | 541 | 338 |
| FARTCOIN | 603 | 391 |
| XPL | 358 | 146 |
| VIRTUAL | 613 | 401 |

**~532 of 744 days (~71%) are "thin" for every asset with a full-span
listing** — most of this is the same band gap as condition 1: `impact_bps`
needs the ±0.2% band, which isn't in the ingested set before 2026-01-15,
so every earlier day is marked thin regardless of the book's actual
real-world depth. **But that pre-band majority is not the whole thin-day
story, and the claim that it "does not corrupt the gate's cost lookups for
the window that matters" is overstated.** Restricting the thin-day count
to each asset's own matrix-covered window (i.e. counting only days on or
after that asset's first kept row) still leaves **78 genuinely thin $5k
days inside the window that actually trains and gates**: CRV has 69 thin
days between 2026-02-06 and 2026-04-28, and KAITO — the single largest
contributor to both h15's and h30's net P&L (see per-asset breakdown
below) — has 9: 2026-06-01, 06-05, 06-06, 06-10, 06-24, 06-25, 06-26,
07-02, 07-03. On each of those days, the recorded ±0.2% book genuinely
could not absorb a $5,000 clip, per the pre-registered impact model.
`days_without_costs` (3, all on KAITO, in every gate report below) does
**not** capture this — it only counts days where the *entry-day-or-prior-
14-days* lookup fails entirely and a prediction never gets costed at all;
a thin day whose cost lookup falls back to a nearby priced day is silently
absorbed into the round-trip cost and never counted as "without costs."
So the true rate of within-window thin-book days is undercounted by that
field, not zero.

### Step 3 — per-horizon fold fit + gate

For each horizon, `walk_forward_scalper.py` then `scalper-data gate`, run
from `training/` and repo root respectively:

```
uv run python walk_forward_scalper.py --matrix ../data/matrices/gate-run-1.jsonl --horizon <H> --out-dir ../data/models/gate-run-1-h<H>
service/target/release/scalper-data gate --matrix data/matrices/gate-run-1.jsonl --folds data/models/gate-run-1-h<H>/folds.json --binance-costs data/binance-micro/costs-daily.json --fee-taker-bps 4.5 --fee-maker-bps 1.8 --notional 5000 --out data/reports/gate-run-1-h<H>.json
```

| horizon | fold-fit runtime | fold-fit result | gate runtime | gate result |
|---:|---|---|---|---|
| 15 | 13:35:34–13:36:26 UTC (~52s) | 4 folds (`fold-0`..`fold-3`, 223,473→467,680 rows) | 13:36:43–13:36:56 UTC (~12s) | 4 folds, 468 trades, **PASS**, sharpe=6.3059 |
| 30 | 13:37:05–13:37:41 UTC (~37s) | 4 folds, 223,449→467,657 rows | 13:37:49–13:37:59 UTC (~10s) | 4 folds, 605 trades, **PASS**, sharpe=2.0620 |
| 60 | 13:38:11–13:38:46 UTC (~35s) | 4 folds, 223,401→467,615 rows | 13:38:53–13:39:01 UTC (~8s) | 4 folds, 170 trades, **FAIL**, sharpe=−2.9575 |

The anchored-expanding-window default (90-day train floor, 30-day test,
30-day step) produced **4 folds** per horizon over the ~7-month actual
matrix span (all folds' training window starts 2026-01-15, the earliest
usable date; test windows: fold0 2026-04-15..05-15, fold1 2026-05-15..06-14,
fold2 2026-06-14..07-14, fold3 2026-07-14..08-13). All training slices
(223k–468k rows) are comfortably above `MIN_ROWS = 50,000`.

---

## Per-horizon results

| horizon | n_trades | sharpe_annualized | gate | gate_threshold | ic (pooled) | rank_ic (pooled) | days_without_costs (total) | projected_30d_volume_usd |
|---:|---:|---:|---|---:|---:|---:|---:|---:|
| 15 | 468 | 6.305897 | PASS | 2.0 | 0.078672 | 0.062923 | 3 | 1,160,330.58 |
| 30 | 605 | 2.062021 | PASS | 2.0 | 0.033607 | 0.027566 | 3 | 1,500,000.00 |
| 60 | 170 | −2.957502 | FAIL | 2.0 | −0.009713 | −0.003896 | 3 | 421,487.60 |

Fold-level detail (`ic`/`rank_ic` are each fold's own model; `pred_abs_p50
/p90/p99` are that fold's predicted |return| percentiles, in bps):

| horizon | fold | test window | n_trades | sharpe | ic | rank_ic | pred_abs p50 | p90 | p99 |
|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 15 | 0 | 2026-04-15..05-15 | 0 | — | 0.0178 | 0.0279 | 0.885 | 1.015 | 1.662 |
| 15 | 1 | 2026-05-15..06-14 | 0 | — | −0.0078 | 0.0060 | 0.544 | 0.871 | 2.341 |
| 15 | 2 | 2026-06-14..07-14 | 0 | — | 0.0233 | 0.0249 | 0.536 | 0.608 | 1.656 |
| 15 | 3 | 2026-07-14..08-13 | 468 | 14.9122 | 0.2145 | 0.2136 | 2.197 | 7.703 | 21.336 |
| 30 | 0 | 2026-04-15..05-15 | 0 | — | 0.0120 | 0.0122 | 1.632 | 2.373 | 4.485 |
| 30 | 1 | 2026-05-15..06-14 | 0 | — | −0.0152 | −0.0034 | 1.027 | 2.116 | 4.342 |
| 30 | 2 | 2026-06-14..07-14 | 0 | — | 0.0311 | 0.0323 | 0.991 | 2.010 | 4.829 |
| 30 | 3 | 2026-07-14..08-13 | 605 | 4.0947 | 0.0919 | 0.0932 | 2.903 | 9.381 | 26.454 |
| 60 | 0 | 2026-04-15..05-15 | 0 | — | −0.0039 | 0.0056 | 3.345 | 4.460 | 8.213 |
| 60 | 1 | 2026-05-15..06-14 | 170 | −5.9857 | −0.0173 | −0.0186 | 2.803 | 8.316 | 17.755 |
| 60 | 2 | 2026-06-14..07-14 | 0 | — | 0.0266 | 0.0386 | 1.959 | 2.570 | 4.773 |
| 60 | 3 | 2026-07-14..08-13 | 0 | — | −0.0047 | −0.0175 | 1.759 | 2.430 | 3.519 |

`excluded_thin_books` is `[]` for all three horizons — no asset was
structurally excluded.

---

## Fee fixed-point step (Amendment 1)

`overall.projected_30d_volume_usd` from the three run-1 reports:

| horizon | projected_30d_volume_usd |
|---:|---:|
| 15 | 1,160,330.58 |
| 30 | 1,500,000.00 |
| 60 | 421,487.60 |

Maximum: **$1,500,000** (h30), computed as `n_trades × 2 × notional ÷
(121 OOS days) × 30` — i.e. the total round-trip notional over the whole
121-day out-of-sample record, annualized down to a 30-day rate. That
smooths over the trade concentration documented below: since every h15
trade and every h30 trade came from a single active ~30-day fold (fold 3,
2026-07-14..08-13 — see "What the diagnostics say"), the volume that
month *actually* traded was **$4,680,000** for h15 (468 × 2 × $5,000) and
**$6,050,000** for h30 (605 × 2 × $5,000) — 3-4x the smoothed
`projected_30d_volume_usd` figure. Whichever figure the fixed-point rule
is meant to use, an authenticated fee fetch needs to resolve tier
thresholds up to at least **~$6M**, not just ~$1.5M.

Mapping to a tier using `docs/binance-um-fee-table-2026-08.md` **by the
volume criterion only**:

- The snapshot **verifies exactly one tier**: **VIP0** (no stated volume
  floor — "regular user," the default tier), fetched directly from a live
  Binance FAQ page. **VIP9** (floor ≥$30,000,000,000 30-day volume) is
  **cited from four secondary aggregator sites, not independently
  verified** — the snapshot is explicit that this row is "directional
  only." Even on that unverified figure, $1.5M–$6M is nowhere near VIP9's
  floor, so this run is confidently not VIP9 regardless.
- The snapshot's "Gap" section explicitly declines to give a reliable
  VIP1 threshold: it cites **four** disagreeing candidate figures from
  secondary (non-Binance) sources — **$250k**, **$1M**, **$5M**, and
  **$15M** — plus a fee rate attached to the $250k citation that the
  snapshot itself flags as "almost certainly contamination" (it's *higher*
  than VIP0's rate, which a real volume-discount tier cannot be).
- **$1,500,000–$6,050,000 straddles that disputed range**: it exceeds two
  of the four cited candidates ($250k, $1M) and falls short of the other
  two ($5M, $15M). There is no verified number in the snapshot to resolve
  this either way, and the *actually-traded* $6.05M figure exceeds three
  of the four candidates.

Per the pre-registered rule: *"If the fee-table snapshot doesn't cover the
mapped tier... re-fetch Binance's authenticated fee schedule before doing
the one allowed re-run; don't substitute an unverified number."* Since the
snapshot cannot verify whether this run's 30-day volume (on any
computation, smoothed or actually-traded) stays under VIP1's real
threshold or clears it, and this run does not invent VIP1-8 fees or
thresholds:

**Outcome: STOPPED. Verdict marked PROVISIONAL pending an authenticated
fee fetch.** What's needed to resolve it: Binance's authenticated VIP1
(and possibly higher) maker/taker fee schedule and its exact
30-day-volume (and BNB-balance) threshold, fetched from a logged-in
account session, covering figures up to at least ~$6M — the data points
the public snapshot could not obtain. **No second gate run was performed**
(no VIP1-8 fees were invented to run at).

---

## Per-asset breakdown

### h15 (23 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| ENA | 29 | −374.51 | 0.414 |
| XMR | 1 | −69.09 | 0.000 |
| BTC | 5 | −44.80 | 0.400 |
| XRP | 2 | −25.37 | 0.000 |
| DOGE | 7 | −1.59 | 0.429 |
| ETHFI | 1 | 28.56 | 1.000 |
| NEAR | 2 | 45.80 | 0.500 |
| SOL | 8 | 51.64 | 0.500 |
| XPL | 3 | 55.70 | 1.000 |
| ETH | 6 | 57.77 | 0.667 |
| SUI | 2 | 62.65 | 1.000 |
| LINK | 2 | 63.56 | 1.000 |
| CRV | 3 | 70.39 | 0.667 |
| AAVE | 6 | 81.33 | 0.667 |
| TAO | 14 | 250.11 | 0.786 |
| ZEC | 56 | 345.12 | 0.589 |
| VIRTUAL | 7 | 429.45 | 0.857 |
| LIT | 14 | 495.54 | 0.571 |
| UNI | 24 | 581.55 | 0.583 |
| kPEPE | 28 | 591.60 | 0.571 |
| WLD | 21 | 897.54 | 0.810 |
| PUMP | 61 | 1,046.51 | 0.590 |
| KAITO | 166 | 4,876.44 | 0.608 |

FARTCOIN is the one mapped asset absent from this table — it produced zero
h15 trades. `days_without_costs`: `{"KAITO": 3}`.

### h30 (23 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| LIT | 35 | −497.62 | 0.429 |
| WLD | 30 | −274.11 | 0.400 |
| LINK | 4 | −220.63 | 0.250 |
| XMR | 2 | −216.69 | 0.000 |
| DOGE | 11 | −183.08 | 0.455 |
| SUI | 8 | −142.19 | 0.500 |
| AAVE | 8 | −132.55 | 0.500 |
| XRP | 9 | −123.45 | 0.333 |
| SOL | 13 | −81.14 | 0.538 |
| UNI | 23 | −36.55 | 0.565 |
| ETH | 14 | −11.03 | 0.429 |
| NEAR | 4 | 14.48 | 0.750 |
| XPL | 8 | 20.34 | 0.625 |
| kPEPE | 31 | 104.11 | 0.548 |
| ENA | 32 | 146.59 | 0.500 |
| BTC | 12 | 151.25 | 0.667 |
| ETHFI | 3 | 161.19 | 0.667 |
| TAO | 12 | 393.99 | 0.667 |
| CRV | 3 | 412.40 | 1.000 |
| VIRTUAL | 7 | 538.38 | 0.857 |
| ZEC | 61 | 707.96 | 0.656 |
| PUMP | 80 | 787.10 | 0.512 |
| KAITO | 195 | 1,700.36 | 0.513 |

`days_without_costs`: `{"KAITO": 3}`.

### h60 (16 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| ZEC | 47 | −2,916.62 | 0.447 |
| WLD | 57 | −1,931.50 | 0.474 |
| ETH | 3 | −384.13 | 0.000 |
| SOL | 2 | −270.89 | 0.500 |
| SUI | 1 | −262.50 | 0.000 |
| DOGE | 1 | −249.53 | 0.000 |
| UNI | 1 | −221.10 | 0.000 |
| LINK | 1 | −203.11 | 0.000 |
| VIRTUAL | 2 | −168.04 | 0.000 |
| AAVE | 1 | −152.36 | 0.000 |
| XMR | 1 | −127.68 | 0.000 |
| XRP | 1 | −116.74 | 0.000 |
| TAO | 2 | 83.86 | 0.500 |
| NEAR | 15 | 300.80 | 0.533 |
| kPEPE | 4 | 436.13 | 0.750 |
| ENA | 31 | 1,127.32 | 0.516 |

`days_without_costs`: `{"KAITO": 3}`.

---

## VERDICT

**h15: PASS** (sharpe_annualized 6.3059 > 2.0 gate threshold).
**h30: PASS** (sharpe_annualized 2.0620 > 2.0 gate threshold).
**h60: FAIL** (sharpe_annualized −2.9575 < 2.0 gate threshold).

**Overall gate outcome: NOT GATE-ELIGIBLE.** Amendment 1's condition 1
(≥18 months of matrix span) is not met — actual per-asset matrix span is
~7 months, bounded by the ±0.2% band not being present in the ingested
`bookDepth` data before 2026-01-15, not by anything this pipeline did
wrong. Per the protocol, a run that fails an eligibility condition cannot produce a
capital-allocation gate verdict; the per-horizon PASS/PASS/FAIL results
above are the gate tool's mechanical output, not a decision. The fee
fixed-point step's outcome is also **PROVISIONAL**, independently of the
eligibility finding (see above) — the fee-table snapshot cannot resolve
whether run-1's $1.16M–$6.05M range of 30-day volume figures (smoothed
`projected_30d_volume_usd` vs. the actually-traded single month) clears
VIP1, and no VIP1-8 fee was invented to test at.

---

## What the diagnostics say

**Every horizon's result is driven by a single 30-day out-of-sample fold,
not a walk-forward record.** At h15 and h30, folds 0-2 (test windows
2026-04-15 through 2026-07-14) produced **zero trades each** — all 468
(h15) and 605 (h30) trades came from fold 3 alone (test window
2026-07-14..08-13). At h60, the pattern inverts: fold 1 (2026-05-15..06-14)
produced all 170 trades and a −5.99 fold Sharpe; folds 0, 2, and 3 produced
zero trades. In every horizon, 3 of the 4 folds are silent (n_preds per
fold: 65,268 / 91,250 / 87,690 / 72,571 — 316,779 predictions total across
all folds, of which only the active fold's ever cross a trade threshold).
The `pred_abs_p90`/`p99` columns show that even in the one active window,
trading is a thin tail: at h15 fold 3, `pred_abs_p90` is **7.703bps**,
which is *below* the lowest per-asset threshold in the run (BTC,
13.577bps) — only `pred_abs_p99` (21.336bps) clears that low end. Of
72,571 predictions in fold 3, only 468 traded (**0.64%**) — so "only the
top ~1% of predictions cleared thresholds," not "comfortably past" them
as a bulk statement about the fold. The h30/h60 folds show the same
pattern of predicted-magnitude quiet periods punctuated by one active,
thin-tail window.

This concentration also explains why the headline annualized Sharpes look
more dramatic than the underlying trading record: `sharpe_annualized` is
computed over all **121** days in the pooled out-of-sample window, of
which **90 (h15), 91 (h30), and 103 (h60) are exact zero-return days**
(no trade, no P&L) — only 31, 30, and 18 days respectively have any
nonzero daily return. The *traded-fold-only* Sharpes (each horizon's one
active fold) are **14.9122 (h15 fold 3), 4.0947 (h30 fold 3), and −5.9857
(h60 fold 1)** — each horizon's 121-day headline number is that one
month's record diluted by three flat months of zeros, not four
independently-earned months averaged together. Precisely the "one period,
not an out-of-sample record" caveat the smoke run raised, still true here
despite four nominal folds.

**h60's FAIL is not a clean "anti-correlated across the walk-forward
record" story — it's two assets losing badly in one fold.** The pooled
`ic` (−0.0097) and `rank_ic` (−0.0039) are both small and only slightly
negative — essentially indistinguishable from zero rather than a strong
anti-signal — and fold 2's own `ic` (0.0266) is actually **positive**.
The FAIL is concentrated: `ZEC` (47 trades, −2,916.62bps) and `WLD` (57
trades, −1,931.50bps) together account for **−4,848.12bps of h60's
−5,056.09bps total net** (≈96%), and per-asset data (not shown per-day in
the gate report, but consistent with both assets' trades landing inside
fold 1's 2026-05-15..06-14 window) point to early June as the loss
window. This is still a real loss — the one fold that traded lost badly
enough to fail the gate on its own — but it is concentrated in two assets
within one month, not a signal that is anti-correlated with returns
throughout the walk-forward record.

**h15 and h30's PASS results both rest on one dominant asset.** KAITO
alone contributed 4,876.44 of h15's **+9,515.90bps** total net (166 of 468
trades, **51%** of the total) and 1,700.36 of h30's **+3,219.11bps**
(195 of 605 trades, **53%**) — by far the largest single-asset contributor
at both horizons, and also the only asset with any `days_without_costs`
(3 days) or any of the 9 within-window thin-book days noted above.
**Excluding KAITO, the remaining assets are still net positive**: h15 ex-
KAITO is +4,639.46bps on 302 trades; h30 ex-KAITO is +1,518.75bps on 410
trades — so the PASS is not solely a KAITO artifact, but KAITO is
responsible for roughly half of each horizon's net edge, on a book that
itself couldn't absorb a $5,000 clip on 9 of the days inside the traded
window. Three assets — XMR, DOGE, XRP — show negative `total_net_bps` at
all three horizons; this is reported as a fact about the per-asset tables
above, not as a recommendation (the one-allowed-drop-and-rerun step in §5
was not exercised in this task).

**Out-of-sample window recap:** 121 days total in the pooled OOS record
for every horizon; days with any nonzero daily P&L: **31 (h15), 30 (h30),
18 (h60)**. The remaining 90/91/103 days are exact-zero no-trade days
(see above).
