# Python-to-Rust backtest parity

Verified on 2026-08-07 against the retired Python planner at Git revision
`01347b5c4eee728bc3713878b3aac890220596c1`.

Both implementations used the same `config/default.toml`, local bar store,
point-in-time universe snapshots, initial cash of 100,000, and slippage
multiple of 1. The replay window was 2019-10-01 through 2026-08-01.

| Result | Python | Rust | Absolute difference |
|---|---:|---:|---:|
| Rebalances | 357 | 357 | 0 |
| Rejected plans | 37 | 37 | 0 |
| Total return | -0.4706032490557203487255547154 | -0.4706032490557203487255547133 | 2.1e-27 |
| CAGR | -0.08894918287931675 | -0.08894918287931675 | 0 |
| Volatility | 0.7273314616087745 | 0.7273314616087748 | 3e-16 |
| Sharpe | 0.23669873125643473 | 0.23669873125643417 | 5.6e-16 |
| Max drawdown | -0.9326805899552599793214631513 | -0.932680589955259979321463151 | 3e-28 |
| Turnover per rebalance | 0.1511707573686203822551005290 | 0.1511707573686203822551005289 | 1e-28 |
| Cost drag (bps) | 242.8558217126886440928189995 | 242.8558217126886440928190000 | 5e-25 |

The normalized comparison found no missing dates, fill differences, status
differences, or material NAV differences. The maximum absolute NAV difference
was 2.6e-22, caused by insignificant Decimal accumulation order.

The first Rust replay exposed a real migration error. When the turnover budget
bound, Rust prioritized reductions before increases; Python selected the
largest absolute portfolio drifts first, then sorted selected orders into safe
execution order. The difference first changed fills on 2026-01-13 and grew to
a 3.77% NAV difference. Rust now follows the original selection policy, with a
focused regression test covering the distinction.

The configured signal is `placeholder_equal_long`, which explicitly claims no
edge. This comparison establishes implementation parity, not strategy quality
or profitability; Rust is intentionally neither better nor worse than Python
on this replay.
