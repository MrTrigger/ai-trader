# Crypto scalper — gate run 2 (Binance venue, Amendment 1 + Amendment 2 / fs-3)

> **Bottom line up front: this run IS gate-eligible, and the gate result is
> FAIL at all three horizons.** The matrix's actual (measured) span, not
> merely its requested `--start`/`--end` flags, reaches 2024-08-01 to
> 2026-08-13 (24.4 months) for the great majority of assets — a genuine,
> measured improvement over run 1, whose matrix was requested at the same
> 24.5-month window but whose *actual* kept rows were uniformly truncated
> to ~7 months by the ±0.2%-band gap. That measured difference is what
> grounds eligibility here, not the command flags alone (which were
> identical between the two runs and cannot by themselves distinguish
> them). See "Eligibility checklist" below. **A second fact matters just as
> much and is not part of the eligibility test: the cost model's own data
> is bound to that same ±0.2% band, so only 2026-01-15..2026-07-22 (189
> days, 6.2 months) of this 24.4-month matrix is actually costed and
> tradeable** — see "Step 2 — Binance costs". **Overall gate outcome: GATE
> FAILED.** Every horizon's out-of-sample annualized Sharpe is negative
> (h15 −1.78, h30 −1.09, h60 −1.00), 3.0-3.8 points below the 2.0 bar, on
> an eligible run — not a diagnostic-only number like run 1's, but one
> whose tradeable record is far shorter than its matrix span.

**Run date:** 2026-08-16
**Repo:** `/home/magnus/dev/magnus/ai-trader`, branch `scalper-plan3d` (not a
worktree this time — verified with `git branch --show-current`)
**Backfill:** none performed. Per Amendment 2's frozen-data rule, this run
rebuilds its matrix and reuses its cost summary from the data already on
disk under `data/` — no pull or backfill of any kind between Amendment 2
and this run.

---

## Eligibility checklist (Amendment 1 + Amendment 2)

| # | Condition | Met? | Evidence |
|---|---|---|---|
| 1 | ≥18 months of matrix span (`training-matrix --start`/`--end`) | **MET** | See "Condition 1, grounded on the measured span" below — the matrix's *actual, measured* kept-row span (not just its requested flags) reaches 24.4 months for 21 of 24 assets; three fall short of 18 months of kept-row history for listing-date reasons, which is a fact about those assets, not a violation of this condition as worded. |
| 2 | Every mapped, 90-day-eligible candidate is in the matrix | Met | 24 of 25 universe candidates are Binance-mapped (`HYPE` has `binance_um: null`, correctly excluded — unchanged from run 1, universe frozen). All 24 appear in the fs-3 matrix manifest's `assets` list and all 24 produced kept rows (see "Matrix" below). |
| 3 | fs-3 features (`fs-rust-scalper-3`) | Met | Manifest line: `"feature_set_version":"fs-rust-scalper-3"`, 38 features listed, matching Amendment 2's spec (indices 27/29/37 are `depth_10_z_60`/`depth_10_log`/`depth_imb_10_m15`; index 28 `depth_imb_10` and all other 34 indices unchanged from fs-1/fs-2). |
| 4 | Time-varying costs via `binance-costs` + `gate --binance-costs` | Met (mechanically) | Every gate report's `binance_costs` field is `data/binance-micro/costs-daily.json`; all three horizons were gated with `--binance-costs`, not `--costs`. Reused, not regenerated (see "Step 2 — Binance costs" below) — that section documents a real, separate limitation: the cost model's own ±0.2%-band dependency confines *tradeable* days to 2026-01-15 onward, well inside this matrix's 24.4-month span. |
| 5 | All three horizons (15/30/60) run, every run, all reported | Met | See per-horizon table below. |

**Conclusion: this run clears Amendment 1 + 2's eligibility bar on the
matrix-span test.** All five conditions are met. The gate's per-horizon
FAIL/FAIL/FAIL result below is therefore a real capital-allocation
verdict, not a diagnostic-only number — unlike run 1, which never reached
this state. This is a narrower claim than "the whole 24.4-month record was
traded on": see "Step 2 — Binance costs" for the separate fact that only
~6.2 months of that span was ever costed and tradeable.

### Condition 1, grounded on the measured span

Amendment 1's exact text: *"1. **≥18 months of matrix span**
(`training-matrix --start`/`--end`), not the 4-weeks-of-book-recording bar
§5.1 set for the HL protocol — Binance's archives go back years, so there's
no reason to gate on a thin window."*

Reading the parenthetical as "the command's `--start`/`--end` flags decide
this condition" does not work: **run 1 was invoked with the identical
`--start 2024-08-01 --end 2026-08-15` flags** and was correctly ruled NOT
MET on this same condition, because its *actual* kept rows were truncated
to ~7 months by the missing ±0.2% band regardless of what the flags
requested. Flag-literalism cannot distinguish run 1 from run 2 — both used
the same flags — so it is not what condition 1 is actually testing. What
changed between the two runs, and what this condition has to be read as
testing, is the matrix's **measured, actual span**: does the data that
`training-matrix` produced reach 18 months, not just the command asked for
it to.

That measured span was independently verified by streaming
`data/matrices/gate-run-2.jsonl` once (`asset`/`ts` fields only, not
loaded as row dicts) and measuring each asset's first and last kept-row
timestamp against 2026-08-15:

| asset | months | asset | months | asset | months |
|---|---:|---|---:|---|---:|
| BTC, ETH, SOL, XRP, DOGE, kPEPE, ZEC, AAVE, TAO, LINK, WLD, ENA, SUI, UNI, NEAR, ETHFI, CRV, XMR | 24.4 | LIT | 21.8 | VIRTUAL | 20.1 |
| FARTCOIN | 19.8 | KAITO | 17.8 | PUMP | 16.1 |
| | | | | XPL | 11.7 |

21 of 24 assets independently clear 18 months of kept-row history, with
the representative/majority-asset span running 2024-08-01 to 2026-08-13
(24.4 months) — a *measured* fact, not a restatement of the command flags,
and the fact that actually distinguishes this run from run 1 (whose
measured span, on the identical flags, was ~7 months for every asset).
This condition is read at the run/majority level, consistent with
condition 2 being the separate condition that speaks to individual
candidates (its text never says "every asset" or "each candidate," and
condition 2 already tolerates late-listed assets having a shorter history
for an external reason). The three that fall short of 18 months — PUMP
(16.1mo), KAITO (17.8mo), XPL (11.7mo) — are short for that same external
reason (their own Binance UM listing date), not a recurrence of run 1's
systemic data-coverage bug.

**If condition 1 is instead read as a strict per-asset bar** — i.e., as if
it said "every mapped candidate's own matrix span," which its text does
not — **this run is NOT eligible on PUMP, KAITO, and XPL**, and, per
Amendment 1's FAIL-branch discipline, that would make every horizon's
result below non-eligible diagnostics, the same status run 1 had. This
document reads the condition as testing the matrix's measured span at the
run/majority level (the reading that is actually consistent with how run
1 was adjudicated) and reports the run as eligible; the stricter
per-asset reading and its consequence are stated here for the record, not
resolved by softening either reading.

**Separately from the eligibility test — matrix span is not the same
question as tradeable span.** Matrix span: 24.4 months (2024-08-01 to
2026-08-13, measured, above). Costed/tradeable OOS span: 6.2 months
(2026-01-15 to 2026-07-22, 189 days — see "Step 2 — Binance costs"). The
gap between these two numbers is the single most important fact about
this run's diagnostics below.

---

## Commands run, in order

### Step 0 — build

The release binary was rebuilt twice during this run's data-fix work (see
"Provenance" below): once after commit `5f8e77a` (finiteness fix, binary
timestamp 2026-08-15T23:41:46+02:00) and once after commit `48af1ca`
(columnar gate loader, binary timestamp 2026-08-16T10:34:19+02:00). Both:

```
cargo build --release -p scalper-data
```

Run from `service/`. `cargo test -p features-scalper -p scalper-data` and
the full-workspace `cargo test` both passed with 0 failures after each
change (94/94 crate-local tests after `5f8e77a`; full-workspace run after
`48af1ca` shows `0 failed` across every crate, including `scalper-data`'s
own 31 unit tests unchanged).

### Step 1 — training matrix (fs-3, full universe, full requested span)

```
service/target/release/scalper-data training-matrix \
  --data-root data --micro-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --out data/matrices/gate-run-2.jsonl --stride 5
```

Same command as run 1's Step 1 with `gate-run-2` in place of `gate-run-1`,
per Amendment 2's frozen-data rule. Completed 2026-08-15 23:51:10 +02:00
(started shortly after the 23:41:46 binary rebuild). Exit 0. **2,446,568
kept rows across 24 assets**, written to `data/matrices/gate-run-2.jsonl`
(2,446,569 lines total: 1 manifest line + 2,446,568 data rows). Manifest
confirms `feature_set_version = fs-rust-scalper-3`, 38 features,
`horizons_min = [15,30,60]`, `stride_min = 5`.

The command's own per-asset log line (`<coin>: kept N of M (book from
<date>, flow from <date>, metrics from <date>, funding from <date>)`)
prints `book from 2026-01-15` for every asset. **This is a stale
diagnostic label, not a description of what fs-3 actually used or a
constraint on which rows were kept.** `coverage_starts()`
(`service/crates/scalper-data/src/main.rs:849-867`) hard-codes the check
`bid_02.is_some() && ask_02.is_some() && bid_10.is_some() && ask_10.is_some()`
for this print statement's `book` field specifically — the same ±0.2%-band
check fs-2 needed, which fs-3 (Amendment 2) was built to stop requiring.
It is not wired to `matrix::matrix_rows()`, which is what actually decides
which rows survive, and fs-3's `depth_10_z_60`/`depth_10_log`/
`depth_imb_10_m15` never read `bid_02`/`ask_02`. The authoritative
per-asset span is the one measured directly from the matrix file's own
`ts` values, below — not this label.

Per-asset kept/first-row/last-row (streamed once from the matrix file
itself, `asset`/`ts` fields only):

| asset | kept | first kept row (UTC) | last kept row (UTC) |
|---|---:|---|---|
| BTC | 203,308 | 2024-08-01 01:00 | 2026-08-13 22:55 |
| ETH | 203,392 | 2024-08-01 01:00 | 2026-08-13 22:55 |
| SOL | 201,090 | 2024-08-01 01:00 | 2026-08-13 22:55 |
| XRP | 196,893 | 2024-08-01 01:00 | 2026-08-13 22:55 |
| DOGE | 190,785 | 2024-08-01 01:00 | 2026-08-13 17:15 |
| kPEPE | 185,506 | 2024-08-01 01:00 | 2026-08-13 20:35 |
| ZEC | 86,006 | 2024-08-01 01:00 | 2026-08-13 22:55 |
| AAVE | 83,716 | 2024-08-01 01:00 | 2026-08-13 16:55 |
| TAO | 104,258 | 2024-08-01 15:55 | 2026-08-13 20:10 |
| LINK | 99,788 | 2024-08-01 05:25 | 2026-08-13 17:10 |
| FARTCOIN | 98,002 | 2024-12-20 20:00 | 2026-07-15 14:25 |
| WLD | 117,201 | 2024-08-01 01:00 | 2026-08-13 17:50 |
| ENA | 121,208 | 2024-08-01 01:00 | 2026-08-13 17:10 |
| SUI | 170,466 | 2024-08-01 15:45 | 2026-08-13 17:40 |
| VIRTUAL | 69,203 | 2024-12-10 16:00 | 2026-08-12 20:15 |
| UNI | 53,668 | 2024-08-01 05:25 | 2026-08-13 13:10 |
| NEAR | 49,670 | 2024-08-01 05:25 | 2026-08-13 16:50 |
| ETHFI | 48,263 | 2024-08-01 15:25 | 2026-08-13 22:55 |
| PUMP | 45,279 | 2025-04-12 16:00 | 2026-08-13 20:50 |
| KAITO | 37,221 | 2025-02-20 16:00 | 2026-08-13 21:50 |
| XPL | 29,534 | 2025-08-22 12:00 | 2026-08-11 16:30 |
| CRV | 20,129 | 2024-08-01 09:20 | 2026-08-12 08:45 |
| LIT | 18,160 | 2024-10-21 15:25 | 2026-08-13 22:35 |
| XMR | 13,822 | 2024-08-02 15:15 | 2026-08-13 22:55 |

### Step 2 — Binance costs (reused, not regenerated)

```
service/target/release/scalper-data binance-costs \
  --data-root data --universe data/scalper-universe.json \
  --start 2024-08-01 --end 2026-08-15 --notional 5000 \
  --out data/binance-micro/costs-daily.json
```

`data/binance-micro/costs-daily.json` was not rerun for this session
(file timestamp 2026-08-15 15:34:45 +02:00, predating this run's matrix
rebuild). Per Amendment 2's frozen-data rule this command reads the same
underlying `data/binance-micro` book archive as run 1's Step 2, over the
same `--start`/`--end`, so a rerun would reproduce the same file
byte-for-byte — its per-asset day counts and thin-day counts below are
**identical** to gate run 1's Step 2 table, which is the expected,
correct outcome of "no pull between runs," not an oversight.

| asset | days | thin days |
|---|---:|---:|
| BTC, ETH, SOL, XRP, DOGE, kPEPE, ZEC, ETHFI, WLD, XMR, SUI, NEAR, TAO, UNI, AAVE, ENA, LINK | 744 | 532 |
| CRV | 744 | 601 |
| PUMP | 464 | 252 |
| LIT | 419 | 207 |
| KAITO | 541 | 338 |
| FARTCOIN | 603 | 391 |
| XPL | 358 | 146 |
| VIRTUAL | 613 | 401 |

**`days_without_costs`** (predictions whose entry-day-or-prior-14-days
cost lookup fails entirely, per the gate report — identical across all
three horizon reports, since it's a matrix/cost-file property, not a
horizon-dependent one) totals **8,280** asset-days across the 24 assets,
ranging from LIT's 30 up to 442 apiece for **seven** assets with
uninterrupted 2024-08-01 history (BTC, DOGE, ETH, SOL, SUI, XRP, kPEPE);
just below that, LINK 438, ENA 436, AAVE 426, WLD 424, TAO 423 — far
larger than run 1's **3 per horizon** (also identical across horizons,
all on KAITO).

**This is not an undercount — it is the same ±0.2%-band gap that drove
run 1's eligibility failure, now surfacing on the cost side instead of the
feature side.** `binance_costs.rs`'s `impact_bps` is a pre-registered
walk-cost model priced off the day's **median ±0.2% band depth**
(`bid_02`/`ask_02`) — the identical band fs-2's `depth_imb_02` needed and
fs-3 was built to stop needing for its *features*. The band is absent
before 2026-01-15 (per Amendment 2), so every day before that has a
measured `impact_bps: None` entry. And `gate.rs::resolve_round_trip`'s own
rule (its doc comment, verbatim: *"if `day` itself has a `DayCost` entry,
that entry decides the outcome outright, with no fallback... A
measured-and-thin entry day is untradeable on ITS OWN evidence"*) means
the 14-day lookback **never rescues a measured-thin day** — it only
activates when a day has no cost entry at all. Since nearly every
pre-2026-01-15 day *does* have a measured (thin) entry, not a missing one,
the lookback cannot reach past 2026-01-15 either. The result: **every OOS
day before 2026-01-15 is untradeable by rule, regardless of signal**, and
442 = 631 − 189 exactly for the seven assets with full 2024-08-01 history
— 442 is every day of the pooled OOS window *before* the 189-day costed
window that begins 2026-01-15, not a sample of miscellaneous gaps within
it. Assets slightly below 442 (LINK 438, ENA 436, AAVE 426, WLD 424, TAO
423, ...) have a handful of additional thin days *inside* the 189-day
costed window on top of that baseline.

**Costed/tradeable window: 2026-01-15 to 2026-07-22 — 189 days (6.2
months) — essentially the same window run 1 was ruled non-eligible on**
(run 1's actual matrix span measured 2026-01-15 to between 2026-07-15 and
2026-08-13, ~205-210 days). fs-3 fixed which *features* the ±0.2% band
gates; it did not, and could not, change which days the *cost model* can
price, because `binance_costs.rs` was not part of Amendment 2's
substitution. This is the direct explanation for why only 4-5 of 21 folds
trade at any horizon (see "Per-horizon results" below): folds 0-13 (test
windows entirely before 2025-12-24) and most of fold 14 (test window
2025-12-24..2026-01-23, only its last ~9 days inside the costed window)
cover dates where trading is impossible by construction, independent of
model quality.

---

### Step 3 — per-horizon fold fit + gate

```
uv run python walk_forward_scalper.py --matrix ../data/matrices/gate-run-2.jsonl --horizon <H> --out-dir ../data/models/gate-run-2-h<H>
service/target/release/scalper-data gate --matrix data/matrices/gate-run-2.jsonl --folds data/models/gate-run-2-h<H>/folds.json --binance-costs data/binance-micro/costs-daily.json --fee-taker-bps 4.5 --fee-maker-bps 1.8 --notional 5000 --out data/reports/gate-run-2-h<H>.json
```

Fold fit run from `training/`, gate run from repo root, all three horizons
`--horizon 15|30|60`. The anchored-expanding-window default (90-day train
floor, 30-day test, 30-day step) produced **21 folds per horizon** over
the full matrix span (vs. run 1's 4, directly reflecting the ~7-month →
~24.5-month span fix).

| horizon | fold-fit command/guard | fold-fit runtime | fold-fit result | gate command/guard | gate runtime | gate result | peak RSS |
|---:|---|---|---|---|---|---|---|
| 15 | `systemd-run --scope --user -p MemoryMax=7G` | ~344s (5m44s) | 21 folds (fold-0..fold-20) | `( ulimit -v 9000000; ... )` | not separately timed (ps-sampled) | 21 folds, 23,544 trades, **FAIL**, sharpe=−1.7834818 | fit 2,533 MB; gate 1,088,364 KB (~1,063 MB) |
| 30 | `ulimit -v 9000000` (sequential harness, see Provenance) | ~08:38:40→08:44:04 UTC (~5m24s) | 21 folds (fold-0..fold-20) | `ulimit -v 9000000` | ~08:44:04→08:44:42 UTC (~38s) | 21 folds, 15,074 trades, **FAIL**, sharpe=−1.0926994 | not separately sampled |
| 60 | `ulimit -v 9000000` (sequential harness) | ~08:44:43→08:48:01 UTC (~3m18s) | 21 folds (fold-0..fold-20) | `ulimit -v 9000000` | ~08:48:01→08:48:28 UTC (~27s) | 21 folds, 10,220 trades, **FAIL**, sharpe=−0.9956028 | not separately sampled |

h30/h60's fit+gate durations are recovered from `=== fit hN HH:MM:SS`
console phase markers printed by the sequential run script, cross-checked
against each report's own `generated_utc` (h30: `2026-08-16T08:44:42Z`;
h60: `2026-08-16T08:48:28Z` — both within 1s of the phase markers). No
separate peak-RSS sampler was run for h30/h60's fit or gate phases (only
h15's phases were wrapped in a `ps`-polling loop); this is reported as a
gap, not backfilled with an estimate.

---

## Per-horizon results

| horizon | n_trades | sharpe_annualized | gate | gate_threshold | ic (pooled) | rank_ic (pooled) | n_preds (total) | folds traded / 21 | zero-return days | nonzero-return days | nonzero-day-only sharpe | projected_30d_volume_usd |
|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 15 | 23,544 | −1.783482 | FAIL | 2.0 | 0.241242 | 0.257331 | 2,143,846 | 4 | 535 | 96 | −4.667093 | 11,193,660.86 |
| 30 | 15,074 | −1.092699 | FAIL | 2.0 | 0.171904 | 0.175808 | 2,143,846 | 5 | 534 | 97 | −2.800117 | 7,166,719.49 |
| 60 | 10,220 | −0.995603 | FAIL | 2.0 | 0.111679 | 0.114397 | 2,143,846 | 5 | 534 | 97 | −2.547330 | 4,858,954.04 |

Out-of-sample window: 631 days total (fold-0's test start 2024-10-30
through fold-20's test end 2026-07-22), identical across horizons since
the fold schedule doesn't depend on horizon. `n_preds` totals are
identical across horizons for the same reason (same folds, same rows —
horizon only changes the forward-return target and which rows cross the
trade threshold). `excluded_thin_books` is `[]` for all three horizons.
The "nonzero-day-only sharpe" column recomputes `mean/stdev·√365` over
just the days with nonzero daily P&L (independently reproduces each
report's own `overall.sharpe_annualized` when run over *all* 631 days,
confirming the recomputation method); unlike run 1, where exactly one
fold traded, here 4-5 folds trade per horizon, so this is a "days with any
P&L" statistic, not a single-fold Sharpe.

Fold-level detail (`ic`/`rank_ic` are each fold's own model; `pred_abs_p50
/p90/p99` are that fold's predicted |return| percentiles, in bps):

### h15

| fold | test window | n_trades | sharpe | ic | rank_ic | n_preds | pred_abs p50 | p90 | p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2024-10-30..2024-11-29 | 0 | — | 0.3069 | 0.3055 | 107,039 | 11.475 | 34.053 | 57.662 |
| 1 | 2024-11-29..2024-12-29 | 0 | — | 0.3104 | 0.3066 | 133,018 | 12.841 | 38.639 | 68.258 |
| 2 | 2024-12-29..2025-01-28 | 0 | — | 0.2906 | 0.2916 | 118,774 | 11.425 | 36.147 | 72.577 |
| 3 | 2025-01-28..2025-02-27 | 0 | — | 0.2883 | 0.2933 | 118,135 | 11.705 | 36.945 | 69.928 |
| 4 | 2025-02-27..2025-03-29 | 0 | — | 0.2829 | 0.2842 | 108,702 | 11.630 | 35.433 | 69.513 |
| 5 | 2025-03-29..2025-04-28 | 0 | — | 0.2655 | 0.2865 | 99,336 | 10.308 | 31.984 | 63.135 |
| 6 | 2025-04-28..2025-05-28 | 0 | — | 0.3194 | 0.3062 | 120,879 | 10.283 | 31.450 | 58.500 |
| 7 | 2025-05-28..2025-06-27 | 0 | — | 0.3305 | 0.3047 | 116,510 | 9.892 | 29.198 | 55.873 |
| 8 | 2025-06-27..2025-07-27 | 0 | — | 0.3218 | 0.2995 | 110,220 | 10.341 | 32.731 | 64.090 |
| 9 | 2025-07-27..2025-08-26 | 0 | — | 0.3072 | 0.2994 | 114,729 | 10.149 | 30.001 | 58.904 |
| 10 | 2025-08-26..2025-09-25 | 0 | — | 0.3237 | 0.3086 | 107,164 | 8.737 | 27.741 | 59.838 |
| 11 | 2025-09-25..2025-10-25 | 0 | — | 0.1567 | 0.2894 | 123,661 | 10.234 | 35.320 | 74.052 |
| 12 | 2025-10-25..2025-11-24 | 0 | — | 0.3120 | 0.2920 | 122,709 | 11.356 | 36.144 | 68.786 |
| 13 | 2025-11-24..2025-12-24 | 0 | — | 0.3032 | 0.2960 | 93,582 | 9.000 | 28.397 | 56.605 |
| 14 | 2025-12-24..2026-01-23 | 3,477 | 8.5155 | 0.2951 | 0.2945 | 87,094 | 8.524 | 28.354 | 58.593 |
| 15 | 2026-01-23..2026-02-22 | 15,194 | −10.2586 | 0.0557 | 0.0577 | 86,130 | 10.480 | 32.703 | 61.773 |
| 16 | 2026-02-22..2026-03-24 | 4,635 | −22.1284 | −0.0057 | −0.0079 | 70,243 | 6.017 | 16.603 | 30.906 |
| 17 | 2026-03-24..2026-04-23 | 238 | −15.0639 | −0.0158 | −0.0323 | 59,281 | 2.657 | 7.005 | 14.727 |
| 18 | 2026-04-23..2026-05-23 | 0 | — | −0.0032 | −0.0229 | 66,936 | 0.475 | 1.471 | 3.311 |
| 19 | 2026-05-23..2026-06-22 | 0 | — | 0.0416 | 0.0428 | 98,215 | 0.273 | 0.901 | 1.419 |
| 20 | 2026-06-22..2026-07-22 | 0 | — | 0.2871 | 0.2792 | 81,489 | 1.206 | 3.721 | 7.327 |

### h30

| fold | test window | n_trades | sharpe | ic | rank_ic | n_preds | pred_abs p50 | p90 | p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2024-10-30..2024-11-29 | 0 | — | 0.2030 | 0.2057 | 107,039 | 10.429 | 31.483 | 57.947 |
| 1 | 2024-11-29..2024-12-29 | 0 | — | 0.2175 | 0.2121 | 133,018 | 11.561 | 34.439 | 64.605 |
| 2 | 2024-12-29..2025-01-28 | 0 | — | 0.1802 | 0.1900 | 118,774 | 11.143 | 34.226 | 75.167 |
| 3 | 2025-01-28..2025-02-27 | 0 | — | 0.1935 | 0.1956 | 118,135 | 9.335 | 28.936 | 59.931 |
| 4 | 2025-02-27..2025-03-29 | 0 | — | 0.1855 | 0.1941 | 108,702 | 11.189 | 33.349 | 68.802 |
| 5 | 2025-03-29..2025-04-28 | 0 | — | 0.1772 | 0.1942 | 99,336 | 9.729 | 29.278 | 59.974 |
| 6 | 2025-04-28..2025-05-28 | 0 | — | 0.2245 | 0.2156 | 120,879 | 10.114 | 30.166 | 57.533 |
| 7 | 2025-05-28..2025-06-27 | 0 | — | 0.2189 | 0.2085 | 116,510 | 9.579 | 27.087 | 51.029 |
| 8 | 2025-06-27..2025-07-27 | 0 | — | 0.2095 | 0.1996 | 110,220 | 9.709 | 29.070 | 55.682 |
| 9 | 2025-07-27..2025-08-26 | 0 | — | 0.2118 | 0.2069 | 114,729 | 10.203 | 29.374 | 57.977 |
| 10 | 2025-08-26..2025-09-25 | 0 | — | 0.2259 | 0.2137 | 107,164 | 8.922 | 26.855 | 58.518 |
| 11 | 2025-09-25..2025-10-25 | 0 | — | 0.1400 | 0.2018 | 123,661 | 10.161 | 33.638 | 70.617 |
| 12 | 2025-10-25..2025-11-24 | 0 | — | 0.2240 | 0.2103 | 122,709 | 11.232 | 35.114 | 68.429 |
| 13 | 2025-11-24..2025-12-24 | 0 | — | 0.2078 | 0.2036 | 93,582 | 8.842 | 26.952 | 53.441 |
| 14 | 2025-12-24..2026-01-23 | 2,061 | 7.8723 | 0.2100 | 0.2055 | 87,094 | 8.347 | 26.577 | 55.355 |
| 15 | 2026-01-23..2026-02-22 | 9,376 | −7.1615 | 0.0546 | 0.0362 | 86,130 | 10.435 | 31.928 | 62.954 |
| 16 | 2026-02-22..2026-03-24 | 3,338 | −19.0320 | −0.0046 | −0.0093 | 70,243 | 5.989 | 16.399 | 30.243 |
| 17 | 2026-03-24..2026-04-23 | 293 | −8.1858 | −0.0055 | −0.0190 | 59,281 | 2.932 | 7.837 | 16.206 |
| 18 | 2026-04-23..2026-05-23 | 0 | — | 0.0053 | −0.0202 | 66,936 | 0.496 | 1.507 | 3.146 |
| 19 | 2026-05-23..2026-06-22 | 0 | — | 0.0245 | 0.0281 | 98,215 | 0.328 | 0.977 | 2.101 |
| 20 | 2026-06-22..2026-07-22 | 6 | 3.4314 | 0.1995 | 0.1890 | 81,489 | 1.275 | 3.713 | 7.259 |

### h60

| fold | test window | n_trades | sharpe | ic | rank_ic | n_preds | pred_abs p50 | p90 | p99 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 2024-10-30..2024-11-29 | 0 | — | 0.1366 | 0.1382 | 107,039 | 9.263 | 29.182 | 57.464 |
| 1 | 2024-11-29..2024-12-29 | 0 | — | 0.1366 | 0.1295 | 133,018 | 9.640 | 30.103 | 57.978 |
| 2 | 2024-12-29..2025-01-28 | 0 | — | 0.1024 | 0.1064 | 118,774 | 8.348 | 27.484 | 68.434 |
| 3 | 2025-01-28..2025-02-27 | 0 | — | 0.1166 | 0.1236 | 118,135 | 6.427 | 20.389 | 51.614 |
| 4 | 2025-02-27..2025-03-29 | 0 | — | 0.1035 | 0.1258 | 108,702 | 9.160 | 27.666 | 61.458 |
| 5 | 2025-03-29..2025-04-28 | 0 | — | 0.1212 | 0.1328 | 99,336 | 6.541 | 19.039 | 39.908 |
| 6 | 2025-04-28..2025-05-28 | 0 | — | 0.1514 | 0.1443 | 120,879 | 9.893 | 29.354 | 60.155 |
| 7 | 2025-05-28..2025-06-27 | 0 | — | 0.1238 | 0.1292 | 116,510 | 7.995 | 22.322 | 42.858 |
| 8 | 2025-06-27..2025-07-27 | 0 | — | 0.1382 | 0.1375 | 110,220 | 8.685 | 25.412 | 50.263 |
| 9 | 2025-07-27..2025-08-26 | 0 | — | 0.1483 | 0.1422 | 114,729 | 9.045 | 25.106 | 49.633 |
| 10 | 2025-08-26..2025-09-25 | 0 | — | 0.1368 | 0.1473 | 107,164 | 8.183 | 23.479 | 52.655 |
| 11 | 2025-09-25..2025-10-25 | 0 | — | 0.0985 | 0.1414 | 123,661 | 9.606 | 30.957 | 69.958 |
| 12 | 2025-10-25..2025-11-24 | 0 | — | 0.1586 | 0.1439 | 122,709 | 10.101 | 30.677 | 61.363 |
| 13 | 2025-11-24..2025-12-24 | 0 | — | 0.1301 | 0.1316 | 93,582 | 8.104 | 23.406 | 46.352 |
| 14 | 2025-12-24..2026-01-23 | 1,304 | 7.7677 | 0.1489 | 0.1458 | 87,094 | 8.529 | 26.091 | 57.384 |
| 15 | 2026-01-23..2026-02-22 | 5,445 | −5.1233 | 0.0419 | 0.0263 | 86,130 | 9.843 | 29.080 | 58.412 |
| 16 | 2026-02-22..2026-03-24 | 3,330 | −12.2875 | −0.0091 | −0.0109 | 70,243 | 7.228 | 19.668 | 37.492 |
| 17 | 2026-03-24..2026-04-23 | 134 | −4.5512 | −0.0116 | −0.0201 | 59,281 | 2.557 | 6.773 | 13.975 |
| 18 | 2026-04-23..2026-05-23 | 0 | — | 0.0022 | −0.0167 | 66,936 | 0.405 | 0.829 | 1.337 |
| 19 | 2026-05-23..2026-06-22 | 0 | — | 0.0405 | 0.0248 | 98,215 | 0.602 | 1.198 | 2.341 |
| 20 | 2026-06-22..2026-07-22 | 7 | 3.4314 | 0.1310 | 0.1231 | 81,489 | 1.318 | 3.469 | 6.334 |

Folds 14-17 trade at all three horizons; fold 20 additionally trades at
h30 (6 trades) and h60 (7 trades). Folds 0-13 and 18-19 trade zero at all
three horizons.

---

## Fee fixed-point step (Amendment 1, unchanged by Amendment 2)

Run 2 charges VIP0 + BNB-discount taker/maker (4.50/1.80 bps) as
pre-registered. `overall.projected_30d_volume_usd` from the three reports:

| horizon | projected_30d_volume_usd | realized busiest-month volume (fold 15) | realized total volume (full 631-day OOS record) |
|---:|---:|---:|---:|
| 15 | 11,193,660.86 | 151,940,000.00 (15,194 trades × 2 × $5,000) | 235,440,000.00 (23,544 × 2 × $5,000) |
| 30 | 7,166,719.49 | 93,760,000.00 (9,376 × 2 × $5,000) | 150,740,000.00 (15,074 × 2 × $5,000) |
| 60 | 4,858,954.04 | 54,450,000.00 (5,445 × 2 × $5,000) | 102,200,000.00 (10,220 × 2 × $5,000) |

`projected_30d_volume_usd` smooths `n_trades × 2 × notional` over the
whole 631-day pooled OOS window and annualizes to a 30-day rate — the same
formula run 1 used. It understates the concentration documented above (4-5
of 21 folds trade): fold 15 alone (2026-01-23..2026-02-22, the single
busiest ~30-day window at every horizon) realizes **13.6x (h15), 13.1x
(h30), and 11.2x (h60)** the smoothed figure — $151.94M vs. $11.19M at
h15, down to $54.45M vs. $4.86M at h60. The full-record realized total is
**exactly 21.0x** the smoothed 30-day figure at every horizon (631 OOS
days ÷ 30 — a ratio of the window lengths, not asset- or horizon-specific,
so it comes out identical across all three: $235.44M/$11.19M,
$150.74M/$7.17M, $102.2M/$4.86M all reduce to 21.0).

Mapping to a tier using `docs/binance-um-fee-table-2026-08.md` **by the
volume criterion only**, per the pre-registered rule:

- **VIP0** has no stated volume floor (the default, unqualified rate) and
  **VIP9** needs ≥$30,000,000,000 30-day volume (cited, not verified) —
  none of this run's figures ($4.86M smoothed up to $235.4M realized-total)
  come close to VIP9 on any reading.
- The snapshot's "Gap" section leaves **VIP1-VIP8 unresolved**: four
  disagreeing, uncorroborated secondary-source figures for VIP1's own
  volume floor alone ($250k / $1M / $5M / $15M), one of them
  self-contradictory (a fee rate *higher* than VIP0's, flagged by the
  snapshot as likely contamination). No Binance-authored number exists in
  the snapshot for this range.
- **Every horizon's smoothed figure clears the two lowest disputed
  candidates ($250k, $1M) by 5x-45x**, and the realized busiest-month and
  full-record figures clear **all four** disputed candidates, including
  the highest ($15M), by 3.6x-15.7x. **This is an argument that VIP0 is an
  implausible mapped tier, built on secondary-sourced, unverified
  candidate thresholds — not a computation that resolves the tier**, since
  the snapshot itself states none of the four VIP1 candidates is
  Binance-authored and one is internally impossible. Run 1's $1.16M-$6.05M
  range at least straddled the disputed band on a similar, weaker
  argument; run 2's $4.86M-$235.4M range sits above or deep inside it on
  every measure, which strengthens the argument without turning it into a
  verified fact.

Per the pre-registered rule: *"If the mapped tier differs from VIP0, run
exactly ONE re-run at that tier's fees... **The second run's verdict is
the gate verdict.** Not the first, not whichever is more favorable. If the
fee-table snapshot doesn't cover the mapped tier... re-fetch Binance's
authenticated fee schedule before doing the one allowed re-run; don't
substitute an unverified number."* This rule is unconditional: it is not
"re-run if the argument above is persuasive enough" — it requires a real
re-run at a real, resolved tier before any second-run verdict exists.
Since this run's volume maps (per the argument above, not a verified
computation) to a tier the snapshot cannot resolve, and no VIP1-8 fee is
invented to test at:

**Outcome: STOPPED. Verdict marked PROVISIONAL pending an authenticated
fee fetch.** This PROVISIONAL status is the headline of this section, not
a footnote to it: by Amendment 1's rule, the run-2 number this document
reports (VIP0 + BNB rates) is *not* the tier the fixed-point rule would
ultimately settle on if the tier were resolved — only the second run, at
the real mapped tier, produces the rule's actual verdict, and that run has
not happened. What the authenticated fetch must resolve: Binance's real
VIP1 (and, given the realized-total figures reach $102M-$235M over the
full record, very possibly higher) maker/taker fee schedule and
BNB-balance thresholds, covering volume figures up to at least **~$235M**,
not just the ~$4.9M-$11.2M smoothed range — smoothed volume here is
**4.8x-11.5x** run 1's smoothed figures per matching horizon (h15
9.6x, h30 4.8x, h60 11.5x) and realized busiest-month volume is
**15x-32x** run 1's realized busiest-month figures (h15 32.5x, h30
15.5x, h60 32.0x). **No second gate run was performed.**

Independent of this open question, and stated only as a numeric fact, not
a substitute for the required re-run: since VIP1+ fees can only be lower
than VIP0's, and every horizon's Sharpe already sits 3.0-3.8 points below
the 2.0 threshold at VIP0's (higher) fee rate, a cheaper fee schedule
would move the exact Sharpe figures but is very unlikely to flip the
FAIL/FAIL/FAIL disposition below — that is an observation about magnitude,
not a reason to skip the one allowed re-run the rule requires.

---

## Per-asset breakdown

All 24 mapped assets traded at all three horizons (unlike run 1, where
FARTCOIN produced zero h15 trades). `days_without_costs` per-asset totals
are in "Step 2 — Binance costs" above (identical across horizons).

### h15 (24 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| ZEC | 2,943 | −20,353.40 | 0.442 |
| kPEPE | 2,351 | −13,208.53 | 0.451 |
| TAO | 1,923 | −11,489.29 | 0.442 |
| DOGE | 1,619 | −10,757.26 | 0.429 |
| XRP | 1,388 | −10,195.36 | 0.428 |
| BTC | 959 | −8,228.52 | 0.397 |
| SOL | 1,373 | −7,843.72 | 0.425 |
| UNI | 478 | −5,848.87 | 0.475 |
| ETH | 1,429 | −5,504.01 | 0.436 |
| AAVE | 752 | −5,191.51 | 0.461 |
| SUI | 1,761 | −4,936.30 | 0.466 |
| WLD | 669 | −4,623.61 | 0.457 |
| NEAR | 201 | −3,137.29 | 0.438 |
| PUMP | 1,136 | −2,364.46 | 0.489 |
| XPL | 178 | −2,186.70 | 0.449 |
| ETHFI | 159 | −1,927.77 | 0.472 |
| ENA | 281 | −1,799.41 | 0.495 |
| LINK | 687 | −1,342.44 | 0.479 |
| FARTCOIN | 538 | −326.13 | 0.517 |
| CRV | 1 | 55.49 | 1.000 |
| KAITO | 121 | 1,026.99 | 0.562 |
| VIRTUAL | 868 | 1,390.68 | 0.492 |
| LIT | 261 | 2,553.73 | 0.544 |
| XMR | 1,468 | 6,323.46 | 0.510 |

Gain side (5 assets net positive, +11,350.35bps total): top-3 = **XMR
(+6,323.46, 55.7% of gains), LIT (+2,553.73, 22.5%), VIRTUAL (+1,390.68,
12.3%)**. Loss side (19 assets net negative, −121,264.59bps total): top-3
= **ZEC (−20,353.40, 16.8% of losses), kPEPE (−13,208.53, 10.9%), TAO
(−11,489.29, 9.5%)**. Net total: −109,914.24bps.

### h30 (24 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| TAO | 1,243 | −11,847.82 | 0.459 |
| kPEPE | 1,479 | −10,359.86 | 0.455 |
| ZEC | 1,811 | −7,478.41 | 0.468 |
| XRP | 902 | −5,828.60 | 0.442 |
| SUI | 1,123 | −5,757.53 | 0.454 |
| BTC | 657 | −3,792.88 | 0.416 |
| ETH | 963 | −3,453.95 | 0.447 |
| DOGE | 1,054 | −3,421.55 | 0.449 |
| AAVE | 498 | −3,336.58 | 0.460 |
| XPL | 119 | −2,670.98 | 0.462 |
| UNI | 305 | −2,377.62 | 0.472 |
| NEAR | 133 | −2,248.91 | 0.368 |
| ETHFI | 99 | −2,175.92 | 0.333 |
| WLD | 429 | −960.25 | 0.471 |
| PUMP | 672 | −649.43 | 0.473 |
| SOL | 936 | −167.67 | 0.469 |
| CRV | 1 | 21.53 | 1.000 |
| KAITO | 80 | 657.13 | 0.575 |
| LINK | 441 | 781.04 | 0.499 |
| FARTCOIN | 344 | 1,296.26 | 0.506 |
| ENA | 195 | 2,402.74 | 0.538 |
| XMR | 868 | 2,438.61 | 0.501 |
| LIT | 166 | 3,049.46 | 0.536 |
| VIRTUAL | 556 | 5,516.31 | 0.516 |

Gain side (8 assets net positive, +16,163.10bps total): top-3 = **VIRTUAL
(+5,516.31, 34.1% of gains), LIT (+3,049.46, 18.9%), XMR (+2,438.61,
15.1%)**. Loss side (16 assets net negative, −66,527.96bps total): top-3
= **TAO (−11,847.82, 17.8% of losses), kPEPE (−10,359.86, 15.6%), ZEC
(−7,478.41, 11.2%)**. Net total: −50,364.86bps.

### h60 (24 of 24 assets traded; sorted by total_net_bps)

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| SUI | 743 | −9,728.07 | 0.436 |
| PUMP | 421 | −6,612.52 | 0.449 |
| ZEC | 1,163 | −6,484.39 | 0.463 |
| NEAR | 120 | −3,454.99 | 0.417 |
| XRP | 634 | −3,115.62 | 0.470 |
| XPL | 95 | −2,924.81 | 0.421 |
| AAVE | 365 | −2,840.82 | 0.466 |
| ENA | 124 | −2,229.84 | 0.427 |
| kPEPE | 995 | −2,172.90 | 0.483 |
| DOGE | 753 | −1,968.56 | 0.458 |
| ETH | 690 | −1,660.82 | 0.442 |
| BTC | 499 | −1,613.14 | 0.435 |
| UNI | 196 | −1,140.25 | 0.454 |
| TAO | 816 | −876.15 | 0.468 |
| XMR | 509 | −755.11 | 0.507 |
| CRV | 1 | −12.31 | 0.000 |
| LINK | 291 | 4.67 | 0.519 |
| LIT | 109 | 31.45 | 0.514 |
| ETHFI | 69 | 319.68 | 0.478 |
| FARTCOIN | 231 | 428.82 | 0.545 |
| SOL | 667 | 721.18 | 0.457 |
| WLD | 314 | 1,089.37 | 0.494 |
| KAITO | 55 | 3,079.69 | 0.582 |
| VIRTUAL | 360 | 3,183.11 | 0.514 |

Gain side (8 assets net positive, +8,857.97bps total): top-3 = **VIRTUAL
(+3,183.11, 35.9% of gains), KAITO (+3,079.69, 34.8%), WLD (+1,089.37,
12.3%)**. Loss side (16 assets net negative, −47,590.32bps total): top-3
= **SUI (−9,728.07, 20.4% of losses), PUMP (−6,612.52, 13.9%), ZEC
(−6,484.39, 13.6%)**. Net total: −38,732.34bps.

---

## Drop-rule candidates (report only — not exercised)

Amendment 1's one-allowed-drop rule requires negative `total_net_bps` in
**all three** horizon reports before a symbol is eligible to be excluded.
13 of 24 assets meet that bar this run:

| asset | h15 | h30 | h60 |
|---|---:|---:|---:|
| ZEC | −20,353.40 | −7,478.41 | −6,484.39 |
| kPEPE | −13,208.53 | −10,359.86 | −2,172.90 |
| TAO | −11,489.29 | −11,847.82 | −876.15 |
| SUI | −4,936.30 | −5,757.53 | −9,728.07 |
| XRP | −10,195.36 | −5,828.60 | −3,115.62 |
| DOGE | −10,757.26 | −3,421.55 | −1,968.56 |
| BTC | −8,228.52 | −3,792.88 | −1,613.14 |
| AAVE | −5,191.51 | −3,336.58 | −2,840.82 |
| ETH | −5,504.01 | −3,453.95 | −1,660.82 |
| PUMP | −2,364.46 | −649.43 | −6,612.52 |
| UNI | −5,848.87 | −2,377.62 | −1,140.25 |
| NEAR | −3,137.29 | −2,248.91 | −3,454.99 |
| XPL | −2,186.70 | −2,670.98 | −2,924.81 |

This is reported as a fact about the per-asset tables above. Per Amendment
1, the drop-and-rerun step is exercised **after** a full first run — this
document does not exercise it, and does not recommend that it be
exercised, given the VERDICT below.

---

## VERDICT

**h15: FAIL** (sharpe_annualized −1.783482, 3.8 points below the 2.0 gate
threshold).
**h30: FAIL** (sharpe_annualized −1.092699, 3.1 points below the 2.0 gate
threshold).
**h60: FAIL** (sharpe_annualized −0.995603, 3.0 points below the 2.0 gate
threshold).

**Overall gate outcome: GATE FAILED.** This run is eligible under the
measured-span reading of Amendment 1's condition 1 (see "Eligibility
checklist" above) — the matrix's actual kept-row span reaches 24.4 months
for 21 of 24 assets, and conditions 2-5 are independently met. Unlike run
1, whose PASS/PASS/FAIL numbers were explicitly non-authoritative
diagnostics, **this run's FAIL/FAIL/FAIL result is the gate verdict the
protocol's §5/Amendment 1 capital-allocation rule is read from — for the
6.2-month window (2026-01-15..2026-07-22) that was actually costed and
tradeable, not the full 24.4-month matrix span** (see "Step 2 — Binance
costs" and "What the diagnostics say"). The fee fixed-point step is
separately **PROVISIONAL, not final** (see above): Amendment 1's rule
makes the second, tier-resolved run's verdict the actual verdict, and that
run has not happened. As a numeric observation only, not a substitute for
that required re-run: VIP1+ fees can only be lower than the VIP0 rate
charged here, and every horizon already fails by 3.0-3.8 points at the
higher VIP0 rate. If condition 1 is instead read as a strict per-asset bar
(see "Condition 1, grounded on the measured span"), this run is **not**
eligible on PUMP/KAITO/XPL, and every number above is a diagnostic, not a
verdict — but the per-horizon FAIL/FAIL/FAIL mechanical result is
unchanged either way.

---

## What the diagnostics say

**Per-fold IC is bimodal, and the two modes line up with the tradeable
window, not with chance.** For h15: folds 0-14 and fold 20 (the folds
whose test windows sit outside the 2026-01-15..2026-07-22 costed window,
plus fold 14 which straddles its start, plus fold 20 which is fully
inside it but sits after the loss run) run ≈0.27-0.33; folds 15-19 (which
carry 85% of h15's 23,544 trades — 20,067 of them — and every trading
fold's net loss) run **0.0557, −0.0057, −0.0158, −0.0032, 0.0416** — near
zero, three of the five negative. The same pattern holds at h30/h60 (see
the per-horizon fold tables above). **Where money was lost, there was no
ranking skill left to lose it profitably with; where ranking skill was
strongest (folds 0-13), the cost model could not price a single trade, so
none was taken.** `overall.ic`/`overall.rank_ic` — h15 0.241242/0.257331,
h30 0.171904/0.175808, h60 0.111679/0.114397 — pool predictions from both
regimes into one number; reading it as "the strategy's skill" conflates a
high-IC, untradeable regime with a near-zero-IC, actually-traded one.

**Gross P&L (before fees/impact) is positive on average; costs, not
signal, drive the net loss — except that even gross P&L only clears costs
in one 8-day window.** Per-trade arithmetic, from `overall.n_trades` and
per-asset `total_net_bps` (already-net figures) plus the trade-weighted
round-trip cost implied by `threshold_bps_by_asset` (`= threshold / 1.5`,
weighted by each asset's `n_trades`):

| horizon | mean net/trade (bps) | trade-weighted round trip (bps) | of which fees | implied mean gross/trade (bps) | hit rate |
|---:|---:|---:|---:|---:|---:|
| 15 | −4.67 | 11.43 | 9.0 (2×4.5 taker) | ≈+6.8 | 45.5% |
| 30 | −3.34 | 11.40 | 9.0 | ≈+8.1 | 46.4% |
| 60 | −3.79 | 11.38 | 9.0 | ≈+7.6 | 46.8% |

A positive implied mean gross/trade with a sub-50% hit rate is a
right-skewed distribution (a minority of large winners funding a majority
of small losers), not a broad edge. It is also **not evenly spread across
the traded folds**: fold 14's own test window (2025-12-24..2026-01-23,
sharpe +8.5155 at h15) is gross-profitable at roughly **+27bps/trade**,
concentrated in the ~8 days (2026-01-15..2026-01-22) that fall inside the
costed window before fold 15 begins. Excluding fold 14, the remaining
traded folds (15-17, and 20 at h30/h60) run roughly **+3.3bps/trade
gross — below the 9.0bps fee floor alone**, before any spread/impact is
added. Daily net P&L (`daily_returns_bps`) is uniformly small and positive
from 2026-01-15 through 2026-01-31, then **flips sharply negative starting
2026-02-01** (h15: −334, −704, +30, −653, −926 bps on 02-01..02-05) and
stays predominantly negative through the rest of the costed window.

**One number here does not look like a real 15-minute signal and is
flagged, not adjusted.** The pooled `ic` for the 2024-25 folds (0-13,
before any trading is even possible) runs **≈0.27-0.33 at every horizon
sampled at h15** — an implausibly high correlation for a 15-minute
crypto-perp return prediction by the standards of published intraday
crypto/equity microstructure literature, where single-digit-percent IC is
already considered strong. This is reported as a fact worth an alignment
check (are `folds[].ic` and the forward-return target actually
non-overlapping and free of any train/test leakage in this range,
independent of the ±0.2%-band question) — not investigated or adjusted
here, and not treated as informing the FAIL verdict either way, since the
verdict rests on the traded folds' net P&L, not on this pooled figure.

Per Amendment 1's protocol: *"A FAIL — on either the first run or the one
allowed re-run — means the project stops or returns to feature research.
Not: lower the 2.0 threshold. Not: keep adding horizons until one clears.
Not: shrink the notional until costs look better. Not: re-run with a
different fold schedule until a favorable window turns up. 'Returns to
feature research' means new signal work under a new `FEATURE_SET_VERSION`,
followed by this exact protocol run again from a fresh first run — not a
second attempt at massaging the same features past the same gate."*

---

## Provenance (data/loader fixes between run 1 and this run)

- **OOM of the first fit attempt** (Python `train_scalper.load_matrix`
  materializing the 2.45M-row × 38-feature matrix as nested per-row dicts,
  ~4.5GB+, then `prepare` building a second full copy) → fixed by a
  columnar Python loader (`ts`/`asset`/`x`/`fwd` arrays, commit `55dcc76`,
  "Load the matrix columnar so a 2.4M-row fit fits in memory").
- **NaN/null `oi_change_60` rows** — the Rust gate refused the matrix with
  `invalid type: null, expected f64` on rows where `oi_change_60`'s
  `ln(cur/old)` went non-finite (zero/negative OI 60 minutes earlier) →
  fixed by a finiteness invariant (`features-scalper`, a feature is
  `Some` only if finite; `matrix.rs` drops and counts non-finite rows;
  commit `5f8e77a`, "A feature is Some only if it is finite"). The
  matrix was rebuilt from the same frozen data under this fix: 2,447,682
  → 2,446,569 total lines (manifest + kept rows) — a drop of **1,113**
  lines. The commit message accompanying `5f8e77a` states "1,114 rows"
  from its own count at commit time; the line-count difference measured
  directly from both matrix files (2,447,682 − 2,446,569) is authoritative
  here and is 1,113, one fewer than the commit message's figure.
- **Rust gate loader SIGKILL at the 7GB memory guard** —
  `gate.rs::load_matrix` read the whole 3.3GB file into a `String`, then
  parsed every row into an owned, non-interned `BTreeMap<String, f64>`
  (`MatrixRow`), never dropping the source `text` before `rows` finished
  building — plausibly ~8-10GB resident for `rows` alone on top of the
  ~3.3GB `text` buffer, at 2,446,568 rows. The h15 gate step hit a
  `systemd-run --scope --user -p MemoryMax=7G` ceiling exactly (peak RSS
  7,168 MB, SIGKILL, exit 137) before this fix. Fixed by a columnar Rust
  reader mirroring the Python fix (commit `48af1ca`, "Read the matrix
  columnar in the gate too"), **verified byte-identical against gate run
  1's own reports**: rerunning the fixed `gate` binary against run 1's
  frozen matrix/folds reproduced `data/reports/gate-run-1-h15.json` and
  `-h30.json` byte-for-byte (diff excluding `generated_utc` = empty;
  console output matched the doc exactly — h15: 4 folds, 468 trades, PASS,
  sharpe=6.305896889670681; h30: 4 folds, 605 trades, PASS,
  sharpe=2.0620...). The parity check as performed covered h15 and h30
  only, not h60; there is no reason to expect h60's fixed loader to behave
  differently, but that expectation was not separately verified.
- With the loader fixed, h15's gate step succeeded under the lighter
  `ulimit -v 9000000` guard (peak RSS 1,088,364 KB, exit 0), and h30/h60's
  fits and gates ran sequentially under the same `ulimit -v 9000000`
  guard, in one script, without incident.

**None of these three fixes changed any pre-registered definition.** fs-3
(commit `f6c3951`) is unchanged by any of them — they are matrix-load and
gate-load memory/validity fixes (a stricter finiteness rule on one
feature's degenerate inputs, and two loaders becoming columnar instead of
row-of-dicts/row-of-maps), not changes to feature definitions, fold
schedule, cost model, fee schedule, or gate threshold. The byte-identical
parity check against run 1's own reports is the direct evidence for that
claim on the gate-loader side; run 1's own frozen matrix and fold files
were never touched.
