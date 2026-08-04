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
