# The Phase 1 research harness

These 77 scripts produced every number in `docs/phase-1-findings.md` and the
record in `docs/research/backtest.json`. They were written and run in Claude Code
sessions against a scratch directory, and none of them were ever committed — the
findings were written up, the code was not. When the Python planner was migrated
to Rust the scratch directory went with it, and the strategy's entire provenance
existed only as prose.

They were recovered by replaying the tool calls out of the session transcript
(`~/.claude/projects/.../7796ca66-*.jsonl`) and are committed here verbatim.

## They are an archive, not a pipeline

Nothing here runs as-is. Absolute paths point at a scratch directory that no
longer exists, and they import the deleted `planner` package. To run one, restore
the Python planner from git (`git show c7b3465^:planner/...`) into a worktree and
repoint the paths — that is how the reproduction described below was done.

The production path is Rust: `crypto-portfolio` plus `bin/walk-forward.sh`.

## What they are worth

They are the only surviving definition of the research. Recovering them settled
three questions that prose could not:

**The +875% was reproducible.** `ml_record.py` is the script that produced
`backtest.json`. Re-running the chain — `hourly_features.py` → `dataset.py` →
merge → `ml_record.py` — returned +760.7% at Sharpe 2.54 against the recorded
+875.5% at 2.70, matching on drawdown (−16.7% vs −17.3%), effective N (12.6 vs
12.7), turnover (98% vs 97.5%) and name counts. The residual is fit
stochasticity. It was a real backtest, not a paste.

**Its fold structure was honest.** `ml_record.py` retrains at every expanding
fold boundary and scores only forward. An earlier theory that the number came
from a single model scoring its own training window was wrong.

**The number was still false, because of one character.** `dataset.py` computed
the funding features over a window running *forward* from the decision date:

    sum(t.get(day + timedelta(days=k), 0.0) for k in range(n))

`funding_7d`, `funding_30d`, `funding_chg` and `funding_z` were therefore built
from realised funding on days that had not happened yet. Funding tracks
contemporaneous price pressure, so next-30-day funding is close to a smeared copy
of the target. Flip that `+k` to `-k`, change nothing else, and the same chain
returns **+134.9% at Sharpe 1.05**.

Everything downstream of the `ds4` dataset inherits the leak, including the LSTM
result (IC +0.094) and the 50/50 blend (+890%). Those need re-measuring before
they are believed.

## Map

| script | what it does |
|---|---|
| `dataset.py` | the daily feature block and the leaky funding windows |
| `hourly_features.py` | 30 hourly path/microstructure features |
| `dataset3.py` | hourly decision rows with the 1h-lagged 24h target |
| `ml_record.py` | **the script that produced `backtest.json`** |
| `sizing.py` | the expectancy-per-unit-risk study (Sharpe 2.19 → 2.70) |
| `null_ml.py` | Null A and Null B |
| `lstm*.py` | the LSTM: broken listwise loss, Pearson fix, ablations, blend |
| `capacity.py`, `impact.py` | capacity and market-impact studies |
| `cadence*.py`, `tranche.py` | the rebalance-frequency sweep |
| `icscreen.py`, `fastfeat.py` | feature IC screens |

The Rust port of the surviving feature definitions is in `features-crypto`; see
`FundingWindow` there for the leak, which is reproducible on purpose behind
`--leaky-funding-diagnostic` so the two implementations can be compared.
