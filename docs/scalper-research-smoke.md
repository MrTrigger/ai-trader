# Crypto scalper research pipeline — end-to-end smoke

> **This is a plumbing smoke test, not a gate run.** It proves that
> `pull-binance-perp`, `universe`, `training-matrix`, `walk_forward_scalper.py`,
> and `scalper-data gate` connect correctly and produce a well-formed report.
> **It does not, and cannot, claim the >= 2.0 Sharpe gate.** The gate decision
> requires at least four weeks of recorded order-book costs and the full
> mapped candidate universe (see `docs/superpowers/plans/2026-08-13-crypto-scalper-plan-3-signal-research.md`,
> Task 6). Here we have six assets, a ten-minute cost sample standing in for
> four-plus weeks, and one six-month period. No result below should be read as
> evidence for or against the strategy.

**Run date:** 2026-08-13
**Worktree:** `.claude/worktrees/scalper-plan3` (data/ is gitignored, per-worktree, not committed)

## Adjustment from the brief

The brief's Step 3 pointed at "the plan-2 smoke cost summary". That file does
not exist in this worktree — `data/` is per-worktree and this worktree never
ran plan 2's book recording. Before Step 3 we generated a stand-in cost file
ourselves:

1. `record-books --data-root ../data --seconds 600 --interval 10 --assets BTC,ETH,SOL,DOGE,XRP`
   — 10 minutes of live Hyperliquid L2 book snapshots (60 rounds x 5 assets =
   300 snapshots), written to `../data/books/2026-08-13.jsonl`.
2. `summarize-costs --data-root ../data --start 2026-08-13 --end 2026-08-13`
   — turned those 300 snapshots into `../data/costs/summary-2026-08-13-2026-08-13.json`.

**This cost sample is ~10 minutes of one day's book depth, standing in for
the >= 4 weeks the real gate protocol requires.** Spreads and depth on
Hyperliquid can and do vary by time of day, day of week, and market regime;
a 10-minute snapshot says nothing about that variation. Every cost-derived
number below inherits this limitation.

## Commands run, in order

### Step 1 — pull 6 months of perp 1m bars, five assets

```
cargo run -p scalper-data -- pull-binance-perp --data-root ../data/perp \
  --assets BTC,ETH,SOL,DOGE,XRP --start 2026-02-01 --end 2026-08-01
```

Ran 07:56:51-07:58:33 UTC (~1m42s). BTC and ETH had 2026-06/07 already
cached from an earlier task; this re-fetched those months too (the command
has no cache-skip — harmless, just redundant bandwidth). Every one of the 30
asset-months (5 assets x 6 months) resolved with real bar counts; **no
month 404'd for any asset** — DOGE, XRP, and SOL are all long-listed on
Binance UM, exactly as expected, so there was nothing to investigate.

| asset | 2026-02 | 2026-03 | 2026-04 | 2026-05 | 2026-06 | 2026-07 |
|---|---:|---:|---:|---:|---:|---:|
| BTC | 40,320 | 44,640 | 43,200 | 44,640 | 43,200 | 44,640 |
| ETH | 40,320 | 44,640 | 43,200 | 44,640 | 43,200 | 44,640 |
| SOL | 40,320 | 44,640 | 43,200 | 44,640 | 43,200 | 44,640 |
| DOGE | 40,320 | 44,640 | 43,200 | 44,640 | 43,200 | 44,640 |
| XRP | 40,320 | 44,640 | 43,200 | 44,640 | 43,200 | 44,640 |

Total: **1,303,200 bars written** (~260,640/asset, matching the ~260k/asset
estimate in the brief).

kPEPE was **not** re-pulled — it already had 2026-06 and 2026-07 bars in the
store from an earlier task (`asset=KPEPE`), so it rode along in Step 2 with a
shorter, two-month history.

### Step 2 — universe refresh + training matrix

```
cargo run -p scalper-data -- universe --data-root ../data --top 25
cargo run -p scalper-data -- training-matrix --data-root ../data \
  --universe ../data/scalper-universe.json --start 2026-02-01 --end 2026-08-01 \
  --out ../data/matrices/smoke-2026-02-08.jsonl --stride 5
```

Ran 07:58:33-07:59:53 UTC (~80s total for both). Universe refresh pulled the
live top-25 Hyperliquid candidates by day volume (mapped to Binance UM where
listed); `kPEPE` (day_volume_usd ~$3.77M) landed at rank 25.

`training-matrix`'s skip-with-warning behavior was **verified, not
reimplemented** — it already exists at `service/crates/scalper-data/src/main.rs:537-544`
and is covered by the passing test
`training_matrix_skips_a_universe_candidate_with_no_bars_in_the_store`. Of
the 25 universe candidates, 19 were skipped with a one-line `<coin>: skipped
(no bars in ../data/perp)` warning (no local store), and `HYPE` was silently
excluded earlier for having no Binance UM mapping (existing, correct
behavior — not printed as a warning, since it's filtered before the
bars-lookup step). Six candidates had bars and produced rows:

| asset | rows |
|---|---:|
| BTC | 52,104 |
| ETH | 52,104 |
| SOL | 52,104 |
| XRP | 52,104 |
| DOGE | 52,104 |
| kPEPE | 17,544 |

Total: **278,064 rows, 6 assets**, written to `../data/matrices/smoke-2026-02-08.jsonl`.
No code changes were needed anywhere in this step.

### Step 3 — per-horizon fold fit + gate

For each horizon in {15, 30, 60}:

```
uv run python walk_forward_scalper.py --matrix ../data/matrices/smoke-2026-02-08.jsonl \
  --horizon H --out-dir ../data/models/smoke-hH
cargo run -p scalper-data -- gate --matrix ../data/matrices/smoke-2026-02-08.jsonl \
  --folds ../data/models/smoke-hH/folds.json \
  --costs ../data/costs/summary-2026-08-13-2026-08-13.json \
  --out ../data/reports/smoke-hH.json
```

Default anchored-expanding-window fold schedule (90-day train floor, 30-day
test, 30-day step) produced **3 folds per horizon** over the 6-month matrix
span, training slices growing from ~130k to ~225k rows — all comfortably
above the 50,000-row training floor. Each horizon's fold fit (3 LightGBM
fits) completed in well under a minute; each gate run in well under 30
seconds — small-universe, single-machine, in-worktree Rust inference is
fast, as expected. No fit was slow enough to need patience.

There was an unrelated multi-hour gap between finishing the book recording
(08:08:59 UTC) and running `summarize-costs` onward (14:43:21 UTC) — an
operational interruption in this session, not pipeline latency. Noted for
transparency; it does not affect any of the numbers below (the recorded
10-minute book sample is timestamped 2026-08-13 regardless of when it was
later summarized).

## Per-horizon results

**These numbers are plumbing output, not evidence.** Six assets (one
of them, kPEPE, with only two months of history), a 10-minute cost sample
standing in for four-plus weeks, and a single six-month period is not a
gate-eligible run under any reading of the protocol.

| horizon | n_trades | sharpe_annualized | gate |
|---:|---:|---:|---|
| 15 min | 176 | -2.7787 | FAIL |
| 30 min | 0 | null (no trades) | NO-TRADES |
| 60 min | 0 | null (no trades) | NO-TRADES |

h15 fold detail: fold 0 (test 2026-05-02..06-01) and fold 2 (test
2026-07-01..07-31) traded zero times; all 176 trades came from fold 1 (test
2026-06-01..07-01). Per-asset breakdown for h15 (the only horizon with any
trades):

| asset | n_trades | total_net_bps | hit_rate |
|---|---:|---:|---:|
| BTC | 25 | -184.99 | 0.480 |
| DOGE | 32 | -941.07 | 0.344 |
| ETH | 31 | -831.48 | 0.355 |
| SOL | 67 | -815.99 | 0.433 |
| XRP | 21 | +303.42 | 0.524 |

No asset was excluded as a thin book at any horizon (`excluded_thin_books`
is `[]` in all three reports) — every asset present in the cost file had a
fillable `cross_bps["5000"]`.

**Why this doesn't count, even where the number looks interesting (it
doesn't here, but the caveat applies regardless of sign):** five assets is
not the candidate universe; a 10-minute book sample cannot represent real
cost variance over weeks; three folds over six months is one period, not an
out-of-sample record; and h15's FAIL / h30-h60's NO-TRADES are exactly as
uninformative as a PASS would have been about the strategy's real edge. The
gate protocol (Task 6) is the only process allowed to produce a number that
counts.

## kPEPE identity sanity check

The earlier review flagged a specific risk: if `kPEPE` appears in the
universe with local store data, could it silently land in the 20bps default
cost bucket while actually present in the costs file under a different
case, masking a case-sensitivity bug?

Traced the coin-name identity through the pipeline:

- `universe` writes `Candidate.coin` verbatim from Hyperliquid's own naming
  (`"kPEPE"`, mixed case) — confirmed in `../data/scalper-universe.json`.
- `training-matrix` uses `candidate.coin.to_uppercase()` (`"KPEPE"`) **only**
  as the bar-store lookup key (matches the on-disk `asset=KPEPE` partition),
  but stamps `MatrixRow.asset` with the original `candidate.coin`
  (`"kPEPE"`) — see `service/crates/scalper-data/src/matrix.rs:101` and its
  doc comment ("asset is the universe coin name ... not the uppercased store
  key").
- `record-books` and `summarize-costs` key `CostSummary` by `snap.coin`,
  which is Hyperliquid's own coin name — the same mixed-case string the
  universe file uses, since both come from the same Hyperliquid API.
- `gate` looks costs up by `costs.get(asset)` where `asset` is
  `MatrixRow.asset` — i.e. `"kPEPE"` vs. a `"kPEPE"` key. Case-consistent by
  construction; there is no uppercase/lowercase seam anywhere in this path.

**In this run specifically**, kPEPE is genuinely **absent** from the cost
file — we only recorded books for BTC/ETH/SOL/DOGE/XRP (the five Step-1
assets), not kPEPE. So kPEPE correctly fell into `DEFAULT_ROUND_TRIP_BPS`
(20bps) via the documented "asset entirely absent from costs" path in
`gate.rs::compute_round_trip_bps` — not a case-mismatch bug, just an asset
we didn't record. At 20bps round-trip and 1.5x threshold, kPEPE needed a
|predicted| move over 30bps to trade; across all three horizons it produced
**zero trades** (it never appears in any `per_asset` map, and it is not in
any `excluded_thin_books` list either — it was simply eligible and never
triggered). Plausible given kPEPE contributes only ~6% of training rows
(17,544 of 278,064) and both non-trading h15 folds (0 and 2) also produced
zero trades for the five majors. **No evidence of a case-identity bug; the
default-bucket behavior for kPEPE here is the "absent from costs" path
working as documented, not the "case mismatch" failure mode the review was
checking for.**

## Test suite

- `cargo test -p scalper-data` (in `service/`): 24 passed, 0 failed — before
  and after this smoke run; no source changes were made.
- `uv run pytest` (in `training/`): 20 passed, 0 failed.

No code changes were required anywhere in this task — Step 2's
skip-with-warning behavior already existed and already had test coverage.

## Bottom line

The pipeline runs end to end: real Binance UM downloads, a real Hyperliquid
book recording, a real cost summary, real LightGBM walk-forward fits, and a
real Rust gate simulation, writing well-formed reports at
`../data/reports/smoke-h{15,30,60}.json`. That is all this run proves.

**This validates plumbing; the gate decision requires >= 4 weeks of recorded
book costs and the full candidate universe, per the gate protocol in
Task 6. No PASS/FAIL/NO-TRADES result in this document is a gate verdict.**
