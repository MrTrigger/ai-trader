# Improve recent net Sharpe

- [x] Read the figure-it-out principles and inspect the fixed-cost baseline.
- [x] Declare a bounded set of hypotheses and preserve all trial results.
- [x] Support per-scenario frozen configurations with verified provenance.
- [x] Test funding-aware conviction and existing turnover controls on folds 6–8.
- [x] Evaluate the selected rule on fold 9, all recent folds and execution stress.
- [x] Run paired Monte Carlo and compare matched-cost diversification if useful.
- [x] Independently review code, metrics and audit trail; record result and limits.

Primary target: pooled daily Sharpe strictly above 1.5 across folds 6–9 with
the declared impact model and $100,000 initial cash per fold. Require a positive
latest-fold return and disclose cost/delay sensitivity and drawdown. A higher
full-history Sharpe alone does not pass. Select using folds 6–8 first and retain
fold 9 as a confirmation diagnostic; all these dates were previously exposed,
so this is development evidence, not independent out-of-sample validation.

Keep all models and data fixed. Start with a small set of economic hypotheses
covering adverse funding and unnecessary turnover, plus a fixed-weight paired
comparison where both strategies use the same execution model. Do not increase
leverage, arbitrarily reduce recorded costs, drop bad dates or choose a favorable bootstrap
seed to reach the target. If the declared changes fail, record that and use their
attribution to choose the next mechanism rather than sweeping arbitrary values.

Use .audit/sharpe-target.tsv for decisions and a checked-in declaration plus
repeatable runner for each research round. Data provider and live deployment
work remain outside this experiment.

User subsequently proposed Lighter Standard zero-fee execution. Include a
separately named zero-commission counterfactual based on the official fee
schedule, preserving funding and spread/impact. Verify market coverage and
document that existing historical bars, funding and liquidity are not Lighter
history. Treat venue implementation and calibration as a separate requirement.

Completed continuation, 2026-09-08:

- [x] Finish rounds1–3 with all declared results retained.
- [x] Verify Lighter fees, original-market coverage and current depth; replay2.2bp quote sensitivity.
- [x] Correct training cohort causality and move per-risk targets into Rust.
- [x] Fit matched24h/72h models, enforce label embargo, confirm and reject72h.
- [x] Combine score selection and quote-anchored costs; confirmationSharpe1.5405.
- [x] Monte Carlo5/20/60, paired comparisons, exposure/attribution reconciliation and negative provenance tests.
- [x] Final independent review and report handoff.

The numerical development target passes for the combined cost counterfactual.
Actual Lighter and independent future performance remain unverified. No default
portfolio change or live account action is authorized by these research results.
