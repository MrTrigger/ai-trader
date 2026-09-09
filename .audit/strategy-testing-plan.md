# Strategy testing implementation

Definition of done: frozen crypto models can be replayed with explicit fill costs and reconciled P&L attribution; deterministic Monte Carlo reports run on saved daily returns; an experiment register records hashes, corrections, trial counts and exposed evaluation dates; one command regenerates the evidence and rejects incomplete inputs. No model is promoted by these diagnostics.

- [x] Read applicable workflow and verification principles; preserve existing selection-pool edits.
- [x] Capture baseline artifacts and declare execution / Monte Carlo assumptions before reruns.
- [x] Implement execution spread, impact, delay and participation scenarios with accounting reconciliation tests.
- [x] Implement block-resampled Monte Carlo with deterministic seeds, drawdown/loss probabilities and paired comparisons.
- [x] Implement experiment register and attribution summaries; inventory data and NQ evidence gaps.
- [x] Run frozen-model replays, validation and Monte Carlo; retain negative findings.
- [x] Review code and evidence; publish report with rerun commands and remaining acquisition constraints.

Execution scenarios are research assumptions, not fitted venue estimates. Preserve flat-cost replay as an explicitly named historical control. Historical current liquidity snapshots cannot be used as point-in-time observations. Monte Carlo uses contiguous daily blocks and is conditional on observed history, not a substitute for an untouched test period.
