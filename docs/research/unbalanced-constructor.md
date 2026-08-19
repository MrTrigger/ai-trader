# The ranked list without a manufactured balance — measured

Question (2026-08-19): `risk_adjusted` selects the top 24 by |expected
return| across both sides and then hands **each side exactly half the gross**.
On a 20-long/4-short day that concentrates 0.40 of NAV in the four names the
model liked least. The user's stated intent — buy the best of the ranked list,
do not manufacture the other side — is the opposite. So a second constructor,
`risk_adjusted_unbalanced`, was written: same selection, same edge/volatility
sizing, but jointly across both sides to the gross target with no per-side
split; net exposure is whatever the list says.

Nine folds, `bin/replay-folds.sh`, same per-fold models, same binary, funding
charged, 1× slippage. Only `constructor` differs; `max_net_exposure` unset in
both so the unbalanced book is seen unconstrained.

| fold | window | Sharpe balanced | Sharpe unbalanced | Δ | maxDD bal | maxDD unb | realized net p10 / p50 / p90 (unb) |
|---:|---|---:|---:|---:|---:|---:|---|
| 1 | 2020-09..2021-05 | 5.64 | 3.58 | −2.05 | −0.070 | −0.107 | −0.24 / −0.03 / +0.14 |
| 2 | 2021-05..2022-01 | 2.83 | 1.59 | −1.24 | −0.149 | −0.132 | −0.23 / −0.05 / +0.09 |
| 3 | 2022-01..2022-08 | 2.02 | 1.14 | −0.88 | −0.117 | −0.104 | −0.08 / +0.06 / +0.22 |
| 4 | 2022-08..2023-04 | 1.80 | 2.03 | +0.23 | −0.110 | −0.099 | −0.10 / +0.02 / +0.17 |
| 5 | 2023-04..2023-12 | 1.86 | 1.78 | −0.08 | −0.141 | −0.119 | −0.07 / +0.05 / +0.19 |
| 6 | 2023-12..2024-08 | 0.28 | 0.43 | +0.15 | −0.114 | −0.154 | −0.08 / +0.05 / +0.20 |
| 7 | 2024-08..2025-04 | 2.17 | 2.22 | +0.05 | −0.089 | −0.086 | −0.13 / +0.02 / +0.17 |
| 8 | 2025-04..2025-11 | 2.41 | 2.32 | −0.09 | −0.074 | −0.101 | −0.15 / +0.01 / +0.15 |
| 9 | 2025-11..2026-07 | 0.91 | 0.59 | −0.31 | −0.174 | −0.182 | −0.13 / +0.03 / +0.17 |

Mean Sharpe **2.21 → 1.74**; compounded +1450% → +864%; 9/9 folds positive
either way; turnover unchanged. `crypto-portfolio compare` (block bootstrap,
2,133 steps, 20-day blocks): **observed delta −0.45, 90% interval
[−0.80, −0.14], variant ahead in 1% of resamples.** The interval excludes
zero by a wide margin — this is not the ±0.5 noise the turnover change lived
inside.

Two things the folds also say:

- **The gap is the early sample.** Folds 1–3 (2020-09..2022-08) carry a mean
  delta of −1.39; folds 4–9 (2022-08..2026-07) a mean of −0.01 with a spread
  of ±0.2. In the regime the bot will actually trade, the two constructions
  are indistinguishable; the balanced one won the 2020–22 sample decisively.
- **The unrestrained ranked list is not very unbalanced.** Realized net over
  all 2,142 rebalances: p5 −0.19, median +0.02, p95 +0.21, extremes −0.44 /
  +0.41. The list itself lands near neutral most days; what the balanced
  constructor changes is not the *direction* of the book but the
  *concentration* — it puts half the gross into the few names on the thin
  side, and over 2020–22 that was where the money was.

Reading. The intuition that a manufactured balance "defeats the purpose of
choosing the top ones" is right about the mechanics and wrong about the
result: on this record the concentrated thin side has been the profitable
one, and letting the list float costs half a Sharpe over the full sample and
nothing measurable since mid-2022. Neither construction was tuned to these
folds — the balanced one was inherited from the recovered harness, the
unbalanced one is the obvious alternative written once — so this is a fair
comparison, and it is one observation of nine folds, not a law.

Decision: not made here. The frozen config keeps `risk_adjusted`.
`risk_adjusted_unbalanced` is available by name; if it is ever adopted,
`max_net_exposure` stops being a drift band around zero and becomes the beta
cap the book is allowed to run at (the unconstrained list reached ±0.44), and
that number is a risk-appetite decision to be written down first.
