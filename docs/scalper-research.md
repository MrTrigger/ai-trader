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

---

## Amendment 2 (2026-08-15): fs-3 for eligibility

*(This section amends, and does not delete, Amendment 1 and the protocol
above. Where it conflicts with either, this section governs. This
amendment is committed before any code change it describes — the fs-3
substitution below does not exist in `features-scalper` yet at the moment
this text is written.)*

**Why: band availability, not model results.** Gate run 1
(`docs/scalper-gate-run-1.md`) came back not gate-eligible on Amendment
1's condition 1 (≥18 months of matrix span) for a reason discovered
during that run, not anticipated by the runbook or the plan that preceded
it: fs-2's `depth_imb_02` needs the ±0.2% book-depth band, and that band
is simply absent from the ingested `bookDepth` archive before a fixed
date. The verified evidence, from gate run 1's own inspection plus the
follow-up check this amendment relies on:

- The ±0.2% band **first appears** in `data/binance-micro/book/BTCUSDT/2026-01-15.jsonl`
  at ts `1768460460` (2026-01-15 07:01 UTC). The snapshot immediately
  before it, ts `1768460400` (07:00 UTC, same file), still carries only
  the `-1.0`/`1.0` band keys; every day file before 2026-01-15 has only
  those two keys (gate run 1 confirmed this by inspecting BTCUSDT's day
  files from 2024-08-01 through 2026-01-14).
- The ±1.0% band, by contrast, **exists for the full ingested history**.
  Re-verified today (2026-08-15) by spot-checking day files across three
  assets spanning the universe's coverage tiers — BTC, ZEC, and KAITO —
  back to each asset's earliest 2024-08 day file: the `-1.0`/`1.0` keys
  are present in every file checked, with no gap corresponding to the
  ±0.2% one.
- fs-2's `depth_imb_02` (index 27), `depth_02_z_60` (index 29), and
  `depth_slope` (index 37) each read `bid_02`/`ask_02`, so `training-
  matrix`'s all-Some requirement drops every row before 2026-01-15 07:01
  UTC — the ~6.9-month actual span gate run 1 measured, against the
  ≥18-month bar. fs-1's 26 features and fs-2's `depth_imb_10` (index 28)
  read only the ±1.0% band and are unaffected.

This is a data-availability ceiling in what Binance's archive publishes
before 2026-01-15, external to this codebase — not a bug in `training-
matrix`'s row logic, and not evidence about the strategy either way.

**What changes: fs-3, three features substituted, everything else held
fixed.** `features-scalper` gains `FEATURE_SET_VERSION =
"fs-rust-scalper-3"`. Feature count stays 38. Only indices 27, 29, and 37
change name/definition; index 28 (`depth_imb_10`) and all other 34
indices are byte-for-byte the same as fs-2 (and, for the 26 fs-1
features, the same as fs-1). `MicroMinute` keeps its `bid_02`/`ask_02`
fields exactly as they are (they may be `None`) — fs-3 simply never reads
them:

| # | fs-2 | fs-3 | Rationale |
|---|------|------|-----------|
| 27 | `depth_imb_02` = (bid_02−ask_02)/(bid_02+ask_02+eps) | **`depth_10_z_60`** = z-score of (bid_10+ask_10) over a trailing 60-minute, gap-reset, full window | Keeps a depth-*dynamics* signal, moved to the band that exists. |
| 29 | `depth_02_z_60` | **`depth_10_log`** = ln(bid_10+ask_10+eps) | Keeps a depth-*level* signal (a liquidity-regime feature, distinct from 27's z-score) at the surviving band. |
| 37 | `depth_slope` = ln((bid_10+ask_10+eps)/(bid_02+ask_02+eps)) | **`depth_imb_10_m15`** = trailing 15-minute mean of `depth_imb_10`, all-Some window, gap-reset | Keeps an imbalance-*persistence* signal instead of a two-band slope that can no longer be computed pre-2026-01-15. |

Everything else in Amendment 1 is unchanged by this amendment: the venue
(Binance UM), the fee schedule and the fee fixed-point rule (VIP0 +
BNB-discount taker 4.50bps / maker 1.8bps on run 2, with the one allowed
re-run at the volume-mapped tier per that rule), the `binance-costs` +
`gate --binance-costs` cost path, all three horizons (15/30/60) every
run, the §5 eligibility conditions 2–5, the anchored-expanding fold
schedule (90-day train floor / 30-day test / 30-day step, `MIN_ROWS =
50,000`), the `--threshold-mult`/`--notional` no-retuning rule, and the
one-drop-one-rerun universe-selection rule (unanimous negative
`total_net_bps` across all three horizons, one exclude list, one rebuild,
one rerun, then the number is the number). None of that is touched by
substituting three feature definitions.

**No run-1 result beyond the eligibility failure informed this
substitution.** The fs-3 changes above are forced by data availability —
which three fs-2 features read the ±0.2% band — not by any P&L, Sharpe,
IC, or per-asset number gate run 1 produced. Gate run 1's per-horizon
PASS/FAIL/NO-TRADES results, its KAITO-concentration finding, and every
other diagnostic in `docs/scalper-gate-run-1.md` played no role in
choosing `depth_10_z_60` / `depth_10_log` / `depth_imb_10_m15` or their
definitions; those were chosen solely to preserve a dynamics, a level,
and a persistence signal at the band the archive actually has. This
statement is made explicitly, before code exists, so it can be checked
against the diff when fs-3 lands.

**Run 2 supersedes run 1 as the gate record.** Once a gate run is
completed under fs-3, its report (`docs/scalper-gate-run-2.md`) is the
gate record this protocol's PASS/FAIL determination is read from. Gate
run 1 stays on file exactly as written, unedited, and is not the gate
record — it was never eligible (Amendment 1 condition 1, and by
extension condition 3, since it ran fs-2) — but its diagnostics remain
available for comparison, same as any superseded document under this
protocol's amendment discipline.

**Frozen-data rule still applies.** No pull or backfill of any kind
between now and the fs-3 gate run — run 2 rebuilds its matrix from the
data already on disk under `data/`, exactly as the runbook's frozen-data
rule (§4, restated by plan 3d's global constraints) requires. Deepening
`data/perp` or `data/binance-micro` between this amendment and the run
would change the matrix's stride phase and warmup window and defeat the
comparison run 2 is meant to make against run 1.

**Fee fixed-point rule applies to run 2 unchanged.** Run 2 charges VIP0 +
BNB-discount taker (4.50bps) / maker (1.8bps) exactly as Amendment 1
specifies, then maps `overall.projected_30d_volume_usd` to a tier and, if
that tier differs from VIP0, runs exactly one re-run at that tier's fees
— the second run's verdict is the gate verdict. VIP1–8 remain unresolved
in `docs/binance-um-fee-table-2026-08.md` pending the user's
authenticated fetch of Binance's fee schedule; if the volume-mapped tier
falls in that unresolved range, the re-fetch happens before the one
allowed re-run, per Amendment 1's rule — no unverified number is
substituted.

## Amendment 3 (2026-08-16): ATR stop/target exits, pre-registered

**Why.** Gate run 2 (`docs/scalper-gate-run-2.md`) FAILED at all three horizons
under the fixed-time exit, while pooled rank IC was 0.26 / 0.18 / 0.11 —
ranking skill without profitable trades. Under a fixed-time exit the trade
collects the full noisy H-minute path; a stop/target pair pays for being right
about direction. User decision 2026-08-16: test an ATR-based stop/target with
1.2:1 reward:risk. This amendment fixes every parameter BEFORE run 3 exists.

**Exit rule (the only change vs Amendment 1/2).**
- ATR(14) on 1-minute bars, Wilder smoothing, true range against the prior
  close, computed in Rust from the same bars the features use. Value at the
  entry bar's close, in price units.
- Stop distance = **k · ATR(14)** with **k = 4**. Target distance = **1.2 ×
  stop** (R:R 1.2:1). Long: stop = entry − 4·ATR, target = entry + 4.8·ATR;
  short mirrored. Entry price = the signal bar's close (unchanged).
- Resolution on 1m bars, per bar after entry, in order: if the bar's low ≤ stop
  (long) / high ≥ stop (short) → exit at the stop price; else if the bar's high
  ≥ target (long) / low ≤ target (short) → exit at the target price. **A bar
  that touches both counts as a stop** (conservative; intrabar order is
  unknowable from bars). If neither is hit by the bar H minutes after entry →
  exit at that bar's close (the Amendment-1 time exit as fallback).
- Costs, entry rule (|pred| > 1.5 × round trip), fees, fold schedule, horizons,
  universe, drop rule and fee fixed-point: unchanged.

**Why k = 4 (measured, not tuned).** ATR(14) at 1m, last six months, median
across the 24 mapped assets ≈ 5–22 bps of price; the round-trip cost the gate
charges is 9–25 bps — i.e. cost ≈ 0.6–1.7 × ATR (BTC 1.73, ETH 1.27, SOL 1.17,
ZEC 0.57; table in the run-3 record). Net of a cost c ≈ 1.1 ATR: at k = 1 the
after-cost R:R is (1.2−1.1):(1+1.1) ≈ 0.05:1; k = 3 → 0.6:1; **k = 4 → 0.72:1
(break-even hit rate ≈ 58%)**; k = 5 → 0.8:1. k = 4 is the smallest value at
which the nominal 1.2:1 survives costs meaningfully while the stop stays inside
the H-minute window for typical volatility. Exactly one k and one R:R are
run; no scan. If run 3 FAILs at these values, the FAIL branch of Amendment 1
applies — no re-tuning of k or R:R.

**Provenance.** Run 3 supersedes run 2 as the gate record only if it is
eligible under the same conditions; run 2 stays on file as the fixed-exit
result. Data stays frozen (no pull between run 2 and run 3). fs-3 unchanged.

## Amendment 3b (2026-08-16): the cost model must not depend on the ±0.2% band either

**Why.** The audit of gate run 2 found that the pre-registered impact model
(`binance_costs.rs`, Amendment 1) prices a $5k clip against the day's median
**±0.2%** band depth — the same band Amendment 2 documents as absent from the
bookDepth archive before 2026-01-15. Every OOS day before that date is
therefore "thin → untradeable" by rule at every horizon: run 2's costed
trading window was 2026-01-15..2026-07-22 (~6 months, 4-5 folds), not the
24-month matrix span. Removing the band dependency from features (Amendment 2)
without removing it from costs left the eligibility problem in place on the
cost side. This amendment fixes that, pre-registered before run 3.

**Impact model (replaces Amendment 1's).** For a notional N against the day's
median ±1.0% band depth d10 = min(median bid_10, median ask_10):
`impact_bps = m · (N / d10) · 50`, i.e. a uniform-density walk within a
100-bps band, halved for average depth, times a multiplier **m = 2.3**;
`None` (thin, untradeable that day) if N > d10 or the band is absent. **Floor:**
on days where the ±0.2% band also exists, `impact_bps = max(new model,
Amendment-1 model)` — post-2026-01-15 costs are never lower than run 2
charged. Spread (p75 of minute estimates), fees, the 14-day absent-day
fallback, thin-day rule: unchanged.

**Why m = 2.3 (measured, not tuned).** On the 5,010 asset-days from
2026-01-15 where both bands exist, the ratio of the ±1%-band model at m = 1 to
the pre-registered ±0.2%-band model has p10/p25/p50/p75/p90 = 0.36 / 0.44 /
0.56 / 0.68 / 0.76 (per-asset medians 0.37 AAVE, KAITO … 0.74 BTC). m = 2.3 is
the smallest value at which the new model is at least as conservative as the
old on ≥ 75% of asset-days; the max-floor covers the remainder where the old
band exists. One m is run; no scan.

**Consequence for the record.** Run 3 (Amendment 3 exits + this cost model)
is the first run whose costed trading window can match the matrix span. Its
eligibility statement must report both spans (matrix; costed/tradeable OOS)
explicitly. Run 2 stays on file with its ~6-month costed window stated.

## Amendment 4 (2026-08-16): a look-ahead in the metrics join — fs-4

**What was found.** Gate run 3 (Amendment 3 exits + 3b costs) reported
Sharpe 15–18 at every horizon with fold Sharpes of 25–42 in 2024–25 and IC
≈ 0.30 at h15. That is not a trading result. Investigation: the fold models'
gain is dominated by `taker_ls_ratio` (~42%) and `oi_change_60` (~11%), both
from Binance's `metrics` archive (5-minute rows). Direct measurement on BTC
2025-03..05 (26,496 rows): the taker ratio stamped `create_time = T`
correlates **+0.365** with the return over **[T, T+5m)** and −0.013 with
[T+5m, T+10m). Binance's `create_time` marks the START of the 5-minute
aggregation window; the row at T summarizes flow through T+5m. `micro_join`
snapped metrics with `ts_s ≤ bar_ts`, handing the bar at T a row that
contains five minutes of its own future. Every fs-2/fs-3 model (gate runs
1–3) is contaminated by this; the run-2 audit's "IC implausibly high" flag was
this. Runs 1–3 stay on file as INVALID for signal purposes (their cost, exit
and eligibility machinery is unaffected and stands).

**Fix (fs-rust-scalper-4).** Feature definitions unchanged (all 38); the
metrics join becomes `ts_s + 300 ≤ bar_ts` (a 5-minute row is available only
after its window closes) with the same 600s staleness tolerance measured
from window close. bookDepth snapshots are point-in-time (a snapshot at T is
the book at T) and aggTrades minute-buckets at T summarize [T, T+60) — a bar
at open-time T whose features are computed at its close T+60 may use bucket
T; those joins are unchanged. Every micro source's timestamp semantics are
now recorded in `micro_join.rs`'s module doc with the measurement that
established them, and a regression test asserts the metrics offset.

**Consequence.** Full re-run: matrix rebuilt on the same frozen data, folds
re-fit, gates at all three horizons under Amendments 1–3b as amended here.
That run (gate run 4) is the first uncontaminated record. Fees, costs (3b),
exits (3), fold schedule, horizons, drop rule, fixed-point: unchanged.
Pre-registered before any fs-4 code exists.

## Research phase closed (2026-08-16)

Gate run 4 (`docs/scalper-gate-run-4.md`) is the record. It is the first
gate run built on an uncontaminated feature set (fs-4, Amendment 4's causal
metrics join), eligible under every condition in Amendments 1–4 without
qualification, and it **FAILS the gate at all three horizons** — h15
Sharpe 0.7567, h30 0.6869, h60 0.5127, all below the 2.0 bar. Per §5's
FAIL-branch text (restated, unedited, by every amendment through this one):
*"A FAIL — on either the first run or the one allowed re-run — means the
project stops or returns to feature research. Not: lower the 2.0 threshold.
Not: keep adding horizons until one clears. Not: shrink the notional until
costs look better. Not: re-run with a different fold schedule until a
favorable window turns up."* That branch is invoked here. This closes the
research phase under this feature set.

**Any continuation is a new pre-registered signal program, not a parameter
change.** Re-tuning `k`, R:R, the threshold-mult, the notional, the fold
schedule, or the gate threshold against gate run 4's numbers is exactly what
§5 and Amendment 1 rule out. A continuation means: a new
`FEATURE_SET_VERSION`, its features and any other changed definition
pre-registered as a dated amendment before a single number exists, then this
exact protocol run again from a fresh first run.

Three directions have been flagged by the engineer as candidates for such a
program. None is endorsed here, none has been tested, and this section
recommends none of them over the others or over stopping entirely:

- Tick-level order-flow features built from the raw aggTrades tape (the
  archives are already ingested per-minute; the raw trade tape itself is
  not currently stored).
- Maker-side execution economics — post-only entries, which would need a
  fill model that is not optimistic (queue-position aware), since the
  current simulator has none.
- An alternative label/target in place of fixed-horizon forward return.

Any of these would require its own amendment, its own feature-set version,
its own matrix rebuild, and its own fresh gate run — the protocol does not
distinguish a "small" continuation from a full new run.

## Amendment 5 (2026-08-18): Program 2 — tick order flow (fs-5) and maker entry, pre-registered

*(This section opens a NEW signal program under the terms the closure
section above sets: a new `FEATURE_SET_VERSION`, every changed definition
written here before a single number exists, then the protocol run again from
a fresh first run. It amends, and does not delete, §1–6 and Amendments 1–4.
Where it conflicts with them it governs; everything it does not name is
unchanged. At the moment this text is written no fs-5 code, no tape store,
no maker fill path and no gate-run-5 number exists.)*

**Why this program, and what did and did not inform it.** Gate run 4 is the
record and it FAILED. Its diagnostics are on file and two of them are the
reason this program exists — read as *where* the edge is lost, not as
numbers to tune against: (i) with minute-aggregated inputs the honest pooled
IC is ≈0.04 at h15 and ≈0 by h60, and (ii) 99.7% of predictions never
clear a taker round trip of ~11–46 bps, so the strategy hardly trades even
though it is cost-positive when it does. Both point at the same object the
current pipeline throws away: the raw Binance aggTrades tape.
`binance_micro.rs` downloads it daily and reduces it to one `FlowMinute` per
minute; the trades themselves are never stored. Program 2 keeps the tape and
uses it for two things at once — sub-minute order-flow features (signal
side) and a pessimistic maker fill model (cost side). No parameter below is
derived from any run-4 P&L, Sharpe, IC, per-asset or per-fold figure; each is
either an a-priori microstructure convention or a definitional consequence,
and this statement is made so it can be checked against the diff.

**Data availability, verified 2026-08-18.** `data.binance.vision/data/
futures/um/daily/aggTrades/<SYMBOL>/<SYMBOL>-aggTrades-<date>.zip` and
`.../trades/...` return HTTP 200 (BTCUSDT 2026-07-15: 13.6 MB and 22.0 MB
compressed); `.../bookTicker/...` returns 404 for UM futures. So this
program has the trade tape (price, quantity, `transact_time` in ms,
`is_buyer_maker`) and does NOT have a tick-level best bid/ask. Every
definition below is written against the trade tape only; nothing here
assumes book state between bookDepth snapshots.

### 5.1 The tape store (new; the only addition to the frozen data)

- New `scalper-data` subcommand `pull-binance-tape`: fetch the daily
  aggTrades archive for every mapped universe symbol and store it losslessly
  as `data/binance-micro/tape/<SYMBOL>/<YYYY-MM-DD>.parquet` with columns
  `ts_ms: i64` (`transact_time`), `price: f64`, `qty: f64`,
  `is_buyer_maker: bool`, in archive row order. Same k-prefix identity rule
  as every other puller (the universe file is the sole identity source).
- **Span: `--start 2024-08-01 --end 2026-08-15`**, exclusive end like every
  puller here — i.e. day files 2024-08-01 through **2026-08-14**, the last
  day the frozen `flow`/`book` stores hold and the exact archive window run
  4's `binance-costs` and `training-matrix` were built on. Not one day more:
  run 5's matrix span must equal run 4's so the two records differ only in
  what this amendment changes.
- **Every other store is frozen byte-for-byte as run 4 left it**: `data/perp`,
  `data/binance-micro/{book,flow,metrics,funding}`, `data/scalper-universe.
  json`, `data/binance-micro/costs-daily-3b.json`. No pull, backfill or
  universe refresh of any kind. The universe is run 4's 24 mapped assets.
- The pull must COMPLETE before the matrix is built. A per-symbol manifest
  (`data/binance-micro/tape/manifest.json`: days present, days the archive
  404'd, row counts) is written by the puller and reported in the run-5
  record. A missing tape day makes every fs-5 tick feature `None` for that
  asset-day, so `training-matrix`'s all-Some rule drops those rows — the
  same way a missing bookDepth day already drops rows; no imputation.

### 5.2 fs-5: twelve tick features appended, the 38 fs-4 features untouched

`FEATURE_SET_VERSION = "fs-rust-scalper-5"`, 50 features. Indices 0–37 are
byte-for-byte fs-4 (same names, formulas, joins — Amendment 4's causal
metrics join included). Indices 38–49 are new and are computed by
`features-scalper` from a per-bar `TapeWindow` the CALLER assembles from
the tape store, in the same pattern as `MicroMinute`: the feature crate
never opens a tape file.

Conventions, binding for all twelve: a bar with open time `T` is evaluated at
its close `C = T + 60 s`. A window of length `W` is the half-open interval
`[C − W, C)` in `ts_ms`, i.e. **strictly before the close** — no trade at or
after `C` is visible. `buy` = taker buy = `is_buyer_maker == false`; `sell`
mirrored. Notional = `price × qty`. `eps = 1e-12`. Bar-gap reset (>120 s
between consecutive bars) resets the 60-minute baselines exactly as every
other rolling window in the crate. As everywhere in the crate: `Some` only if
finite; any `None` input → `None` output.

| # | name | definition |
|---|------|------------|
| 38 | `tk_imb_10s` | `(buy_notional − sell_notional) / (buy_notional + sell_notional)` over W = 10 s; `None` if no trades in the window |
| 39 | `tk_imb_30s` | same, W = 30 s |
| 40 | `tk_imb_5m` | same, W = 300 s |
| 41 | `tk_large_imb_5m` | same imbalance over W = 300 s restricted to trades whose notional is in the top decile of that window's own trades (≥ the window's p90 notional, `costs::percentile`); `None` if the window has < 10 trades |
| 42 | `tk_run` | signed length of the run of consecutive same-side trades ending at the last trade before `C` (positive = buys), clipped to ±50, counted within W = 300 s; `None` if no trades in the window |
| 43 | `tk_ret_10s` | `1e4 · ln(p_last / p_ref)`, `p_last` = price of the last trade before `C`, `p_ref` = price of the last trade before `C − 10 s`; `None` if either is missing within W = 300 s |
| 44 | `tk_ret_30s` | same with `C − 30 s` |
| 45 | `tk_vwap_dev_30s` | `1e4 · (p_last − vwap_30s) / vwap_30s`, notional-weighted vwap over W = 30 s; `None` if no trades |
| 46 | `tk_intensity_10s` | `ln((n_10s + 1) / (n_60m / 360 + 1))`: trade count in the last 10 s against the trailing-60-minute count scaled to 10 s; `None` unless the 60-minute baseline window is full (60 minutes of tape coverage without a bar gap) |
| 47 | `tk_notional_ratio_30s` | `ln((notional_30s + 1) / (notional_60m / 120 + 1))`, same baseline discipline as 46 |
| 48 | `tk_impact_5m` | Kyle-λ proxy: over the 30 consecutive 10-second buckets in `[C − 300, C)`, `x_i` = signed notional (buy − sell) of bucket `i`, `y_i` = `1e4 · ln(last_i / last_{i−1})` (last trade price of the bucket; a bucket with no trades inherits the prior last, giving `y_i = 0`, `x_i = 0`); feature = `cov(x, y) / var(x)` × 1e6 (bps per $1M signed); `None` if fewer than 10 of the 30 buckets contain a trade or `var(x) = 0` |
| 49 | `tk_size_med_5m_log` | `ln(median trade notional + 1)` over W = 300 s; `None` if no trades |

Why these twelve and these windows: 10 s / 30 s / 5 min are the standard
short/medium/long microstructure horizons; imbalance, large-print
imbalance, aggressor run, sub-minute return, vwap deviation, intensity,
notional surprise, price impact and trade size are the textbook order-flow
family. Exactly this set is run; no feature is added, dropped or re-windowed
after a number exists. fs-4 model artifacts are refused against an fs-5
matrix and vice versa (the existing version cross-check).

Label unchanged: `fwd_bps` = `1e4 · ln(close_{t+H} / close_t)` at H = 15,
30, 60 from the signal bar's close. This program deliberately changes the
inputs and the execution, not the target — the third direction the closure
names (a new label) is NOT part of this program.

### 5.3 Maker entry: a pessimistic trade-through fill model

The gate gains an entry mode `--entry maker` (default remains `taker`,
Amendments 1–4 unchanged). Run 5 is gated under `--entry maker --exit atr`.

- **Signal and threshold**: unchanged in form. At the signal bar's close `C`
  a prediction `pred_bps` from the fold model; long if `pred_bps > 1.5 ×
  round_trip`, short if `< −1.5 × round_trip`, where `round_trip` is now the
  maker-entry round trip of §5.4. `--threshold-mult 1.5` is not changed.
- **Order**: a post-only limit at `P = close` of the signal bar (long: bid at
  `P`; short: ask at `P`), resting from `C + 1000 ms` (a fixed one-second
  placement latency — an order cannot be on the book before it could
  physically have been sent) until `C + 60 s` (the next bar's close). One
  order per asset at a time; a resting order or an open position blocks new
  signals for that asset exactly as `flat_after` does today.
- **Fill rule (the conservative core)**: the order is filled iff a tape trade
  with `ts_ms ∈ [C + 1000, C + 60 000]` prints at a price **strictly better
  for us than P** — long: `price < P`; short: `price > P`. A trade AT `P`
  never fills us (we do not claim queue priority; a print at our level is
  assumed to have gone to orders ahead of us). Fill price = `P`, fill time =
  that trade's `ts_ms`. If no such trade prints, the order is cancelled and
  no trade occurs — a **miss**, counted, not skipped silently.
- Why strict trade-through: without a tick-level book we cannot know queue
  position, so the only fill claim that cannot be optimistic is "the market
  traded through our price". This is *more* pessimistic than reality on
  fills and *exactly as* pessimistic as reality on adverse selection: we are
  filled precisely when price has just moved against the signal. That is the
  economics being tested.
- **Exit**: Amendment 3 unchanged in every parameter — ATR(14) Wilder on 1m
  bars, `k = 4`, R:R 1.2, stop-wins-ties, time-exit fallback — with three
  definitional consequences of a maker entry: (a) stop/target distances are
  measured from the fill price `P` using ATR at the **signal** bar's close
  (known at order placement, hence causal); (b) the exit walk starts at the
  bar in which the fill occurred (open `C`), and that bar itself is checked
  first, stop-wins-ties, since a fill and a stop can share a bar; (c) the
  time exit is the close of the bar at `C + H·60 s` — the horizon runs from
  the fill bar's open, i.e. one bar later than the taker path's, and it is
  the same `[C, C+H]` window the label is measured over. All exits are
  executed as **taker** (no maker fill claimed on the way out).
- *Clarification (2026-08-18, before any fill code exists):* the fill window
  is taken half-open, `[C + 1000, C + 60 000)` ms, so the fill bar is always
  the bar with open `C` (a print at exactly `C + 60 s` belongs to the next
  minute and is not a fill — one ms fewer, never more). Inside the fill bar
  the walk does not use OHLC: intrabar order IS known there from the tape,
  so the fill bar is resolved on the tape from the fill trade onward, in
  archive order — the first later print at or through the stop level exits
  at the stop, at or through the target exits at the target (a print
  before the fill can trigger nothing: we held no position yet). From the
  next bar on, `exits::resolve_exit` walks OHLC exactly as Amendment 3
  (stop-wins-ties, gapped-stop-at-open, time exit at the bar at
  `C + H·60 s`). This is neither more nor less optimistic than the OHLC
  fill-bar check §5.3(b) sketched — it is the exact version of it — and it
  is stated here so the diff can be checked against it.
- Every fold's report gains `n_signals`, `n_fills`, `fill_rate`, `n_misses`,
  and mean `fill_delay_ms`, overall and per asset. Nothing about the gate
  criterion changes: annualized Sharpe > 2.0 on daily net-of-cost returns,
  out-of-sample folds only, all three horizons every run.

### 5.4 Costs under maker entry

`round_trip_bps = fee_maker + fee_taker + spread_bps_p75 / 2 + impact_bps`
per asset-day, from the same frozen `costs-daily-3b.json` (Amendment 3b
impact model, unchanged): one maker fee in, one taker fee out, one crossing
of half the day's p75 spread on the taker exit, one side of impact on the
taker exit. The maker entry pays no spread and no impact by construction —
its price is `P` and its cost is the adverse selection the fill rule
imposes. Thin-day (`impact_bps = None`) and 14-day-lookback rules unchanged.
Fees: run 5 charges **VIP0 + BNB: maker 1.80 bps, taker 4.50 bps**
(`docs/binance-um-fee-table-2026-08.md`, verified rows), and Amendment 1's
fee fixed-point rule applies unchanged (map `projected_30d_volume_usd` to a
tier; at most one re-run; the second run's verdict is the verdict; VIP1–8
still require the user's authenticated fetch before any re-run at those
tiers). Funding: Amendment 1's < 0.3 bps/trade bound still holds (holds
are ≤ 61 minutes). Notional 5,000 unchanged.

### 5.5 Everything else is unchanged

Venue (Binance UM). Universe (run 4's 24 mapped assets, frozen). Matrix
span 2024-08-01..2026-08-15, `--stride 5`. Fold schedule (anchored
expanding, 90-day train floor / 30-day test / 30-day step, `MIN_ROWS =
50,000`). Horizons 15/30/60, all three every run. Gate = OOS annualized
Sharpe > 2.0 on daily net returns. The one-drop-one-rerun universe rule.
The fee fixed-point rule. The FAIL branch: **if run 5 FAILs at these
definitions, no window, threshold, latency, rest time, fill rule, exit
parameter, feature or cost term is revisited against its numbers** — the
project stops or a further *new* program is pre-registered on its own
terms, exactly as this one is.

### 5.6 What the diff must show, in this order

1. `pull-binance-tape` + parquet tape store + manifest (with a test that a
   stored day round-trips the archive rows losslessly and that a 404 day is
   recorded, not fabricated). The pull itself for the §5.1 span may run as
   soon as this lands — it fetches a public archive and produces no
   feature, fill or gate number — and must be complete before step 4.
2. `TapeWindow` assembly in `scalper-data` (window bounds strictly `< C`,
   tested with a trade at exactly `C` being excluded) and the twelve fs-5
   features in `features-scalper` with golden values on a hand-built tape
   (including the `None` conditions in the table above and the ±50 clip).
3. `--entry maker` in `gate.rs`: the fill rule (tests: a print AT `P` does
   not fill; a print strictly through `P` fills at `P` at that trade's
   `ts_ms`; a print at `C + 999 ms` does not fill; no print → miss), the
   fill-bar exit walk, the §5.4 round trip, the new report fields.
4. Only then: `training-matrix` (fs-5),
   `walk_forward_scalper.py` × 3 horizons, `gate --entry maker --exit atr`
   × 3, and `docs/scalper-gate-run-5.md` written from the reports.

Run 5 becomes the gate record for Program 2 only if eligible under
Amendment 1's conditions (≥18-month matrix span; full mapped universe;
fs-5 end to end; `--binance-costs`; all three horizons). Runs 1–4 stay on
file exactly as written; run 4 remains the record for Program 1.

## Program 2 closed (2026-08-18)

Gate run 5 (`docs/scalper-gate-run-5.md`) is the record for Program 2. It
is eligible under every condition in Amendments 1–5, and it **FAILS the gate
at all three horizons** — first run (24 assets) h15 Sharpe 0.3649, h30
0.1883, h60 0.4321; the §5 one-allowed-drop re-run (CRV, FARTCOIN, LIT, XMR
excluded — the first time any symbol qualified — 20 assets) h15 0.1508, h30
0.1331, h60 −0.2510, and the second run's verdict is the verdict. Fee tier
provisional as in runs 2 and 4 (volume inside the disputed VIP1 band); a
lower tier cannot plausibly close a 1.6–2.3-point gap and no unverified
number is tested.

What the program established, in the record's own words: the twelve tick
order-flow features add no measurable ranking skill over fs-4 (pooled IC
unchanged at h15, lower at h30/h60; 3–8% of model gain, none of it from the
imbalance family), and the maker entry, priced by a strict trade-through
fill rule, fills ~90% of orders adversely — net per trade falls from run 4's
+30 bps to +2–6 bps although the round trip is ~40% cheaper. The binding
constraint of Program 1 — prediction magnitude — is the binding constraint
of Program 2.

Per Amendment 5 §5.5 the FAIL branch is invoked: no window, threshold,
latency, rest time, fill rule, exit parameter, feature or cost term is
revisited against these numbers. Program 2 is closed. Any continuation is a
further new pre-registered program on its own terms — or stopping. Of the
three directions the Program 1 closure named, two (tick order flow, maker
economics) have now been run and failed; the third (a different label than
fixed-horizon return) has not been tested and this section, like the last,
recommends nothing.

## Amendment 6 (2026-08-18): Program 3 — cross-asset tick context (fs-6), taker execution, pre-registered

*(A NEW signal program under the closure sections' own terms. It amends,
and does not delete, §1–6 and Amendments 1–5; where it conflicts it
governs; everything it does not name is unchanged. At the moment this text
is written no fs-6 code exists and no number involving a BTC tick feature
against any other asset's forward return has been computed or looked at.)*

**Why this and not something else.** Two facts from the record motivate
this program, read as *where* to look, not as numbers to tune against:
(i) in both clean runs (4 and 5) the single largest-gain feature at h15 is
`btc_ret_5` — BTC's own 5-minute return, joined at 1-minute resolution —
i.e. what little the models find, they find largely in cross-asset
context; (ii) Program 2's twelve tick features were all *own-asset* and
added no IC. The tape allows the one obvious combination neither program
tried: BTC's order flow and sub-minute return, at tick resolution, as
context for every other asset's bar. Cross-asset lead-lag from the largest
contract to the rest is a documented microstructure effect on centralized
crypto venues; whether it survives at 15–60-minute horizons net of costs is
exactly what the gate is for. The other closure-named direction (a
different label) is deliberately NOT part of this program: at h15 — the
horizon where the signal is strongest — 80% of run-4 exits and 79% of
run-5 exits are time exits, so the fixed-horizon label already is the
realized outcome there; changing it cannot be what is missing.

**Execution: taker, exactly Amendments 1–4 (run 4's setup).** Program 2
established that under a fill model that is not optimistic, maker entry
fills adversely and lowers net per trade; this program does not re-run it.
`gate --exit atr` (Amendment 3 exits, Amendment 3b costs, fees 4.50/1.80
VIP0+BNB, `--threshold-mult 1.5`, `--notional 5000`), `--entry taker` (the
default). The comparison this program makes is against run 4 (fs-4, taker):
same folds, same window, same execution — only the feature set differs.

### 6.1 fs-6: six BTC-context tick features appended, the 50 fs-5 features untouched

`FEATURE_SET_VERSION = "fs-rust-scalper-6"`, 56 features. Indices 0–49 are
byte-for-byte fs-5 (and 0–37 fs-4). Indices 50–55 are computed by
`features-scalper` from a second `TapeSource` the caller serves for BTC
(`BTCUSDT`'s tape), evaluated at the SAME close `C` as the asset's own bar,
with the same window convention (`[C − W, C)`, strictly before the close),
the same coverage discipline (a BTC minute that is `None`, or a skipped
BTC minute, breaks BTC coverage and BTC's deques restart), and the same
`Some`-only-if-finite rule. Definitions reuse Amendment 5 §5.2's table
applied to BTC's tape:

| # | name | definition |
|---|------|------------|
| 50 | `btc_tk_ret_10s` | Amendment 5 row 43 on BTC's tape at `C` |
| 51 | `btc_tk_ret_30s` | row 44 on BTC's tape |
| 52 | `btc_tk_imb_30s` | row 39 on BTC's tape |
| 53 | `btc_tk_imb_5m` | row 40 on BTC's tape |
| 54 | `btc_tk_intensity_10s` | row 46 on BTC's tape (BTC's own 60-minute baseline) |
| 55 | `rel_tk_ret_30s` | `tk_ret_30s − btc_tk_ret_30s` (own index 44 minus index 51); `None` if either is `None` |

For BTC itself the caller passes BTC's tape as both sources, so 50–54
equal the own-asset features and 55 is `0.0` — the same no-special-casing
convention `btc_ret_5` / `rel_ret_5` already use. Exactly these six are
run; no feature is added, dropped or re-windowed after a number exists.
Label unchanged (`fwd_bps` at H from the signal bar's close). fs-5 model
artifacts are refused against an fs-6 matrix and vice versa.

### 6.2 Everything else is unchanged

Data: byte-frozen as run 5 left it — perp, book, flow, metrics, funding,
universe (run 4's 24 mapped assets, the full universe; run 5's drop list
does not carry over — §5's drop rule is applied afresh from this program's
own first run), `costs-daily-3b.json`, and the tape (§5.1 span; no pull of
any kind). Matrix span `2024-08-01..2026-08-15`, `--stride 5`. Fold
schedule, `MIN_ROWS`, horizons 15/30/60 all every run, gate = OOS
annualized Sharpe > 2.0 on daily net returns, one-drop-one-rerun rule, fee
fixed-point rule, the FAIL branch: **if run 6 FAILs at these definitions,
no window, feature, exit, cost or threshold term is revisited against its
numbers.**

### 6.3 What the diff must show, in this order

1. `features-scalper`: `compute` takes a BTC `TapeSource` alongside the
   asset's; six features per §6.1 with golden values on a hand-built pair
   of tapes (including: BTC minute `None` → 50–55 `None` while 38–49 stay
   as they were; BTC-as-own → 55 is `0.0`; a BTC trade at exactly `C` is
   not visible), and the fs-5 parity assertion (fs-6 with `NoTape` for BTC
   reproduces fs-5's 50 values with six `None`s appended).
2. `training-matrix`: serve BTC's tape from the same `--tape-root` via a
   second `TapeCursor` keyed on the universe file's BTC symbol; per-asset
   None counts continue to print.
3. Only then: `training-matrix` (fs-6) → `walk_forward_scalper.py` × 3 →
   `gate --exit atr` × 3 (taker) → `docs/scalper-gate-run-6.md`, all three
   horizons reported, drop rule applied afresh if any symbol qualifies.

## Program 3 closed (2026-08-18)

Gate run 6 (`docs/scalper-gate-run-6.md`) is the record for Program 3. It
is eligible under every condition in Amendments 1–6 and it **FAILS the gate
at all three horizons** — first run (24 assets, taker, fs-6) h15 0.6007,
h30 0.7272, h60 0.6718; the §5 one-allowed-drop re-run (CRV, LIT, WLD
excluded; 21 assets) h15 0.6671, h30 0.4368, h60 0.5586, and the second
run's verdict is the verdict. Fee tier provisional as in runs 2, 4 and 5.

What the program established: the six BTC tick-context features are used
heavily by the models (31% / 24% / 16% of gain, four of the top six
features at h15) and add no out-of-sample ranking skill — pooled IC
0.026 / 0.020 / 0.021 against run 4's 0.037 / 0.022 / 0.015, Sharpe at the
same level as run 4 on the same folds and execution.

Per Amendment 6 §6.2 the FAIL branch is invoked; nothing is revisited.
Program 3 is closed. Three programs — six gate runs, four clean — have now
been run under pre-registration on the same 24-month Binance UM record:
minute-bar features (fs-1..4), own-asset tick order flow, maker execution,
cross-asset tick context. Every clean run lands at pooled IC 0.02–0.04 and
Sharpe 0.1–0.8; nothing clears 2.0. This section recommends nothing; the
one closure-named direction never run (a different label/target) remains
untested, and the honest prior from the record is that it addresses the
wrong constraint (at h15 ~80% of exits are time exits, so the label already
is the outcome).


## Research round (2026-08-19)

`docs/scalper-research-round-2026-08.md` surveys the 2021–2026 literature on
short-horizon crypto predictability, market-making/execution economics,
industry practice, and medium-frequency evidence, and reads gate runs 4–6
against it. Its conclusion is recorded here for the protocol: the outside
evidence predicts exactly what the six runs found (IC 0.02–0.04, net Sharpe
< 1 at 15–60 minutes from public data, maker execution adversely selected),
no credible source supports a Sharpe > 2 signal at this horizon for this
participant, and the professionals' documented P&L comes from speed, queue,
rebates, inventory and flow — not minute-scale prediction. One data
correction: Binance did publish a UM `bookTicker` archive for 2023-05-16
through 2024-03-31 (verified 2026-08-19); Amendment 5's statement holds for
the 24-month record. The document recommends closing the 1-minute scalper
line as a direction and, if a systematic crypto book is still the goal,
pre-registering a separate medium-frequency project (daily-bar trend on
liquid perps + funding carry sleeve, expected net Sharpe ~1). No amendment
is made by this section; nothing here is a number against any gate.
