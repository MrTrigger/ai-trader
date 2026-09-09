# Selection and trend allocation research

- [x] Read the poteto-mode Principles and the figure-it-out workflow.
- [x] Frame: reproduce the current score-selected baseline; separate selection from directional allocation.
- [x] Design: declare a small matched-cost screen before measuring candidates.
- [x] Loop: test selection/allocation candidates on folds 6–8, then confirm the declared winner on folds 6–9 with stress, delay and paired block bootstrap.
- [x] Loop: test one trained higher-timeframe market signal with chronological labels and a matched no-signal control if the existing training/data paths support it.
- [x] Verify: inspect risk, turnover, long/short attribution, later-date evidence if available, and venue coverage.
- [x] Record results, rejected hypotheses and confidence limits; obtain independent review.

Baseline: score-selected 24-name policy, 0 commission, 2.2 bp half-spread,
original impact/funding/participation settings, $100,000 and daily decisions.
Expected recent pooled Sharpe 1.540502 over 957 days, original folds 6–9.
No cost or capital changes count as a selection improvement.

A development winner must improve pooled Sharpe versus the matched baseline,
retain positive final-fold return, and disclose drawdown/stress/delay regressions.
A reliable improvement additionally needs paired uncertainty excluding zero;
reused historical dates are not an untouched holdout. A failed experiment is a
completed result, not a reason to change its gate after seeing the answer.

Initial scope: at most eight fixed policy hypotheses, one trained market-signal
comparison, screen plus confirmation. Start with existing replay/training tools;
new code is limited to expressing the declared policies and verifying causality.
No production changes or orders. Existing user selection_pool edits stay intact.

Delegate only independent evidence reviews and an isolated training seam.
Root owns the candidate declarations, portfolio implementation, replay and report.
The existing constructor/config/replay interfaces settle the architecture;
no architecture bakeoff is needed for these mechanical additions.

Completed with a negative result. HTF conviction won the screen but failed
four-period confirmation: Sharpe 1.445882 versus 1.540502 control. Static current
Lighter membership also favored control, 0.516018 versus 0.460573. No policy was
promoted. Complete local data after the deployed model's creation is unavailable.
The report records that evidence boundary and the venue-specific research priority.
