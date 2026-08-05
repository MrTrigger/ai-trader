# Phase 1: two candidates, and what the gate said about each

> Recorded because a negative result is the asset. §9 states gates as evidence,
> and evidence that is not written down gets re-litigated by whoever arrives
> next with the same idea.

**Run:** 2021-10-01 → 2026-08-01, weekly rebalance, 253 decisions.
**Universe:** 656 Binance USDT assets including 174 whose series end before
2026-07 — the delisted are present, so this is not a survivor sample.
**Verdict:** **NOT PASSED.** Three of four criteria failed.

## The candidate

Cross-sectional momentum: rank the eligible universe on the return from
**t−30d to t−7d**, hold the top 10 long, size by conviction tilt.

The skip period is deliberate — short-horizon reversal is well documented in
crypto, and a plain 30-day measure contains last week's reversal, so the two
effects partially cancel. The stated mechanism was slow information diffusion in
a market with high retail participation.

Long-only, because shorting spot needs margin and §9.2 puts leverage above 1×
out of scope before Phase 3.

## The result

| | n | return | CAGR | vol | Sharpe | maxDD | turnover | cost bps |
|---|---|---|---|---|---|---|---|---|
| candidate | 253 | −86.87% | −34.30% | 37.5% | −0.93 | −91.89% | 28.16% | 356 |
| at 2× slippage | 253 | −89.14% | −36.83% | 38.8% | −0.98 | −93.20% | 29.01% | 367 |
| baseline (`liquidity_top`) | 253 | −60.52% | −17.49% | 52.5% | −0.10 | −79.24% | 13.20% | 167 |

**It lost, and it lost substantially worse than the baseline.** That second half
is the informative part. A long-only crypto book beginning near the 2021 cycle
top was going to lose over this window regardless — the baseline lost 60% too.
What the ranking added was a further 26 points of loss, at **twice the turnover
and twice the cost drag**.

Costs are not the explanation. Total cost drag is 356bps — 3.6% — against an
86.87% loss. The signal is the explanation.

## Robustness: it is not the risk limit

`max_benchmark_beta = 1.00` rejected 101 of 253 plans, and the typical breach
was 1.0007 against 1.00 — 0.07%. That threshold sat *exactly* on the strategy's
natural operating point (0.80 gross × ~1.25 average beta ≈ 1.00), so 40% of
plans were vetoed on rounding noise.

That is a badly chosen number, and it was corrected. It is **not** why the
strategy lost:

| beta limit | rejected | return | Sharpe |
|---|---|---|---|
| ≤ 1.00 | 101 | −86.87% | −0.93 |
| ≤ 1.50 | 14 | −86.64% | −0.78 |
| unenforced | 0 | −86.94% | −0.75 |

Invariant to within a quarter of a percent. The gate's verdict stands under any
of the three.

## What §6.2 predicted, and what actually happened

The stated failure mode was that long-only crypto momentum is a leveraged BTC
bet whose attribution would call it alpha. The measured average beta of the held
book is ~1.25, and at 0.80 gross that puts portfolio beta at ~1.00 — so the
diagnosis was right about the *exposure*. It was optimistic about the outcome:
the book did not quietly earn beta and call it alpha, it underperformed beta.

A corollary worth keeping: **for a long-only book, `max_benchmark_beta` is
close to a restatement of the gross limit.** Its value arrives with shorts.
Until then it constrains little and can bind on noise, which is what happened.

## What this does not establish

- **Not "momentum does not work in crypto."** One window, one horizon (30/7),
  one holding count (10), one rebalance cadence (weekly), one venue's universe.
  The plateau sweeps exist precisely so those are not asserted from one point.
- **Not a statement about edge.** §7.5 is blunt that no run of this length
  establishes a modest edge in either direction. What it *can* establish is
  gross failure, which is far cheaper to detect — and that is what this is.
- **The window is unkind.** It starts near the 2021 cycle top. A different start
  would give a different number; the *relative* comparison against the baseline
  travels better than the absolute one.

## The IC verdict: the ranking has no usable content

Run after the gate, over the same 253 decisions. Forward returns measured
`mark_open` to `mark_open`, delisted assets kept at their last traded price.

| horizon | periods | eff n | obs | mean IC | t-stat | hit rate | |
|---|---|---|---|---|---|---|---|
| 7d | 253 | 253 | 9,263 | −0.0223 | −1.37 | 46.6% | not distinguishable from zero |
| 14d | 253 | 126 | 9,263 | −0.0382 | −1.74 | 45.5% | not distinguishable from zero |
| 30d | 253 | 59 | 9,263 | −0.0503 | −1.72 | 38.7% | not distinguishable from zero |

**No horizon shows positive predictive content**, which fully explains the
backtest: the strategy underperformed the baseline because the ranking was, at
best, uninformative about subsequent returns.

The sign is consistently negative and the 30d hit rate is 38.7% — the IC was
positive in fewer than four periods in ten. That is *suggestive* of mild
short-horizon reversal, which is a documented crypto effect and the very thing
the 7-day skip was meant to avoid contaminating the measurement with. It is not
established: nothing clears |t| > 2.

### The correction that changed this verdict

Sampling a 30-day forward return every 7 days reuses each window 4.3×.
Uncorrected, the 30d IC reads **t = −3.55** and would have been reported as an
established reversal effect. Deflating to 59 effective periods gives **−1.72**,
which is a hint. The instrument now reports both the raw period count and the
effective one, and reads significance off the latter.

## What this rules out, and what it leaves open

**Ruled out:** that the Phase 1 loss was a construction problem. If the ranking
had content and sizing or costs were destroying it, the IC would be positive and
it is not. Sweeping holding counts, cadences or constructors on this signal
would be tuning the packaging of noise.

**Left open:** cross-sectional *reversal* at this horizon. The sign is
consistently negative across all three horizons. That is a hypothesis, not a
result, and testing the inverse of a failed signal on the same data is the
classic way to fit noise — it wants a fresh window or a different venue's
universe before it means anything.

## The next diagnostic, and why it is not "try another strategy"

**Measure the signal, not the portfolio** (§7.5). Portfolio P&L yields one
observation per rebalance — 253 here. The information coefficient, the rank
correlation between the score and subsequent returns, yields one *per asset per
period*: ~30 names × 253 periods is ~7,500 observations rather than 253. That
is roughly 30× the information per unit of calendar time, and it answers the
question that actually matters — does this rank assets better than chance?

The gate reports the portfolio and says so. It does not compute the IC. That is
the cheapest next thing to know, and it splits the outcome cleanly:

- **IC ≈ 0** → the signal is dead. Delete it and pick a different hypothesis.
- **IC > 0 but the portfolio loses** → the ranking works and the *construction*
  destroys it: sizing, holding count, rebalance cadence or the constraint set.
  That is a different and much more tractable problem.

Doing the sweeps before knowing which of those is true would be tuning a
strategy whose signal might have no content. The IC comes first.

**Answered: the first branch.** `xs_momentum` as specified is retired.

---

# Candidate 2: Gaussian Channel breakout

A deliberately different *family*. Momentum is **relative** — rank assets
against each other. This is **absolute** — each asset judged against its own
channel, and when nothing is breaking out the book goes flat. §10.2 names
channel-breakout trend-following as a legitimate documented family and "a
reasonable baseline to beat, not a destination".

Rules taken from the TR-GC prompt family, which runs this live: enter above the
upper band, exit below it, size by 25-day breakout recency (their 8%/2% of NAV,
expressed here as conviction 4:1). Indicator is Donovan Wall's Gaussian Channel
at its defaults — 144-period, 4-pole, 1.414× the filtered true range.

**Verdict: NOT PASSED.** Three of four criteria failed.

| | n | return | CAGR | vol | Sharpe | maxDD | turnover | cost bps |
|---|---|---|---|---|---|---|---|---|
| `gc_breakout` | 253 | −77.65% | −26.66% | 35.8% | −0.68 | −82.67% | 38.56% | 488 |
| at 2× slippage | 253 | −78.04% | −26.92% | 35.8% | −0.69 | −82.95% | 38.60% | 488 |
| baseline | 253 | −53.31% | −14.58% | 53.9% | −0.02 | −79.21% | 15.98% | 202 |

Better than momentum's −86.87%, still well short of holding the biggest names.

**The folds say whipsaw**, and they are far more varied than momentum's:
+16.08%, +31.43%, −31.52%, −6.18%. The signal captures trends when they exist —
fold1 is a Sharpe of 1.83 through the 2023–24 run — and bleeds through chop.
Turnover is 38.56% per rebalance, the highest of anything tested.

## The spread test, and why rank IC is the wrong instrument here

An absolute signal is a *state*, not an ordering, so the question is whether
assets above the band outperform those below it:

| horizon | eff n | mean spread (above − below) | t | |
|---|---|---|---|---|
| 7d | 185 | +0.93% | 1.60 | not distinguishable from zero |
| 14d | 92 | +1.07% | 0.90 | not distinguishable from zero |
| 30d | 43 | −1.44% | −0.66 | not distinguishable from zero |

Suggestive at short horizons, decaying and reversing by 30 days, and nothing
clears |t| > 2. About 32% of the eligible universe is above its band at any time.

**The structural point is the one worth keeping: the spread is relative and the
book is absolute.** Picking assets that fall less than others still loses money
in a long-only book when everything falls. A relative edge needs a short leg to
be monetised, which is exactly why the long-short variant of those prompts
exists — and why §9.2 putting leverage out of scope before Phase 3 also puts
this class of edge out of reach for now.

## The cadence sweep: a peak, not a plateau

§10.3 asks for this one explicitly — *"Daily is the assumed start... Sweep it in
Phase 1 — one axis, plateau centre, not the peak."* It was also the last live
thread: GC's only positive signal was at 7 days and gone by 30, so if a cadence
existed that could capture it, this is where it would show.

**The control is what makes it interpretable.** A cadence sweep on a long-only
book in a falling market trivially favours trading less — every trade costs and
every position loses — so the baseline was swept on the identical grid and the
reported statistic is `candidate − control`. Both arms pay the same kind of cost
at the same frequency; what is left is whether acting on the signal more or less
often helps.

| every | n | candidate | control | spread | cand. turnover | cand. cost | |
|---|---|---|---|---|---|---|---|
| 1 | 1766 | −72.13% | −59.36% | −12.77pp | 24.03% | 2122 bps | loses |
| 3 | 589 | −89.59% | −62.79% | −26.80pp | 34.57% | 1018 bps | loses |
| 7 | 253 | −77.65% | −53.31% | −24.34pp | 38.56% | 488 bps | loses |
| **14** | 127 | −56.49% | −60.18% | **+3.69pp** | 36.80% | 234 bps | **beats control** |
| 30 | 59 | −58.44% | −45.95% | −12.49pp | 38.54% | 114 bps | loses |

    rebalance_every: plateau 14..14 (width 1), centre 14  <- a PEAK, not a plateau

**This is the rule earning its keep.** Reported without it, "14-day rebalancing
beats the baseline by 3.7pp" is a finding, and a plausible-sounding one. A
setting that wins at exactly one value and loses by 12–27pp at both neighbours
is an artifact of this particular history, and the sweep is built to say so
rather than to hand back its best cell.

The one clean mechanical result: **daily rebalancing costs 21% of NAV in fees**
(2122 bps at 24% turnover per rebalance) against the control's 4.23% turnover.
The candidate churns roughly six times as much for a worse outcome at every
cadence but one.

**A gap, recorded rather than papered over.** The harness script crashed on
serialisation after all ten replays finished, and the per-cadence walk-forward
folds were lost with it. The table above was assembled from the run's own
output. Re-running 90 minutes to add fold detail to a candidate that a width-1
plateau has already retired would be one-more-analysis on a settled question, so
it was not done.

## The structural probe: it was the constraint, not the signal

Both candidates showed a weak *relative* signal and lost *absolutely*. That is a
shape problem rather than a strategy problem, and it is cheap to test: what would
a **market-neutral** version of the same signals have returned?

Construction is deliberately crude and pessimistic — dollar-neutral 50/50, gross
1.0, no leverage, equal weight within each leg, forward returns `mark_open` to
`mark_open`, costs charged on measured name turnover, and re-run at 2× slippage.
**No borrow, no funding, no squeeze risk**, all of which are real and would make
it worse. The number is therefore an *upper bound* on what the shape could
deliver, which is the useful direction for a go/no-go.

| signal | n | gross | net | net @2× | CAGR | vol | Sharpe | maxDD |
|---|---|---|---|---|---|---|---|---|
| `xs_momentum` | 253 | −1.34% | −8.48% | −10.42% | −1.82% | 25.0% | 0.07 | −46.69% |
| `gc_breakout` | 185 | +104.20% | +96.99% | +94.98% | +15.05% | 28.9% | **0.81** | −21.10% |

Momentum is flat even market-neutral, which closes it for good: the signal is
dead in every shape. **The breakout signal is not.** Long-only it lost 78%;
market-neutral it returns +97% after costs and +95% at 2× slippage.

### Three checks that could have killed it, and did not

**Is it just short beta?** GC longs what is above its channel and shorts what is
below, and beaten-down alts are typically higher beta — so a "neutral" book could
be a disguised short. Measured: long leg beta +1.148, short leg +1.272, **net
−0.062** (t = −6.45, net-short in 66% of periods). Statistically unambiguous and
economically tiny — and BTC *rose* over this window, so a −0.062 tilt was a small
drag on the result, not its source.

**Did one regime carry it?** Net return by sub-period: **+19.9%** through the
2021–22 bear, **+25.1%** through the 2023 recovery, **+28.5%** across 2024–26. It
made money in all three, which is what a short-beta bet would not do.

**Does it depend on an arbitrary pick?** The probe took the alphabetically-first
ten names per leg. Re-run holding *every* above-band name against *every*
below-band name: Sharpe 0.81 vs 0.80, net +98.95% vs +95.86%. The effect does not
live in the selection.

### It still does not clear the bar

§7.5's arithmetic, applied to this window: the standard error on an estimated
Sharpe is roughly `sqrt(1/T)` = **0.455** over 4.8 years, so two standard errors
needs **Sharpe > 0.91**. This is 0.81. It is the best thing measured in Phase 1 by
a wide margin, it survives every check aimed at it, and it **does not establish an
edge** — exactly as §7.5 warns no run of this length can.

Nor is it runnable. §9.2 puts leverage above 1× out of scope before Phase 3, and
shorting spot needs margin. Borrow cost, funding and availability on beaten-down
alts are all unmodelled and all cut the same way.

### What it changes

**The long-only constraint, not the signal, is what killed `gc_breakout`.** That
is a roadmap fact rather than a strategy: it says the next useful move is not a
third indicator but a decision about *when this shape becomes reachable*, which
is a venue question (§10.1) and a Phase 3 question. It also gives §6's
`max_net_exposure` — currently unenforced because "it needs shorts to be
meaningful" — a concrete future.

## The benchmark that reframes everything: buy-and-hold BTC

Neither candidate was ever compared against the obvious thing.

**BTC, 2021-10-01 → 2026-08-01: +43.37% total, CAGR +7.73%, Sharpe 0.11, maxDD −76.63%.**

| | return | CAGR | Sharpe | maxDD |
|---|---|---|---|---|
| `xs_momentum` long-only | −86.87% | −34.30% | −0.93 | −91.89% |
| `gc_breakout` long-only | −77.65% | −26.66% | −0.68 | −82.67% |
| `liquidity_top` baseline | −53.31% | −14.58% | −0.02 | −79.21% |
| **buy-and-hold BTC** | **+43.37%** | **+7.73%** | **0.11** | −76.63% |

Every deterministic long-only thing built in Phase 1 lost catastrophically to
doing nothing but holding BTC. That is the honest headline, and it should have
been the first comparison run rather than the last.

## Funding and shortability, modelled with real data

The first long/short probe assumed shorting is free and universal. Both are
false, and both were checked against Binance's own USD-M perpetual data —
1.88M funding intervals across 442 assets.

**Only 442 of our 656 assets ever had a perp**, so 214 are unshortable at any
price. Enforcing that dropped 679 name-slots from the short leg over the window.

**Funding is not a rounding error**: the median across all of it is 0.03%/day,
about 11% annualised. Measured conditionally on our own legs:

| leg | mean funding | direction |
|---|---|---|
| above band (we go long) | **−9.79%/yr** | negative — shorts pay longs |
| below band (we go short) | **+4.20%/yr** | positive — our short receives |
| short − long | **+13.99%/yr** | |

The sign on the long leg is the surprise. Assets that have just broken out
attract short interest fading the move, the perp trades below spot, and funding
goes negative — so **the long leg gets paid as well**. That is a documented
crypto dynamic and it is doing a lot of work in the numbers below.

| structure | shortability | n | net | CAGR | vol | Sharpe | maxDD | funding/yr |
|---|---|---|---|---|---|---|---|---|
| perp-long / perp-short | not enforced | 185 | +150.01% | +20.86% | 28.7% | 1.03 | −19.19% | +6.51% |
| perp-long / perp-short | enforced | 185 | +181.45% | +23.86% | 28.7% | 1.16 | −19.35% | +6.86% |
| **spot-long / perp-short** | enforced | 185 | **+139.32%** | **+19.78%** | 28.5% | **1.00** | −22.54% | +2.22% |

The third row is the one to read. Spot-long/perp-short is the realistic
market-neutral shape at 1.0 gross with no leverage, and it is the only structure
with no funding-data gap on the long leg. It clears §7.5's bar — Sharpe 1.00
against the 0.91 that two standard errors needs over this window — and it beats
buy-and-hold BTC on every axis: +139% vs +43%, CAGR 19.8% vs 7.7%, Sharpe 1.00
vs 0.11, drawdown −22.5% vs −76.6%.

**A known bias in the perp/perp rows**: 10% of long-leg names had no perp, so
their funding was silently treated as zero. Since the real figure is *negative*
(a benefit), those two rows understate rather than flatter. The spot-long row is
unaffected, which is another reason to read it rather than them.

**Still unmodelled, all cutting the same way**: borrow availability, perp
liquidity at size, squeeze risk on beaten-down names, and the fact that a short
leg made of the weakest assets is exactly where a position gets recalled or
gapped through.

**And it remains one window.** Everything above is a single 4.8-year slice that
also produced two dead candidates, so the run is not naive to it. A fresh window
is the test that matters and it is next.

## The fresh window, and the benchmark that decides it

A window with **zero overlap** with anything tested: 2019-10 → 2021-10, using
bars pulled back to 2017 so the channel warmup and history floor are satisfied
before the first decision, and funding pulled back to 2019 so the short leg is
costed with real data throughout.

| window | | strategy | | | buy & hold BTC | | |
|---|---|---|---|---|---|---|---|
| | return | CAGR | Sharpe | | return | CAGR | Sharpe |
| **fresh** 2019-10..2021-10 | +100.4% | +41.5% | **2.19** | | **+480.7%** | +140.7% | 1.09 |
| orig 2021-10..2026-08 | +139.3% | +19.8% | **1.00** | | +43.4% | +7.7% | 0.11 |
| **combined, chained** | **+379.5%** | +25.8% | | | **+657.8%** | +34.5% | 0.48 |

**The signal survives out of sample.** Sharpe 2.19 on the fresh window against
the 1.41 that two standard errors needs over two years; 1.00 against 0.91 over
4.8. It clears §7.5's bar on both, independently. That is more than anything else
in Phase 1 has managed and it is a real finding.

**And it loses to buy-and-hold BTC.** +379.5% against +657.8% over the full 6.8
years. Not marginally — by a third of the total return.

### Why, and why it was predictable

The split is the whole story: the strategy **beats BTC by 96 points** in the
flat-to-falling window and **loses to it by 380 points** in the raging bull. A
market-neutral book does not capture beta. That is not a defect in the
implementation, it is the definition of the shape — and it means "beat
buy-and-hold BTC" is a criterion this construction can only satisfy in regimes
where BTC does badly.

Risk-adjusted the picture inverts: worst drawdown −22.5% against BTC's −76.6%,
and roughly 2–4× the Sharpe depending on window. **Absolute return says BTC,
risk-adjusted return says the strategy**, and they do not resolve into one
answer without deciding what the account is actually for.

### Two things that temper the fresh-window number

**Funding does 36% of the work there** — 14.79pp of a 41.48% CAGR, against 11%
(2.22pp of 19.78%) in the original window. 2020–21 was a leverage mania and
perp funding was extreme; a short leg earned carry simply for existing. Funding
regimes change, and a third of that CAGR is a bet that they do not.

**n = 56 periods over two years.** It clears the bar for its own length, but the
bar scales with `sqrt(1/T)` precisely because short windows are easy to clear by
luck.

## The regime tilt: keeping the edge, stopping being flat through the big moves

The market-neutral book grinds steadily upward and sits out every large
directional move, up and down. The hypothesis: point the **same detector already
used on each asset** at the benchmark, and let its channel state decide how much
net exposure the book carries.

    BTC above its upper channel   -> tilt net LONG
    BTC below its channel filter  -> tilt net SHORT
    in between                    -> stay neutral

**Gross stays at 1.0 in every state** — only the split moves, so this is not
leverage and §9.2 still holds:

    up:      long 0.5+t,  short 0.5-t
    down:    long 0.5-t,  short 0.5+t
    flat:    long 0.5,     short 0.5

`t = 0` is exactly the book already measured, so the sweep carries its own
control: any setting that fails to beat that column has added nothing.

| tilt | fresh CAGR | fresh Sharpe | fresh maxDD | orig CAGR | orig Sharpe | orig maxDD |
|---|---|---|---|---|---|---|
| 0.00 | 41.5% | 2.19 | −20.5% | 19.8% | 1.00 | −22.5% |
| 0.05 | 54.2% | 2.60 | −16.5% | 21.1% | 1.10 | −18.4% |
| 0.10 | 67.4% | 2.79 | −13.0% | 22.2% | **1.15** | −14.1% |
| **0.15** | **81.1%** | **2.82** | **−9.9%** | **22.8%** | 1.14 | −16.5% |
| 0.20 | 95.1% | 2.78 | −13.3% | 23.1% | 1.09 | −20.8% |
| 0.25 | 109.4% | 2.71 | −16.7% | 23.1% | 1.02 | −25.0% |
| 0.30 | 124.0% | 2.64 | −20.6% | 22.6% | 0.96 | −29.2% |
| 0.40 | 153.5% | 2.50 | −28.7% | 20.6% | 0.83 | −45.9% |

    improves Sharpe in BOTH windows at: 0.05, 0.10, 0.15, 0.20, 0.25
    regime_tilt: plateau 0.05..0.25 (width 5), centre 0.15

**A real plateau, not a peak.** Five settings wide, improving Sharpe in each
window *independently*, with the centre at 0.15. That is the distinction the
plateau rule exists to draw, and it is the first time in Phase 1 that a sweep has
produced one.

**Drawdown improves alongside return**, which is the unusual part. Adding
directional exposure normally buys return with risk; here 0.10–0.15 is better on
both axes than 0.00 in both windows. The mechanism is that the tilt goes *short*
in downtrends, so the 2022 decline that mauled every long-only variant is
partially harvested rather than merely avoided.

### Against the benchmark, chained across both windows

| | total return | worst-window drawdown |
|---|---|---|
| market-neutral (tilt 0) | +379.5% | −22.5% |
| **regime-tilted (0.15)** | **+787.7%** | **−16.5%** |
| buy & hold BTC | +658.6% | −74.2% |

**It beats buy-and-hold BTC on absolute return and holds a drawdown roughly a
quarter of its depth.** Both windows clear §7.5's Sharpe bar independently —
2.82 against 1.41 on the fresh window, 1.14 against 0.91 on the original.

### What is still not established

- **Two windows, 241 periods.** Better than one, still not many.
- **The tilt shares an indicator family with the selection signal.** Both read a
  Gaussian channel, so they are not independent bets and a regime where that
  detector fails would hurt twice.
- **Funding assumptions carry through** unchanged, and still exclude borrow,
  squeeze and liquidity-at-size.
- **It needs shorts**, so §9.2 puts it behind Phase 3 and §10.1's venue decision
  regardless of how good the number is.

## Reading the market faster than the positions

A challenge worth recording, because testing it changed the answer: *is the
level test the right regime mechanism? If BTC is crashing we should not wait
until it is below the filter.*

**Measured first.** How late does a level test turn defensive?

| | 2021-11 top | |
|---|---|---|
| BTC peak 2021-11-08 at 67,526 | | |
| leaves "up" (below upper band) | +10d | **−15.7%** from peak |
| turns "down" (below filter) | +14d | **−16.7%** from peak |
| filter slope turns negative | +32d | **−30.2%** from peak |

The concern is real — 16.7% before turning defensive. But the *proposed fix is
worse*: the derivative of a 4-pole 144-day filter lags **more** than price
crossing it, because smoothing is exactly what a derivative of a smoothed series
inherits. Tested in the strategy rather than argued about:

| regime | fresh Sharpe | fresh maxDD | orig Sharpe | orig maxDD | combined |
|---|---|---|---|---|---|
| level, 144d (was) | 2.82 | −9.9% | 1.14 | −16.5% | +787.7% |
| **filter slope** | 2.69 | −22.8% | **0.83** | **−28.1%** | +613.6% |
| continuous position | 3.00 | −9.7% | 0.99 | −19.3% | +716.9% |

Slope is worst in both windows. Refuted.

**The instinct was right about the wrong thing.** What was too slow was not the
*test* but the *instrument*: a 144-day channel is the right timescale for
deciding which assets to hold and the wrong one for deciding how exposed to be.
Sweeping the regime channel's period, tilt pinned at 0.15:

| period | fresh CAGR | fresh Sharpe | orig CAGR | orig Sharpe | combined | min Sharpe |
|---|---|---|---|---|---|---|
| 12 | 23.5% | 1.08 | 31.3% | 1.56 | +468.6% | 1.08 |
| 18 | 26.5% | 1.21 | 35.6% | 1.70 | +599.1% | 1.21 |
| 24 | 38.2% | 1.67 | 36.4% | 1.74 | +758.4% | 1.67 |
| 36 | 50.5% | 1.99 | 35.9% | 1.72 | +899.8% | 1.72 |
| **48** | **65.2%** | **2.76** | **36.8%** | **1.74** | **+1142.0%** | **1.74** |
| 72 | 67.7% | 2.84 | 31.6% | 1.53 | +961.9% | 1.53 |
| 96 | 81.9% | 3.15 | 31.9% | 1.53 | +1164.0% | 1.53 |
| 144 | 81.1% | 2.82 | 22.8% | 1.14 | +787.7% | 1.14 |

    beats the 144d baseline on BOTH min-Sharpe and combined return at 24..96
    regime_channel_period: plateau 24..96 (width 5), centre 48

A second genuine plateau. The criterion is deliberately the **worse** of the two
windows — improving the good window while wrecking the bad one is how a
parameter gets fitted — and 48 raises that floor from 1.14 to 1.74.

### Where that leaves the construction

| | total return | worst-window drawdown |
|---|---|---|
| market-neutral (no tilt) | +379.5% | −22.5% |
| regime-tilted, 144d read | +787.7% | −16.5% |
| **regime-tilted, 48d read** | **+1142.0%** | −19.5% |
| buy & hold BTC | +658.6% | −74.2% |

Two parameters, each at the centre of a five-wide plateau, each chosen on the
worse window: **tilt 0.15, regime period 48**. The principle underneath is worth
keeping even if the numbers move: *the market state has to be read faster than
the positions are chosen.*

## Leaning hard needs a fast channel — and where the search went wrong

The synthesis was worth testing: lean-sizing failed on a 144-day channel, but
maybe the failure was *slow channel plus magnitude sizing* rather than magnitude
sizing itself. On a channel short enough to turn, leaning hard might work.

On the 144-day channel it does not, and the failure is diagnostic:

| mode | fresh Sharpe | orig Sharpe | |
|---|---|---|---|
| lean (sign + size from slope) | 2.70 → 2.13 | **0.87 → 0.34** | collapses |
| state × lean (bands set sign, lean sets size) | 2.82 → 2.48 | **0.90 → 0.20** | collapses |

Both hold up in the fresh window and fall apart in the original. The fresh
window is essentially one sustained trend; the original is choppy. Lean-sizing
makes exposure proportional to how strong the trend *has already been*, so it is
largest just after the biggest run — disproportionately just before the turn.
It is momentum-of-momentum, and it is pro-cyclical the wrong way.

That is the mirror image of the period result, and the pair is the useful part:

| change | effect on the **worse** window |
|---|---|
| read the market **faster** (48d vs 144d) | min Sharpe 1.14 → **1.74** |
| size the tilt by **trend magnitude** (144d) | min Sharpe 1.14 → **0.90** |

**Reading faster catches turns. Sizing by magnitude leans hardest into trends
that are already old.**

### A costing bug the search surfaced

Turnover was measured as the symmetric difference of the *name* sets. A tilt
swinging from +0.5 to −0.5 holds the **same names at inverted weights** — an
enormous trade the model charged nothing for. That systematically under-costs
exactly the settings that tilt hardest, which is the direction the search was
being pulled. Fixed to `sum |Δweight|` across the union of holdings. It moved
the headline from +7725% to +6884%: real, and not the explanation.

### The thing that matters more than any of these numbers

On a fast channel, lean-sizing *does* clear every criterion, with an interior
optimum rather than a boundary one — plateau 4–12, centre 8, min Sharpe 2.06,
combined +6884%. Roughly 88% CAGR.

**That result should not be believed, and the reason is arithmetic.**

| what was swept | configurations |
|---|---|
| tilt magnitude | 16 |
| regime definitions | 8 |
| regime channel period | 16 |
| lean scale × mode × period | 36 |
| extended scale sweep | 12 |
| **total** | **88**, all on the same two windows |

Forty-four distinct configurations, each scored on both windows. Under the null
of no edge, taking the best of forty-four on a two-window criterion selects for
luck. More decisively: **both windows have now informed parameter choice, so
neither is out-of-sample any longer.** The out-of-sample claim earned earlier —
one hypothesis, tested once, on a window never touched — has been spent.

The early results and the late ones are not the same kind of evidence, and the
document should not present them as though they were:

| finding | status |
|---|---|
| market-neutral GC has a relative edge (Sharpe ~1.0) | **one hypothesis, one clean OOS test** |
| regime tilt improves it (0.15, plateau of 5) | 2nd–3rd hypothesis; still credible |
| faster regime channel (48d, plateau of 5) | ~30 configs in; weakening |
| lean-sizing at scale 8 (+6884%) | 88 configs in; **not evidence** |

Also worth noting on its own terms: the drawdowns doubled to get there —
−27.6% and −27.1% against −9.9% and −16.5% — which is not the steady character
the construction was being steered toward.

**The correct next step is not another variant.** It is to freeze a
configuration and get data that has never informed it: forward-test, or a third
window. There is no third window available here — pre-2019 has no perpetuals, so
no funding and no shortability — which means forward testing is the only honest
instrument left, and §7.5 already says what it can and cannot establish.

## Falsifying the fast version instead of doubting it

"88 configurations, therefore not evidence" is an assertion, not a test. Three
tests turn it into one.

**1. Label shuffle.** Keep the tilt schedule, leg sizes, costs, funding and
universe identical; randomise only *which* assets land in which leg. If the
result survives, the channel signal contributes nothing.

    REAL   min-Sharpe 2.06   combined +6884%
    NULL   median 1.29   90th pct 1.58   max 1.71   (25 seeds)

Real sits above all 25. The selection is doing work.

**2. Timing only.** Same tilt schedule, legs replaced by BTC itself.

    TIMING  min-Sharpe 1.30   combined +504.6%

The selection adds roughly 8× on top of the timing, so this is not a BTC
market-timer in a long/short costume.

**3. The search itself.** The first two test one configuration; the real result
was the best of forty-four. So give **each null draw the same sweep** and take
its best — the fair comparison.

    REAL best-of-sweep    min-Sharpe 2.06   combined +6884%
    NULL best-of-sweep    median 1.33   max 1.71        (24 seeds)
    0/24 matched or beat it   ->   empirical p = 0.040

A signal with no information, allowed to search the same grid, tops out at 1.71.
**The search does not explain the result.**

Worth reading alongside: the null draws routinely make **+466% to +1588%**
combined, median +830%. Most of the headline is the tilt, the funding carry and
alt beta — things random legs also collect. The selection is what takes it from
~830% to 6884%.

### What these tests do and do not license

They rule out one specific failure — that the number is an artifact of searching.
That was the objection, and it does not survive.

They do **not** make the windows out-of-sample. Parameters were still chosen
using both, and no test on this data can undo that. And they say nothing about
whether the edge persists: the same signal was measured collapsing from
+1.19%/wk to +0.11%/wk after mid-2025, with the hit rate unchanged at ~57%. A
real edge stopping is not hypothetical here, it is observed.

    p = 0.040 is also close to the floor 24 draws allows, (0+1)/(24+1).
    The null shuffles SELECTION while holding the tilt schedule fixed, so it
    establishes that the picking is real, not that the timing is.

---

# Where Phase 1 stands

Two candidates, two families, both failed. `config/default.toml` is back to the
Phase 0 placeholder that claims no edge, because a config naming a signal that
failed its gate is the softest possible version of shipping it.

**The market-neutral shape is the only thing that measured an edge**, it holds
out of sample, and it does not beat buy-and-hold BTC over the full period. §9.2
also puts it out of reach before Phase 3, since it needs shorts.

What the exercise did produce, and what it cost:

| | |
|---|---|
| Data | 656 assets, 714k bars, **174 delisted series retained** |
| Instruments | replay backtest, walk-forward, plateau sweeps, gate, IC, spread test |
| Bugs found by running rather than reading | **five**, four of which flattered the strategy |

The largest was ticker reuse: LUNA renamed to LUNC with Luna 2.0 taking the
ticker turned a −87% result into **+23,742%**, and it was caught by an
implausible volatility number rather than by the return.

## What would actually be worth trying next

Ordered by evidence-per-unit-effort, not by appeal:

1. ~~**The 7d spread, measured properly**~~ — done. The cadence sweep found a
   peak, not a plateau, and closed the thread. `gc_breakout` is retired.
2. **Test the structural hypothesis directly.** Both candidates showed a weak
   *relative* signal and lost *absolutely*. That combination is not a strategy
   problem, it is a shape problem: a relative edge needs a short leg, and §9.2
   puts leverage out of scope before Phase 3. Measuring what a long/short
   version of these same signals would have returned is cheap and settles which
   of two very different situations we are in — signals that are simply bad, or
   signals whose edge is real but unreachable with the book we are allowed to
   run. That is a roadmap fact either way and it costs one diagnostic.
3. **Cross-sectional reversal.** Momentum's IC was consistently negative across
   every horizon. On a *fresh window*, because testing the inverse of a failed
   signal on the same data is how noise gets fitted.
4. **Not a third trend variant.** Two families have now failed the same way.
   The next thing tried should differ in structure rather than in indicator.

---

# Addendum: what the port into the pipeline found

Everything above the line was produced by standalone research scripts that
computed weights directly. Moving the long/short strategy into `signal.py` so it
runs through `pipeline.run()` — one implementation, per §0.1 — falsified three of
those results and characterised the strategy differently. This is the clearest
argument for §0.1 the project has produced, so the errors are recorded rather
than quietly corrected.

## 1. Every reported Sharpe was inflated by 15–23%

The research scripts advanced past a stand-down week with `continue`, appending
no point to the equity curve, and then annualised the resulting series with a
factor of 52. That asserts 52 traded weeks a year. **The book actually trades
about 36** — it is flat 31% of the time — so the annualisation was applied to a
series with roughly a quarter of its observations missing.

| window | live | flat | Sharpe as reported | corrected |
|---|---|---|---|---|
| fresh 2019-10..2021-10 | 55 wk | 34 wk (38%) | 2.34 | **1.81** |
| orig 2021-10..2026-08 | 184 wk | 68 wk (27%) | 2.08 | **1.76** |

Returns, CAGR and drawdown are unaffected: the compounding is a straight product
and the annualisation there used elapsed calendar time. Only the risk-adjusted
figures moved — which is precisely the criterion the strategy was selected on.

**The comparison against BTC was biased by this.** Buy-and-hold is never flat, so
its Sharpe was never inflated, and every strategy-vs-BTC Sharpe comparison above
favoured the strategy by this margin.

`backtest.replay()` never had this bug. It skips a date only on `GateFailure` —
a data outage — while a flat book still produces a plan with zero targets and
records a `Step` at unchanged NAV, which is a real zero-return week. The repo
implementation was correct and the research that bypassed it was not.

## 2. The book stands down in exactly the conditions it should be paid for

111 of 357 weeks are flat. **80 of those are caused by too few longs**, and 59%
of them occur with the benchmark below its regime filter. The long leg is a
selection and the short leg is a residual, so when nothing is trending upward the
long leg falls below the three-a-side minimum and the *whole* book stands down —
abandoning the short leg, which was the half that would have profited.

This is the mechanism behind "why did it not capitalise on the big downtrends",
and it is a design consequence rather than a tuning problem: requiring both legs
means requiring something to be trending up.

## 3. "Market-neutral with a tilt" is not an accurate description

At maximum tilt the short leg's weight reaches zero and the book is 100%
directional. Measured over 3,227 days:

| |net exposure| / NAV |
|---|---|
| median | 0.511 |
| 75th percentile and above | 0.800 — equal to gross |
| days pinned fully one-sided | **1,291 / 3,227 (40.0%)** |

Gross never exceeds its target, so §9.2 holds and no leverage is introduced. But
40% of the time this is a directional book, net short (1,504 days) roughly twice
as often as net long (844). It is better described as a **benchmark market-timing
strategy expressed through a long/short book** than as a market-neutral one, and
that reframing raises the stakes on the label-shuffle null (p=0.040) considerably:
the timing component is not a modifier, it is most of the strategy.

## 4. What the risk gate said, once it was finally asked

| check | observed | limit | |
|---|---|---|---|
| `max_gross_exposure` | 0.800 | 1.00 | ok |
| `max_position` | 0.033 | 0.25 | ok |
| `max_position_count` | **34** | **12** | **rejected** |
| `max_cluster_exposure` | 0.106 | 0.40 | ok |
| `max_benchmark_beta` | **0.093** | 1.25 | ok |

Two of these are informative beyond pass/fail. The **beta check at 0.093** is
independent confirmation that the long/short construction cancels beta as
claimed; the long-only form of the same signal would sit near 1.0. The **cluster
check at 0.106** passes because the legs net out within each cluster, which a
long-only book of the same names could not do.

`max_position_count` rejected every plan. The strategy was measured under the
limit rather than the limit being raised to fit it, and it survives: min-Sharpe
1.97 truncated to 12 names against 2.08 untruncated (both pre-correction). The
sweep peaked at 16 names; **12 was kept because it is the config's number**, and
choosing 16 would have converted a constraint into a fitted parameter.

Truncation did require inventing a short-leg ranking — distance below the lower
band — because the residual leg carries no score of its own. That is a genuine
new degree of freedom and is disclosed as one.

## 5. Two hypotheses tested and refuted

**Weighting the leg split by breadth.** If 30 names are above their bands and 5
below, allocating by evidence rather than 50/50 sounds obviously right. It is
monotonically worse: min-Sharpe 2.06 → 1.45 as the breadth weight goes 0 → 1.
Breadth does not predict the forward week (rho +0.09, t 1.45), and the widest
quartile is nearly the worst-performing.

**A symmetric short leg** (short only what is below the *lower* band, making both
legs selections). Worse — min-Sharpe 1.49 against 2.05 — and it cannot form a
book at all for most of 2019–21 for want of three qualifying shorts. The residual
short leg is kept, and is now described honestly as a hedge rather than a second
forecast.

## 6. Still open

- **The edge is decaying and nothing here explains why.** On a fixed stake the
  strategy returned ~80% of stake per year through 2020–2024 and ~30% across
  2025–26. The compounded curve hides this completely, because a weaker edge on
  an account eighteen times larger still adds dollars.
- **`max_net_exposure` has no value.** Config left it unset on the explicit
  grounds that it "needs shorts to be meaningful". They have arrived. Setting it
  is a risk-appetite decision that trades directly against capturing trends.
- **None of the corrected numbers have been re-run through `replay()`.** The
  strategy now runs through the pipeline, but the headline figures in this
  document still come from the scripts. They should be regenerated by the
  harness before any of them is quoted again.

---

# The result that supersedes everything above

Cleaning up the research page for internal consistency surfaced a discrepancy: a
run over the full 2019-2026 span returned +113% while its own two halves returned
+397% and +1177%. The two runs differed in one respect. They started 731 days
apart, which is not a multiple of seven, so they rebalanced on different weekdays.

## Seven phases, one strategy

Nothing in the strategy refers to a weekday. Shifting the rebalance by three days
changes no parameter, no rule, no data - only which seven-day windows the returns
are chopped into. A real weekly edge should barely notice.

| rebalance day | fresh return | orig return | orig Sharpe | orig maxDD |
|---|---|---|---|---|
| **Friday** | **+397.2%** | **+1177.4%** | **1.63** | −27.1% |
| Saturday | +145.5% | +207.6% | 0.79 | −35.0% |
| Sunday | +16.9% | +102.1% | 0.55 | −40.9% |
| Monday | +38.3% | +82.2% | 0.50 | −58.0% |
| Tuesday | +167.6% | **−57.1%** | **−0.10** | −80.6% |
| Wednesday | +186.8% | +6.6% | 0.23 | −51.7% |
| Thursday | +155.6% | +761.5% | 1.31 | −30.6% |
| **median** | **+155.6%** | **+102.1%** | **0.55** | |

**Friday is the best of the seven in both windows, and Friday is the phase every
result in this document was computed on.**

Buy-and-hold BTC is the control. It is the same asset in all seven phases, so its
spread is the sampling noise floor: returns 15.7%..47.8%, Sharpe 0.31..0.42, a
range of 0.11. The strategy's Sharpe range is 1.7 - fifteen times wider.

## Every candidate, re-measured

| candidate | fresh med | orig med | Sharpe med | Sharpe range |
|---|---|---|---|---|
| buy & hold BTC | **+425.0%** | +30.6% | 0.37 | 0.31 … 0.42 |
| market-neutral | +95.3% | +6.8% | 0.17 | −0.06 … 0.79 |
| + regime tilt 0.15, 144d | +220.0% | +54.5% | 0.46 | 0.08 … 0.87 |
| + regime read at 48d | +118.7% | +81.3% | 0.65 | −0.03 … 1.36 |
| + lean-sized tilt, scale 8 | +155.6% | +102.1% | 0.55 | −0.10 … 1.63 |
| as shipped (12 names, capped) | +201.2% | +210.2% | 0.73 | 0.04 … 1.75 |

Every variant's Sharpe range reaches down to approximately zero, so none is
distinguishable from no edge. **BTC beats every variant in the fresh window on
median.** The §9 requirement to beat buy-and-hold is not met on this evidence.

## Why nothing caught it

The fresh out-of-sample window, the walk-forward folds, the plateau sweeps, the
2x-slippage stress and the label-shuffle null were all run on the Friday phase.
The null test is the sharpest illustration: all 24 draws used the same phase as
the real data, so it compared a lucky phase against a lucky phase and returned
p=0.040. It was measuring selection effect within a phase while the phase itself
was the dominant free parameter.

`find_plateau` was the right instinct - a result should be insensitive to small
perturbations of its parameters - applied to too small a set. Rebalance phase was
never treated as a parameter because nothing in the code exposes it as one, and
that is exactly what made it dangerous. **An unrecognised degree of freedom is
not protected against by testing the ones you recognised.**

## What this retracts

- **"+6884%"** — one phase of seven; median +102%, worst −57%.
- **"survives the null at p=0.040"** — the null shared the phase and cannot support the claim.
- **"the edge is decaying"** — a Friday artifact. On the median phase 2022 is the worst year and 2025 the best, the reverse of the earlier reading.
- **"min-Sharpe 2.05"** — corrected first to 1.76 for the flat-week bug, and now to a median of 0.55 with a range including negative.
- **"beats buy-and-hold BTC"** — not on median across phases.

## What survives

- The market-neutral construction genuinely cancels beta: the gate measured 0.093 against a 1.25 limit.
- Long-only trend on this universe is bad: −77.65% and −88.9%, robust to anything this small.
- The data layer is sound — 670 assets with 188 delisted series retained, point-in-time universe and borrow.
- The pipeline, once used, rejected the plan and enforced limits the research had been ignoring.

## The rule this buys

Any future backtest of a strategy that rebalances on a period of N units must be
reported across all N phases, median and range, with the benchmark measured the
same way as its noise floor. A single-phase number is one draw from a
distribution and is not a result. This belongs in the gate, not in a convention.

---

# Where this actually stands

Two further results, both from questions that turned out to matter more than the
answers I had.

## Tranching, and why the strategy should be run that way

Rebalancing weekly means choosing one of seven decision grids and discarding six.
Holding all seven at once - seven sub-books, one per weekday, a seventh of capital
each - removes the choice rather than justifying it. Each sub-book has the same
weekly holding period and the same turnover, so it costs nothing.

| rebalance day | fresh | orig | Sharpe | maxDD | combined |
|---|---|---|---|---|---|
| Tue | +201.2% | −45.9% | 0.04 | −76.6% | +63.0% |
| Sun | −17.0% | +185.4% | 0.70 | −43.9% | +136.9% |
| Mon | −9.1% | +210.2% | 0.73 | −51.1% | +182.0% |
| Wed | +349.7% | +47.6% | 0.40 | −52.4% | +563.6% |
| Sat | +179.7% | +494.5% | 1.10 | −38.8% | +1562.8% |
| Thu | +301.4% | +970.3% | 1.35 | −38.0% | +4196.3% |
| Fri | +295.3% | +1913.9% | 1.75 | −31.3% | +7860.3% |
| **tranched** | | | | **−27.8%** | **+1515.1%** |

The tranched drawdown is shallower than *every* individual phase including the
luckiest, because the sub-books are only about a third correlated (mean
off-diagonal 0.334) and their worst weeks do not coincide. That correlation is
itself the cleanest measure of how thin the signal is: seven runs of the same
strategy on the same period should move together if they were tracking something
persistent.

## The current best version, and what it is made of

Tranched, twelve positions, 25% per-name cap - the configuration the risk gate
accepts.

| variant | fresh ret | fresh Sh | orig ret | orig Sh | orig t | combined |
|---|---|---|---|---|---|---|
| strategy | +227.4% | 1.67 | +393.3% | 1.25 | 2.74 | **+1515.1%** |
| BTC buy & hold | +558.7% | 1.87 | +64.3% | 0.46 | 1.00 | +982.0% |
| selection only | +124.9% | 2.08 | +90.4% | 0.77 | 1.69 | +328.2% |
| timing only | +30.3% | 0.52 | +20.5% | 0.28 | 0.61 | +56.9% |

It beats buy-and-hold on the combined window and on drawdown (−27.8% against
−72.9%), and **loses the fresh window outright** on both return and Sharpe. The
case rests on the second window and the drawdown, not on a sweep.

Neither component explains the whole. Timing alone is weak, which kills the
simplest deflationary story - this is not BTC market-timing in a long/short
costume. Stripping the perp funding carry from the selection leg leaves Sharpe
1.70 and 0.71, so it is not a funding harvester either. Funding carry alone was
strongly positive before 2021 and turned negative after: trading costs now exceed
funding income.

**This sits badly against the spread test**, which is flat over the full window
(+0.14% at 7d, t=+0.27; negative at 14d and 30d). The book is not trading the raw
group split - it ranks within each leg, caps count and size, and collects
funding - and those parts have had the least scrutiny. Resolving the
contradiction is the most informative thing left to do.

## Two failure modes in the drawdowns, one of which must not be "fixed"

The book stood down 13 of 40 weeks across late 2025 and 2026 with 27-34 shortable
candidates available and the tilt pinned at maximum bearish, including a week BTC
fell 16.2%. The three weeks before the first stand-down earned +8.4%, +13.5% and
+10.8% entirely from shorts. It looks self-evidently wrong.

Running the surviving leg instead is **worse**: combined +729% against +1513%,
orig Sharpe 0.62 against 1.23, drawdown −53.6% against −26.9%. The ledger shows
why - BTC rallied +6.8%, +3.0%, +3.4% and +4.1% during those flat weeks. Bear
rallies are where a fully-short crypto book dies, and `MIN_LEG = 3` is an
accidental but effective filter against taking a large directional position when
nothing at all is trending up.

The second failure is unexplained: through July 2026 the long leg lost 10-13% a
week while the regime read "flat", so no tilt protected it.

## What the record is now

`planner/research.py` builds the whole record in one pass, over one window, from
the store. The research page shows the current state only; this document is the
log. Retracted along the way: +6884%, the p=0.040 null, "the edge is decaying",
min-Sharpe 2.05, and "beats buy-and-hold" as an unqualified claim.

---

# Venue: Hyperliquid, and what that changes

Chosen for **self-custody**, not for price. On fees the two candidates are within
about 10pp of each other across the whole backtest, which is not a margin worth
trading custody against in either direction.

## Fee tiers do not apply and will not start applying

Tiers are thresholds on rolling traded volume, and traded volume is just NAV
times turnover, so the balance required to reach one is arithmetic:

| tier | window | threshold | NAV required |
|---|---|---|---|
| OKX VIP 1 | 30d | $5M | ~$1.75M |
| OKX VIP 2 | 30d | $10M | ~$3.49M |
| Hyperliquid tier 1 | 14d | $5M | ~$3.74M |

A $30k book at this turnover generates **$86k** of 30-day volume. Raising the
balance to $100k, $250k or even $1M leaves it in the base band on both venues, so
the tier programme is not a lever at any size currently contemplated.

## What the venue decision actually changed

Hyperliquid is perps-first, so **both legs are now perpetual futures**. Three
consequences, and the second is a genuine behaviour change rather than a
re-pricing:

1. **Fees fall on the long leg.** A perp long pays the perp taker rate rather
   than the materially higher spot one.
2. **Funding is now paid on the long leg**, not only received on the short. Over
   the run that cuts funding from +15.6% to +7.6% — and the fee saving more than
   covers it, so the perps-both structure is better despite losing the income.
3. **The listing table gates the whole universe.** An asset with no perp cannot
   be held in either direction, where previously it was merely unshortable. The
   `shortable` column is renamed `perp_listed`, because a name that gates longs
   while calling itself "shortable" is the kind of thing that causes a bug rather
   than merely reading oddly.

| structure | long | short | return | Sharpe |
|---|---|---|---|---|
| OKX spot long / perp short | 12bps | 7bps | 710.3% | 1.27 |
| OKX perp / perp | 7bps | 7bps | 780.1% | 1.36 |
| **HL perp / perp (shipped)** | **6.5bps** | **6.5bps** | **790.1%** | **1.36** |
| HL perp / perp, maker fills | 1.5bps | 1.5bps | 896.9% | 1.43 |

## The ranking of what matters, which is not what the question implied

1. **Maker versus taker fills — about +110pp.** Far the largest lever, and the
   only one worth engineering for. Config stays at the taker rate because passive
   fills are an execution capability the system does not have, and assuming them
   would price in work that has not been done.
2. **Perps on both legs versus a spot long — about +70pp.**
3. **OKX versus Hyperliquid — about +10pp.** Effectively a tie.
4. **Fee tier — zero, at any plausible balance.**

## Fee sensitivity, and why the cadence question turns on it

Cost enters linearly in turnover, so the recorded ledger can be re-priced exactly
at any rate. Break-even — the all-in rate at which the entire edge is consumed —
is **98bps weekly** against an assumed 7. A 14× margin, so the weekly book is
insensitive to getting fees wrong.

The daily cadence is not: break-even **35bps**, and at zero cost daily *beats*
weekly (1200.8% against 918.8%). Daily has more raw signal and is destroyed
purely by its 3.1× turnover. The crossover is around **5bps all-in**, which means
the cadence choice is really a fee question, and one worth revisiting if maker
fills become available.

## Outstanding data gap

**Funding and listing dates are Binance USD-M, not Hyperliquid.** Every funding
figure above is an estimate taken from the wrong venue. Rates are correlated
across venues but not identical, and listing dates differ, so a Hyperliquid pull
should replace this before the numbers are trusted. The fee rates were read from
published schedules rather than the venue API and should be confirmed the same
way.

## The Binance funding proxy, checked against Hyperliquid's own API

`app.hyperliquid.xyz/fundingComparison` is a client-rendered page and fetches as
an empty shell, but it is driven by a public API that serves the same data:
`POST api.hyperliquid.xyz/info` with `{"type":"fundingHistory","coin":...}`,
paginated at 500 records, funding hourly.

**Hyperliquid funding is roughly double Binance's**, consistently:

| asset | Binance | Hyperliquid | gap |
|---|---|---|---|
| BTC | +7.3%/yr | +14.2%/yr | +7.0% |
| ETH | +7.4%/yr | +14.3%/yr | +7.0% |
| SOL | +5.0%/yr | +12.3%/yr | +7.3% |
| DOGE | +8.0%/yr | +16.9%/yr | +9.0% |
| AVAX | +4.6%/yr | +8.0%/yr | +3.4% |
| **pooled** | **+6.45%/yr** | **+13.15%/yr** | **+6.71%** |

5,885 asset-days, daily correlation 0.716.

A snapshot of `predictedFundings` had shown no gap at all, because its median is
dominated by both venues sitting at the same default rate. The history is the
honest measurement and it disagrees with the snapshot — which is a reminder that
a point-in-time comparison of a mean-reverting quantity says very little.

**The proxy survives, for a structural reason rather than a lucky one.** Net
funding is `ws*f_short - wl*f_long`, so a constant added to every rate
contributes `gap * (ws - wl)` — the gap times minus the net exposure. Mean signed
net exposure here is −0.03x NAV, so the shift cancels between the legs: about
**+1.3% across the whole run**, against a total return of 783.7%. The venue that
pays more on the shorts charges more on the longs.

Two limits remain, and neither is closed:

- Only five majors were sampled. The book trades alts, whose funding is more
  volatile and whose venue gap may differ.
- **Hyperliquid did not exist before 2023-05-12**, which is 53% of the backtest
  window. For that half there is no substitution to make, at any level of effort.

---

# A learned ranker, and the first out-of-sample signal in the project

## Why a model, and why not the one first proposed

The objection to feeding a model many features was that none of the features had
demonstrated predictive power. That objection was half wrong: an IC screen picks
winners from fifteen candidates and carries its own selection bias, so a
regularised model with honest cross-validation is the *better* filter, not a
premature one.

The objection to an LSTM specifically survives. The candidate features
autocorrelate at 0.98–0.999 at a one-day lag, so there is almost no sequential
variation for a recurrent model to learn, and its advantage over a cross-sectional
learner is exactly sequence structure. Gradient boosting was used instead: same
job, orders of magnitude fewer parameters.

## Does daily sampling give seven times the training data?

Only if the target changes with it.

| sampling / horizon | rows | mean IC | naive t | eff n | honest t |
|---|---|---|---|---|---|
| weekly, 7d target | 338 | −0.1461 | −8.40 | 338 | **−8.40** |
| daily, 7d target | 2,370 | −0.1446 | −21.62 | 339 | **−8.17** |
| daily, 1d target | 2,370 | −0.0994 | −14.35 | 2,370 | **−14.35** |

With a 7-day target, daily sampling inflates the naive t by 2.57× — almost exactly
√7, which is the signature of counting overlapping windows as independent. Deflate
for overlap and the information is unchanged.

With a 1-day target the observations really are independent, and the weaker
per-observation IC is more than repaid by having seven times as many: honest t
−14.35 against −8.40. **Training frequency and trading frequency are separable** —
a ranker can be trained on daily non-overlapping targets and applied at any
rebalance cadence.

## The setup

70,062 rows, 2,364 dates, 215 assets, 16 features. Features rank-normalised
*within each date*, so the model learns ordering rather than calendar level.
Targets are forward return minus that date's cross-sectional mean, because the
book is long/short and predicting the market is the tilt's job. Expanding-window
folds with a purge of one horizon either side of every test block — without the
purge a 7-day target leaks across the boundary, and the results look **better**,
which is how that mistake survives.

## Out-of-sample IC, positive in every fold

| model | mean OOS IC | folds positive | fold-level t |
|---|---|---|---|
| weekly (7d target) | +0.0760 | 6 / 6 | +5.36 |
| daily (1d target) | +0.0511 | 6 / 6 | +7.50 |

For scale: `ret_30_skip_7` has IC ≈ 0, and the channel spread is flat at t = +0.27.
This is the first thing measured in this project with consistent out-of-sample
cross-sectional predictive power.

## The cadence question, answered

Traded only on walk-forward test blocks, 2022-09 to 2026-07:

| | n | return | Sharpe | maxDD | t | turnover/yr | cost/yr |
|---|---|---|---|---|---|---|---|
| ML weekly (tranched) | 201 | +82.5% | 1.17 | −17.3% | 2.30 | 4,627% | 3.0% |
| **ML daily** | 1,419 | **+381.9%** | **1.92** | −22.4% | 3.78 | 26,453% | 17.2% |
| BTC buy & hold | 200 | +233.3% | 0.90 | −51.8% | | | |

**The daily book pays 14.2%/yr more in fees and returns 4.6× as much.** The
earlier rejection of daily rebalancing was correct for the channel signal and
wrong as a general conclusion: even then daily had more gross alpha (1200.8%
against 918.8% at zero cost) and lost only on turnover. A better predictor tips it.

## Why this is not yet a recommendation

| | break-even vs zero | break-even vs BTC | assumed |
|---|---|---|---|
| daily | 21.8bps | **10.1bps** | 6.5bps |
| weekly | 40.2bps | — | 6.5bps |

The daily book beats the benchmark only below about 10bps all-in, a margin of
1.5×. The weekly channel book had 14×. The result has moved from resting on the
signal to resting on the execution assumption, and that assumption is weak in a
specific, identifiable way: the 2bps spread is described in config as *"a
placeholder for liquid majors"*, this book trades alts at a **1.1-day** mean
holding period, and no impact term is modelled at all. At a 5bps spread the daily
advantage over buy-and-hold disappears.

**The next measurement is realised spread on the assets actually traded**, not
another model. Until that exists, the defensible claim is that the ranking has
out-of-sample content — which is new and worth having — and not that the daily
cadence is profitable.

`lightgbm` and `numpy` are installed in the venv for research and deliberately
NOT added to `pyproject.toml`. Nothing enters the shipped dependency set before
the thing it enables has passed a gate.
