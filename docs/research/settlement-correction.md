# Settling positions whose asset stops trading — a correction to the record

Changed 2026-08-19 in the backtest (`backtest::settle_missing`). Found while
enforcing `max_net_exposure` (`docs/research/net-exposure-guard.md`): the
fold-3 book read −45% of NAV in LUNA on 2022-05-31.

## What was wrong

The daily store keys assets by ticker. When a venue delists a contract and a
new token later reuses the ticker, or a token is redenominated in place,
the store stitches two identities under one key: LUNA 2022-05-13 close
$0.00005 → 2022-05-31 open $1.00 (Terra 2.0, 20,000×); COCOS 1000×; QUICK
and SUN 1/1000; BNX 1/100; BTCST and STRAX 1/10. Every gap of 2+ days among
659 assets since 2020-09 is a venue event of this kind — there are no
transient holes in the store.

The backtest kept a position whose asset had no bar as a **ghost**: marked
at zero (`unwrap_or_default`), then re-marked at whatever the ticker meant
next. For a short — and every affected position on the record was a short —
"marked at zero" means the liability vanishes: a phantom gain equal to the
short's notional, booked on the day the ticker disappears. Where the ticker
never returns (LEND→AAVE, NPXS→PUNDIX, MATIC→POL, FTM→S, HNT, TON) the gain
stays in the record forever; where it returns (LUNA) it turns into a
phantom −45%.

## What changed

On the first decision day an asset has no bar, every position in it is
settled at its last known price — what the venue does — the cash leg is
booked, the position is gone, and the run's disclosures name it. The reborn
ticker is a new identity as far as the feature windows are concerned
already (they poison across the gap), so nothing re-enters it until it has
history. Live, the venue settles; this only changes what the backtest
believes it earned.

## What the folds say

Nine folds, same models, same binary, only the settlement rule differs.

| | ghost | settled |
|---|---|---|
| mean Sharpe | 2.212 | **2.113** |
| compounded | +1450% | **+1189%** |
| folds positive | 9/9 | 9/9 |
| bootstrap Δ | | −0.10, 90% interval [−0.21, +0.005], variant ahead in 6% |

Per-fold Δ: `−0.07 0 −0.04 −0.24 0 0 −0.47 0 −0.07`. Positions settled:
fold 1 LEND (2020-10-13), NPXS (2021-04-06); fold 3 LUNA and UST
(2022-05-14); fold 4 HNT (2022-10-15); fold 7 MATIC (2024-09-11), FTM
(2025-01-14), BNX (2025-03-19); fold 9 TON (2026-07-01). All shorts.

**The frozen record is overstated by ~0.10 Sharpe and ~260 points of
compounded return by phantom short gains in renamed tickers.** The corrected
walk-forward figure with funding and the turnover exemption is **2.11**
(interval vs the old 2.21 touches zero at its edge; the direction is not in
doubt because the mechanism is not statistical). Nothing about the signal
changed; the expected live Sharpe written down in advance (1.0–1.3) is
unaffected and, if anything, now has a slightly smaller backtest behind it.

## Status of the "collapsed-asset short" item

Resolved for the backtest by this change. Live, the venue settles delisted
contracts itself. What remains open from the same investigation is the
related but different concentration item: a side that thins can put a
single name at the 0.25 `max_position` cap (E's DEXE day,
`docs/research/absolute-label-unbalanced.md`); A's replay never binds it,
but it is the next risk item to measure.
