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
bin/scalper-pull.sh [days]   # default 3
```

Cron it:

```
20 0 * * *  cd /path/to/ai-trader && bin/scalper-pull.sh >> var/live/scalper-pull.log 2>&1
```

The script reads `data/scalper-universe.json` itself, takes every coin whose
`binance_um` field is non-null — the universe's **mapped** candidates only —
and calls `pull-binance-perp --data-root data/perp --assets <that list>
--start <today - days> --end <tomorrow>` in Hyperliquid's own casing
(`kPEPE`, not `KPEPE`): `resolve_asset` needs the original case to recognize
HL's lowercase-`k` thousandths prefix before it uppercases for the store
key. A coin with no UM listing is skipped with a warning by the command
itself, not silently substituted with spot — but it's cheaper to just not
pass it, since it can never contribute a matrix row.

`pull-binance-perp` has no cache-skip: it always re-fetches every month in
`[--start, --end]`, even ones already on disk. The smoke run confirmed this
is harmless, just redundant bandwidth (Task 5: BTC/ETH's already-cached
2026-06/07 months were re-fetched without incident). So the nightly job is
simple — re-pull the last few days, every night, for every mapped asset. No
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

*(Superseded by Amendment 1 as the gate's cost source — Binance UM
`binance-costs` is what a gate-eligible run charges now. This section still
runs unchanged: HL recording continues accruing as the future HL-port
option. See Amendment 1.)*

```bash
bin/scalper-record.sh [top] [seconds] [interval]   # default 25 3600 10
```

Cron it:

```
0 * * * *  cd /path/to/ai-trader && bin/scalper-record.sh >> var/live/scalper-record.log 2>&1
```

Each invocation polls Hyperliquid L2 books for the live top-25 markets by
`day_volume_usd()` (recomputed at invocation time, not pinned to the last
universe refresh) at 10-second intervals for a full hour — 360 rounds per
coin — then exits. The next hour's cron tick starts the next invocation.
Running it as back-to-back hour-long jobs rather than one free-running
process means a hung recorder gets replaced by the next tick instead of
silently stopping coverage for the rest of the day.

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

Deepening `data/perp` between a gate run and its re-run changes the
matrix's stride phase and warmup window and therefore its rows - pull any
new history *before* a gate cycle starts, never between a run and its
re-run.

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

1. **At least 4 weeks of recorded book data.** *(Superseded by Amendment 1's
   condition 1 — an 18-month matrix span on Binance archives, not this HL
   book-recording bar, gates a Binance run. Original text below, unchanged,
   for the HL-costed path.)* `data/books/` must cover at least 28 distinct
   UTC days feeding the cost summary passed to `gate`. Anything less (the
   smoke run's 10 minutes included) produces a number, but not a
   gate-eligible one.
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

Reading the diagnostics fields the gate report also carries: each
`folds[].ic`/`rank_ic` is that fold's own model's skill (Pearson / Spearman
IC of its predictions against realized returns), while `overall.ic`/`rank_ic`
pools predictions across ALL folds — and every fold is a different,
independently-fit model — into one walk-forward-aggregate number. Don't read
`overall.ic` as "the model's" IC; there is no single model to attribute it
to.

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

**Funding-rate feature is deferred, not implemented.** *(Superseded by
Amendment 1 / Plan 3c Task 2 — `fs-rust-scalper-2` implements
`funding_rate_bps` and `funding_x_mom` from Binance's funding archive.
Original text below describes fs-1, which is no longer what a gate-eligible
run uses.)* The spec's feature
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
20bps default.** *(Superseded by Amendment 1 for the Binance gate path —
`binance-costs`/`gate --binance-costs` has its own untradeable-day
mechanism, "day found within the 14-day lookback or not," reported as
`days_without_costs`; there is no 20bps default on that path. This
paragraph still describes the HL-costed `gate --costs` path unchanged.)*
`gate`'s cost lookup (`compute_round_trip_bps`) charges
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

---

## Amendment 1 (2026-08-14): Binance venue

*(This section amends, and does not delete, the protocol above. Where it
conflicts with §1-§6, this section governs. Paragraphs superseded by this
amendment are marked in place, above, with a one-line note — the original
text stays, unedited, immediately below each note.)*

User decision (2026-08-13): prove profitability on Binance UM first. HL
becomes a later port, justified by buying vendor data only after a Binance
PASS — see "Funding" and "HL recording continues" below.

**Venue: Binance USDT-M futures (UM).** The gate simulates execution on
Binance UM, not Hyperliquid: Binance klines (§1) for bars, Binance
microstructure archives (`binance-costs`, Plan 3c Task 3) for time-varying
costs, and Binance's own published fee schedule (below) for fees. HL book
recording (§2) is no longer the cost source for the gate — see below for
what it's still for.

**Gate eligibility, amended.** A run is gate-eligible only if, in addition
to the existing §5 conditions that still apply unchanged (the one-drop-one-
rerun universe rule, the Sharpe > 2.0 bar on out-of-sample daily returns):

1. **≥18 months of matrix span** (`training-matrix --start`/`--end`), not
   the 4-weeks-of-book-recording bar §5.1 set for the HL protocol — Binance's
   archives go back years, so there's no reason to gate on a thin window.
   Supersedes §5, condition 1 (the "≥28 distinct UTC days" book-recording
   rule) — HL's cost source no longer determines eligibility.
2. **Every mapped, 90-day-eligible candidate is in the matrix** — unchanged
   from §5.2.
3. **fs-2 features** (`FEATURE_SET_VERSION = "fs-rust-scalper-2"`,
   `features-scalper`) — fs-1 matrices are refused by the gate against fs-2
   fold artifacts and vice versa; a gate-eligible run must be fs-2 end to
   end.
4. **Time-varying costs via `binance-costs` + `gate --binance-costs`** — not
   the flat `--costs` summary. The concrete commands:

   ```bash
   service/target/release/scalper-data binance-costs \
     --data-root data --universe data/scalper-universe.json \
     --start <matrix-start> --end <matrix-end> \
     --out data/binance-micro/costs-daily.json --notional <live notional>

   service/target/release/scalper-data gate \
     --matrix data/matrices/gate-run.jsonl \
     --folds data/models/gate-run-h<horizon>/folds.json \
     --binance-costs data/binance-micro/costs-daily.json \
     --fee-taker-bps <F> --fee-maker-bps <F> \
     --notional <live notional> \
     --out data/reports/gate-run-h<horizon>.json
   ```

   (Note for anyone reading Task 4's plan bullet literally: `binance-costs`
   takes `--universe`, not `--assets` — the plan doc's own bullet is stale
   on this point; the flags above are what `scalper-data`'s USAGE block
   actually defines.)
5. **All three horizons — 15, 30, 60 — every run, all three reported.**
   Unchanged from §5.3.

**Fee fixed-point rule (pre-registered).** Fees are charged as follows, with
no discretion once a run starts:

- **Run 1** charges **VIP0 + BNB-discount taker: 4.50 bps**
  (`--fee-taker-bps 4.5 --fee-maker-bps 1.8`), the verified rate in
  `docs/binance-um-fee-table-2026-08.md`.
- After run 1, read `overall.projected_30d_volume_usd` from its gate
  report. Map that figure to a tier in the fee-table snapshot **by the
  volume criterion only** — assume the BNB-balance requirement for that
  tier is met, since the fee-table snapshot doesn't resolve every tier's
  BNB threshold (see its "Gap" section) and the strategy's account is
  assumed to hold whatever BNB the tier needs.
- **If the mapped tier differs from VIP0, run exactly ONE re-run** at that
  tier's fees. Report both runs' full output.
- **The second run's verdict is the gate verdict.** Not the first, not
  whichever is more favorable. If the fee-table snapshot doesn't cover the
  mapped tier (today it only resolves VIP0 and VIP9 — see the snapshot's
  "Gap" section), re-fetch Binance's authenticated fee schedule before
  doing the one allowed re-run; don't substitute an unverified number.
- **No further iteration.** One run at VIP0+BNB, at most one re-run at the
  volume-mapped tier, then the number is the number — same discipline as
  §5's universe-selection rule.

**Funding cost is not charged, pre-registered as negligible.** The
arithmetic: strategy holds are ≤60 minutes; crossing probability is bounded
by `hold_minutes / interval_minutes`, and the interval is not uniformly 8
hours — Binance UM lists some contracts on a 4-hour funding cycle (our own
`FundingRow` records this per-symbol as `funding_interval_hours`), so the
bound must use the shortest interval observed in the universe, not the
common case. At the 60-minute horizon with a 4-hour (240-minute) interval:
`60/240 = 25%` crossing probability; typical Binance UM funding rates run
around 1bp per period; so expected funding cost per trade is bounded by
roughly `0.25 × 1bp = 0.25bp`, comfortably under the stated **< 0.3
bps/trade** bound this amendment pre-registers. (For an 8-hour-interval
contract the same arithmetic gives `60/480 ≈ 12.5%`, i.e. `≈0.125bp` —
the 4-hour case is the binding one and the one the bound is set against.)
This is deliberately not folded into `round_trip_bps` — it's a documented,
bounded omission, not a silent gap.

**HL recording continues accruing.** §2's hourly `bin/scalper-record.sh` job
is not being turned off by this amendment. It's superseded only as the
gate's cost *source*: the paragraph in §2 describing `summarize-costs`'s
output as "the per-coin cost summary the gate consumes" now describes the
flat-cost, HL-execution path (`gate --costs`), which remains available but
is no longer what a Binance gate-eligible run uses (see condition 4 above).
The HL book history keeps accruing in parallel as a live option on a future
HL port.

**"Buy HL vendor data (e.g. Tardis)" is the PASS-branch action, named
explicitly.** If a Binance gate run under this amendment's rules produces a
PASS, the next step — and only then — is to buy Tardis (or equivalent) HL
historical book/trade data to build an HL-costed gate run for a possible
port, rather than waiting on HL's own book recording to accumulate 18
months organically. A FAIL on Binance is not grounds to buy HL data hoping
the other venue does better — §5's FAIL handling (stop, or return to
feature research under a new `FEATURE_SET_VERSION`) applies exactly as
written, venue included.
