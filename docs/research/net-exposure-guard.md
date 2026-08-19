# Enforcing `max_net_exposure`

Changed 2026-08-19. The limit had been left unset since Phase 1 ("needs shorts
to be meaningful"); shorts arrived with the risk-adjusted constructor, and the
Phase 2 paper book was measured at −13.2% net against a 0.0% target with
nothing enforcing anything. It is now enforced in two places and measured in a
third.

## What was wrong, and it is two things

1. **The plan's target was never checked.** `risk()` had a `max_net_exposure`
   check, gated on the limit being set; the config left it `""`. Setting it to
   a number is a risk-appetite decision — `config/default.toml` now says
   `0.10` and why.

2. **The target is not the book.** The constructor keeps the target at zero
   net by design (each side gets exactly half the gross), so a target check
   alone would almost never fire. The book that exists after a rebalance is a
   different number, because the turnover budget lets every reduction through
   (correctly — `docs/research/turnover-cap-exemption.md`) and defers
   additions. On a reversal day one side shrinks while the other side's
   rebuild waits, and the book goes net without any plan asking for it.

## What changed

- `config/default.toml`: `max_net_exposure = "0.10"` — |Σ weights| ≤ 10% of
  NAV. Tightening costs turnover on reversal days; loosening is a decision to
  carry beta the model never asked for.
- `crypto-portfolio::diff`: after the turnover budget has selected orders, the
  **projected** book (current weights + selected drifts) is netted. If |net|
  exceeds the limit, deferred additions on the *light* side — and only that
  side — are released past the budget, largest drift first, until the
  projected book is inside the band or none remain. Restoring neutrality is
  risk reduction, so it gets the same treatment as a Reduce: it does not need
  the cap's permission. Every release is disclosed (`skipped` line and a
  `TurnoverCapped` warning with net before/after); a projected book that
  cannot be brought inside is disclosed as a warning rather than hidden in a
  rejected plan, which would leave the book exactly where it is.
- `backtest::Step` records realized post-fill `net_exposure` per rebalance,
  so a replay can show the distribution the limit is about, not just Sharpe.

Tests: `net_exposure_guard_releases_deferred_additions_on_the_light_side_only`
(a reversal day: exits free, additions deferred, book projected −0.40 net;
the guard releases the two long entries and never the short one; released
weight does not consume the allowance) and
`net_exposure_guard_is_idle_inside_the_band`.

## What the folds say

Nine folds from `var/research/wf-rank-2020`, replayed with the same per-fold
models via `bin/replay-folds.sh`, funding charged, 1× slippage, the same
binary for both runs. Only `max_net_exposure` differs (`""` vs `"0.10"`).

| | unset | 0.10 |
|---|---|---|
| mean Sharpe | 2.212 | 2.206 |
| compounded | +1450% | +1437% |
| folds positive | 9/9 | 9/9 |
| plans rejected | 0 | 0 |
| turnover / rebalance | unchanged to 3 dp on every fold | |
| rebalances where the guard released anything | — | 9 of 2,142 |
| rebalances with realized \|net\| > 0.10 | 39 | 30 |

Per-fold Sharpe delta: `−0.03 0.00 −0.02 0.00 −0.00 0.00 0.00 −0.00 −0.00`.
`crypto-portfolio compare` (block bootstrap, 2,133 steps, 20-day blocks):
**observed delta −0.006, 90% interval [−0.013, −0.001], variant ahead in 3% of
resamples.** The interval excludes zero: the guard has a real cost, and it is
six thousandths of a Sharpe — the turnover it buys neutrality with on nine
days. As with the turnover exemption, the argument holds whatever the
backtest says; the backtest says the argument is nearly free.

## What the folds also say, and this is the finding

The guard fixes what a rebalance can fix. After every plan in every fold the
*projected* book was inside 0.10. The 30 rebalances where the *realized* book
was still outside are not deferral — on those days the deferred weight was
0.01–0.06 — they are mark-to-market drift after the plan, and 17 of them are
one episode: **fold 3, 12–31 May 2022.** The book was short LUNA and UST at
about −4.5% each on 2022-05-13. Their prices collapsed by ~99.9%: the short
side evaporated by mark, so the book read +0.11 to +0.19 net long for two
weeks with a neutral target every day; then on 2022-05-31 the LUNA short —
re-sized daily at a collapsed price, i.e. an enormous quantity — showed as
**−45% of NAV** after LUNC's bounce, and the book read −0.295 net. Next-day
|net| after a breach day has median 0.12: the daily cycle corrects it, but
not within the day, and no rebalance-time rule can.

That is a different gap from the one this change closes, and it is recorded
here rather than fixed: **a short in a token whose price has collapsed is a
position whose intraday move can be 10× its weight.** The eligibility screen
(`universe-rank`, point-in-time listing) kept LUNA/UST tradeable because the
venue did; the participation cap sizes against volume, which was enormous;
`max_position` caps the *planned* weight, not what a 300% bounce does to a
short. Candidates, none decided here: a hard per-position mark-to-market
stop, a "price fell > X% over N days → ineligible" screen, or sizing shorts
against a volatility floor. This is a Phase 3 prerequisite in the sense the
capital plan uses the word — it needs its own measurement — and it belongs
next to the impact-coefficient item, not silently under this one.

## Ship

Same as any execution change: build, `cargo test -p crypto-portfolio`
(57 pass), push, pin the cluster to the commit. The paper book's own record
(Postgres `nav_snapshots.net_exposure`) is where the −13.2% was measured; the
first cycles after this ships should show `net_exposure` staying inside
±0.10 at plan time and any guard release in the plan warnings.
