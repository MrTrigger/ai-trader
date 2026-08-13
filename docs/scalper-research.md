# Scalper research pipeline

This is the machinery from Plan 3: pull bars, record book costs, refresh the
candidate universe, build the training matrix, fit folds, and run the Rust
gate. Plan 3 built and smoke-tested every stage of it end to end
(`docs/scalper-research-smoke.md`) without claiming the gate — five assets, a
ten-minute cost sample standing in for four-plus weeks, one six-month period.
That document proves the plumbing connects. This document is the runbook for
the real run, and — written now, before that run happens — the rule for
reading its result.

Nothing here touches `crypto-portfolio`. Different crate (`scalper-data`),
different data root, and eventually a different account. The frozen
rank-reward strategy in `docs/operations.md` is unaffected by anything below.

All `scalper-data` commands: build once with `cargo build --release -p
scalper-data` (in `service/`), then run `service/target/release/scalper-data
<command>` from the repo root — paths below are repo-root-relative. Python
commands (`walk_forward_scalper.py`) run with `uv run python
walk_forward_scalper.py ...` from `training/`, per its `pyproject.toml`.

---

## 1. Nightly: pull Binance perp bars

```bash
service/target/release/scalper-data pull-binance-perp \
  --data-root data/perp \
  --assets BTC,ETH,SOL,DOGE,XRP,kPEPE,... \
  --start 2026-08-01 --end 2026-08-14
```

The asset list is the universe's **mapped** candidates only — every coin in
`data/scalper-universe.json` whose `binance_um` field is non-null. Pass the
coins in Hyperliquid's own casing (`kPEPE`, not `KPEPE`): `resolve_asset`
needs the original case to recognize HL's lowercase-`k` thousandths prefix
before it uppercases for the store key. A coin with no UM listing is skipped
with a warning by the command itself, not silently substituted with spot —
but it's cheaper to just not pass it, since it can never contribute a matrix
row.

`pull-binance-perp` has no cache-skip: it always re-fetches every month in
`[--start, --end]`, even ones already on disk. The smoke run confirmed this
is harmless, just redundant bandwidth (Task 5: BTC/ETH's already-cached
2026-06/07 months were re-fetched without incident). So the nightly job is
simple — re-pull the current month, every night, for every mapped asset. No
incremental logic to get wrong.

`--data-root data/perp` is mandatory and must stay separate from the frozen
bot's daily spot store: `pull-binance-perp` refuses to write perp minutes
into a root that already holds an `interval_s=86400` partition, specifically
so this pipeline can never commingle data the frozen bot was never tuned
against.

**Backfilling a newly added coin** is a separate, one-time pull, not part of
the nightly job: when the weekly universe refresh (§3) adds a coin, pull its
full history back to when it needs to reach 90 days before it's eligible for
training.

---

## 2. Hourly: record book costs

```bash
service/target/release/scalper-data record-books \
  --data-root data \
  --top 25 --seconds 3600 --interval 10
```

Cron this hourly. Each invocation polls Hyperliquid L2 books for the live
top-25 markets by `day_volume_usd()` (recomputed at invocation time, not
pinned to the last universe refresh) at 10-second intervals for a full hour
— 360 rounds per coin — then exits. The next hour's cron tick starts the
next invocation. Running it as back-to-back hour-long jobs rather than one
free-running process means a hung recorder gets replaced by the next tick
instead of silently stopping coverage for the rest of the day.

Snapshots accrue at `data/books/{YYYY-MM-DD}.jsonl`, one UTC-day file,
flushed every round. A coin's fetch or book error is a warning, not a reason
to stop the run — one bad coin in one round doesn't lose the other 24.

`summarize-costs --data-root data --start ... --end ...` turns the
accumulated snapshot files into the per-coin cost summary the gate consumes
(`data/costs/summary-{start}-{end}.json`). Run it once the recording window
is long enough to gate on (§5) — there's no reason to run it nightly.

---

## 3. Weekly: universe refresh

```bash
service/target/release/scalper-data universe \
  --data-root data --top 25
```

Cron this weekly. It lists the live top-25 Hyperliquid markets by day
volume, maps each to its Binance UM symbol where listed, and overwrites
`data/scalper-universe.json`. Coins with no Binance mapping print with a
`NO-BINANCE` marker and are excluded from training everywhere downstream —
that's existing, correct behavior, not something this refresh needs to
special-case.

**A coin newly appearing in the universe does not enter training the same
week it appears.** It needs 90 days of pulled Binance history first — the
same 90-day figure as `walk_forward_scalper.py`'s default train floor
(`DEFAULT_TRAIN_DAYS = 90`); a coin with less history than that can't
possibly fill a first fold's training window, so including it early doesn't
buy a real walk-forward record, just a truncated one. The smoke run is the
concrete example of what that looks like: kPEPE rode along with only two
months of Binance history and contributed a truncated 17,544 of 278,064
matrix rows (6%) — acceptable for a plumbing smoke test, not acceptable
input to a gate run.

Operationally: track when each coin first cleared its Binance backfill
(§1), and don't hand a coin to `training-matrix --universe ...` for a gate
run until 90 days have passed since then.

---

## 4. On demand: matrix, fold-training, gate

```bash
# the matrix — one row per fully-warm bar per mapped, 90-day-eligible asset
service/target/release/scalper-data training-matrix \
  --data-root data --universe data/scalper-universe.json \
  --start 2026-02-01 --end 2026-08-01 \
  --out data/matrices/gate-run.jsonl --stride 5

# per horizon: purged fold fits in Python, then the Rust gate
for horizon in 15 30 60; do
  uv run python walk_forward_scalper.py \
    --matrix data/matrices/gate-run.jsonl --horizon "$horizon" \
    --out-dir "data/models/gate-run-h${horizon}"

  service/target/release/scalper-data gate \
    --matrix data/matrices/gate-run.jsonl \
    --folds "data/models/gate-run-h${horizon}/folds.json" \
    --costs data/costs/summary-<start>-<end>.json \
    --notional <intended live per-trade notional> \
    --out "data/reports/gate-run-h${horizon}.json"
done
```

`training-matrix` skips any universe candidate with no bars in
`data/perp` — a one-line `<coin>: skipped (no bars in data/perp)` warning,
not a fatal error (verified against `training_matrix_skips_a_universe_
candidate_with_no_bars_in_the_store` in Task 5). For a gate run, that
warning should never fire for a mapped, 90-day-eligible candidate — if it
does, §1's nightly pull missed something and the run isn't gate-eligible
until it's fixed.

`walk_forward_scalper.py`'s default fold schedule is an anchored, expanding
window: 90-day train floor, 30-day test, 30-day step. `train_scalper.py`
additionally refuses to fit any fold whose pooled training slice is under
`MIN_ROWS = 50,000` rows. Both are existing, tested defaults — don't
override them per run to make a thin matrix fit; a matrix too small to clear
them honestly isn't ready to gate.

`--notional` on `gate` defaults to `5000` — for a real gate run, set it to
the notional the strategy would actually intend to trade live, not the
default. Costs are notional-dependent (`cross_bps[notional]` from the
recorded book depth), so gating at the wrong size measures the wrong thing.

---

## 5. The gate protocol, pre-registered before any numbers exist

This is the rule. It's written now, before a single gate-eligible number has
been produced, specifically so no future number gets to argue with it.

**A run is gate-eligible only if all of the following hold:**

1. **At least 4 weeks of recorded book data.** `data/books/` must cover at
   least 28 distinct UTC days feeding the cost summary passed to `gate`.
   Anything less (the smoke run's 10 minutes included) produces a number,
   but not a gate-eligible one.
2. **Every mapped, 90-day-eligible candidate is in the matrix.** No
   pre-filtering the universe down to "the ones that look promising" before
   the first run — that's the failure mode this protocol exists to prevent.
3. **All three horizons — 15, 30, 60 — run every time.** Never run one
   horizon, see the result, and decide whether the others are worth trying.
   All three, every run, all three reported.
4. **Gate = overall out-of-sample annualized Sharpe > 2.0** on daily net
   returns (`overall.sharpe_annualized` in the gate report, √365-annualized,
   out-of-sample folds only), with measured per-symbol costs at the
   notional the strategy actually intends to trade.

**Universe selection is the one place iteration is allowed, and it is
exactly this and nothing more:**

- After the first full run (all three horizons, full mapped universe), a
  symbol may be dropped **only** if it has negative `total_net_bps` in the
  `per_asset` breakdown of **all three** horizon reports from that run — the
  pre-registered rule, no other reason. Requiring unanimity across horizons
  is the strictest evidence standard for exclusion, which minimizes
  universe-selection overfitting, and it's the only reading consistent with
  the single shared `--exclude` list and matrix rebuild below: there is one
  exclude list, not one per horizon.
- Drop exactly those symbols (`universe --exclude <symbol>,...`), rebuild
  the matrix, refit the folds, and rerun the gate **once** — the same
  exclude list, and the same rebuilt matrix, feeding all three horizons.
- That is the entire allowed process. No second round of dropping. No
  rerunning with a different `--threshold-mult` or `--notional` to see if
  the number moves. No cherry-picking a horizon that happened to pass while
  ignoring the two that didn't. No adding a symbol back because the drop
  made things worse. One drop, one re-run, then the number is the number.

**A FAIL — on either the first run or the one allowed re-run — means the
project stops or returns to feature research.** Not: lower the 2.0
threshold. Not: keep adding horizons until one clears. Not: shrink the
notional until costs look better. Not: re-run with a different fold schedule
until a favorable window turns up. "Returns to feature research" means new
signal work under a new `FEATURE_SET_VERSION`, followed by this exact
protocol run again from a fresh first run — not a second attempt at
massaging the same features past the same gate.

A NO-TRADES result is not a lesser failure that deserves a different
process — the smoke run's h30/h60 NO-TRADES and h15's FAIL were called out
as equally uninformative outside a gate-eligible run, and inside one they're
read the same way a FAIL is: stop, or go back to feature research.

---

## 6. Known gaps that matter here

**Funding-rate feature is deferred, not implemented.** The spec's feature
catalog lists funding rate as a candidate; `features-scalper`'s 26-feature
v1 (`FEATURE_SET_VERSION = "fs-rust-scalper-1"`) does not include it — it
needs a Binance UM funding-archive ingestion job that doesn't exist yet.
It's recorded as the first candidate for `fs-rust-scalper-2`. This means
every gate run under the current feature set is scored without funding
context; do not read a FAIL as final evidence against the strategy without
noting that a plausible signal is still missing, and do not add a
funding source that's only available live (not in backtest) to close the
gap early — that would silently break train/live parity.

**kPEPE-tier coins need their own book recording, or they gate at the
20bps default.** `gate`'s cost lookup (`compute_round_trip_bps`) charges
`DEFAULT_ROUND_TRIP_BPS = 20.0` for any symbol entirely absent from the cost
summary — confirmed correct, not-optimistic behavior in the smoke run's
kPEPE trace, and not a case-mismatch bug (coin identity is case-consistent
end to end: `universe` writes HL's own casing, `record-books` and
`summarize-costs` key by that same casing, `training-matrix` only uppercases
for the store lookup, and `gate` looks costs up by the un-uppercased
`MatrixRow.asset`). But the default is a stand-in, not a measurement — it's
`DEFAULT_ROUND_TRIP_BPS`, and Global Constraints are explicit that costs are
never optimistic. §2's hourly `--top 25` recording mostly covers this
automatically, since the universe and the book recorder both select by live
volume rank — but a coin whose rank flickers in and out of the live top-25
during the 4-week window can end up with gaps, or be entirely absent, in the
cost summary. Before treating a gate run as valid, check that every coin in
the matrix actually has an entry in the cost summary used — a candidate
gating on the 20bps default hasn't had its real cost measured, and its
`total_net_bps` (which feeds §5's one allowed drop rule) is only as good as
that default.
