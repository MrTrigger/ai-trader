# Lighter strategy improvement

## Reopened objective, September 8

The earlier numerical pass is not completion of the user's request for a good
strategy. Require full-period net Sharpe > 1.5 and return above BTC, then seek
a material return improvement over R6 with stronger later-period evidence.
Report rally participation, each chronological half, stress and all failed
trials. Do not promote a reused-date fit as independent validation.

- [x] Read workflow principles and preserve R6/R7 controls.
- [x] Diagnose structural market-direction and holding-period limitations.
- [x] Separate daily valuation/funding from multi-day rebalance schedules;
      verify non-trading days retain price risk and funding.
- [x] Test a small declared batch of slower holding and absolute-return
      models, with explicit training cutoff and unchanged execution assumptions.
- [x] Evaluate qualified improvements against BTC, both halves, stress and
      block bootstrap. Reject marginal or inconsistent gains.
- [x] Extend temporal validation or retraining experiments if frozen model
      decay remains. Preserve chronological training and label embargoes.
- [ ] Independently review evidence and prepare only a verified paper candidate.

Target: daily net Sharpe above 1.5 on the native November 27, 2025 through July 30, 2026 replay. These dates have already been examined and remain development data. Record every trial; report chronological subperiods, execution stress and block-bootstrap uncertainty before accepting a candidate. Do not relabel reused dates as a fresh holdout.

- [x] Diagnose missing fills and historical training cohort mismatch.
- [x] Round 1: capacity-balanced targets at existing 1% hourly participation, weekly rebalancing, venue-cohort model with external labels, venue-cohort model with native next-open labels, and native-label model plus capacity-balanced targets.
- [x] Verify implementation and all causal input boundaries.
- [x] Compare fixed candidates against the unchanged native control.
- [x] Follow-up rounds found external-cohort model plus20%resize deadband at1.7869 under daily-open proxies.
- [x] Verify daily-proxy candidate, chronologicalparts, bootstrap and nativeprice benchmarks. Exact rerun matches all246financialsteps.
- [x] Correct measured-hour execution after hourlyaudit showed26fills larger than entireobservedhour. Re-evaluate unchangedcandidate under observedvolume cap, hourlyVWAP and01hfunding/valuation.
- [x] Finish fill-by-fill audit and record the measured-hour target result. All4,832selectedfills pass nativeVWAP, observedcapacity,01hNAV andcostreconstruction.

All round-1 candidates retain $100,000 starting capital, 2.2 bps half-spread, zero commission, impact coefficient 0.1 and 1% participation. Capacity balance caps position weights at prior completed native ADV / 24 times 1% / NAV, redistributes within each side and scales the larger side down to the smaller. It must not consume current-day volume. Weekly rebalance changes only rebalance_every to 7. Both models use the existing fixed training recipe and a pre-evaluation cutoff. Training omissions and label availability are recorded by the matrix generator.

Root owns planner and replay integration. Isolated workers own the training generator, public hourly downloader and independent diagnostics/review. No policy deployment, credential use or orders.

Weekly rebalancing was deferred before measurement because the suite correctly
requires daily valuation. Three executor-guard attempts failed closed and were
fixed with preserved Decimal quantities and prior-only previews. Guard is absent
from the selected policy and incompatible with the one-hour execution window.

R6 measured-hour result: original model Sharpe-1.0111, venue-cohort external-label
model2.5278, same model withdeadband2.2206. Highestcentralpolicy earns9.8899%
with4.1227%DD. Later123days areeffectivelyflat; stressSharpe-0.0329 and wide
bootstrapinterval prohibit a future-performanceclaim. All137tests pass and
independentreview verifies costs, timing, funding, native data and provenance.

Continuation through R16: monthly refits, slower trading, four-hour execution,
BTC direction, executable turnover, earlier-regime checks and consensus all
failed the combined quality requirement. Native BTC/ETH coverage now spans
April3,2025–July31,2026 exclusive, allowing483continuous daily decisions.
R15 BTC momentum returns36.64%,Sharpe1.1002; BTC/ETH35.64%,Sharpe1.0917.
R16 tests existing conviction-preserving sizing because the single-asset
vol-parity denominator cancels trend magnitude. A fixed ranker/momentum
target blend is being implemented independently, then will receive one
shared account risk check, order plan, execution cost and funding path.
All reused dates remain development data. The goal remains open.

R22 failed locked August 1 through September 7 confirmation: central -10.72% versus BTC +23.83%. Exact original 246-day prefix and independent 285-day accounting audits pass. No candidate is accepted. All 285 dates are now development data.

- [x] Preserve failed lock, outcomes and independent audit.
- [x] R23: fixed positive BTC monthly trend selects BTC long; otherwise original ranker.
- [x] R24: fixed ranker with four raw completed BTC market context features.
- [x] Compare all 285 daily returns, halves, monthly returns, BTC and stressed execution; retain failures.
- [ ] Independent code review and candidate accounting before any promotion.

R23, R24, R25 and R27 fail the joint target. R26 absolute daily direction is promising at16.09% return/1.3211Sharpe with8.60% stress return and bothhalvespositive, but remains below the minimum. R28 compares daily cost-aware unbalanced allocation and per-risk selection with a matched volatility-sizing control. FrozenR26model, no newfit. R26 rawcapacitydiagnosis is pending. All dates remain development; no strategy accepted.

R28 cost-aware sizing, R29 four-hour execution and R30 additive targets all fail.
R26 capacity audit passes all570 plan IDs and daily ledgers; only32.64%requested
notional fills at100k. R31 native01h labels preserve exact6486rows/241dates/45assets
and71features;6485labels change. Strict reference-pinned parity passes before
replay. R31 and R32 fixedfive-seed averages are running. A separate R33capital
sensitivity diagnostic preserves100k as the acceptance capital. No candidate
accepted; continue with measured hypotheses and retain every failed trial.

R32native5seed pointcandidate:18.71%/1.5070, stress11.14%/.9560, later7.44%/2.0001
versusBTC6.97%. Independentfinancialaudit and exact1500tree refit pass.
R34costfloor,R35slowfeatures,R36doublebudget,R37Huberall fail to improve jointly.
R38fixedBTCETH/BTC execution subsets are declared; preserve full ranking cohort.
Event concentration and crossvenue pricechecks pending. Freeze candidate only
with limitations visible; no exclusions, newliveorders or deployment.

R38 BTC/ETH routing: central1.5516/26.51%, later-half Sh1.0105 (fails), stress
.5113/6.83%, turnover28.55% vs10.32% all-asset; BTC-only fails. R39 2x2 on the
BTC/ETH book: deadband removes ~5% turnover; best cell full+deadband1.6478/28.53%,
stress.6527/9.30%, later-half Sh1.0742; slow-feature cells later-half Sh1.24 but
full1.54-1.58. Bootstrap Sharpe CI includes zero in every central book. R32 event
audit passes: Aug20 real on both venues; leave-one-out Sharpe1.2764-1.9022. R38 and
R39 rejected; native R32 remains the only provisional candidate. Open: independent
review of the R38/R39 evaluator path and any promotion decision; no deployment.

R40 (user direction, raise cap for two-asset book): cap40 central1.8638/43.17%,
later-half1.6546/16.49% vs BTC6.97%, both halves pass; bootstrap Sharpe CI
.06-3.66 first to exclude zero; stress.7663/14.01% fails; turnover38.77%. cap80
adds risk, no return. Not accepted: 285 reused days, stress unresolved. User
asks for a2y window; native Lighter data begins2025-01-17, so max~17 months
native (from Apr2025, overlaps R32 training through2025-11-24). A2y test needs
Binance proxies with Lighter cost assumptions, and a walk-forward or earlier
model cutoff. Awaiting user decision on which window to build.

R41 cap50/gross100: 1.8132/45.27%, stress .6417/11.81%; recorded, not accepted.
R42 three-year Binance proxy walk-forward (user asked3y; native data begins
2025-01-17): six expanding folds retrained with the R32 recipe, R41 book.
Central -2.94%, Sharpe .110, DD45.4% vs BTC +122.3%; stress -68.9%. Folds1-3
(Aug2023-Feb2025) all lose; folds4-6 gain. REJECTED. The 285-day native results
are period specific; no candidate has a multi-year edge. Chart and series under
var/research/lighter-optimization-r42-three-year/. Open: user decision on
whether to continue this line or close it.

R43-R50 (three-year harness, bin/research-three-year.sh): the deployed relative
long/short ranker is the base (3y Sh1.47/137%); turnover levers (R44), hysteresis
(R45, small gain), slower labels (R46) and sleeves (R48/R49) fail or lose edge.
Breadth is the step change (R47): top-120 point-in-time cohort, 40 names, 1.5x
hysteresis -> 3y Sh1.85/236%, mod137%, stress -32%. R50 holdout on Sep2020-Aug2023
(never evaluated before) confirms: Sh2.21/288%, DD13.5%, all folds positive.
Six-year chain Sh2.02 (CI1.31-2.82), +1202% vs BTC +491%, DD23.6% vs 77%.
Open: 10x-spread stress still negative; 2026 fold flat as cohort shrinks; live
Lighter listing covers fewer names than top120. Next: native Lighter measured-hour
replay of this book on the R6 harness with the widest Lighter-listed cohort.
