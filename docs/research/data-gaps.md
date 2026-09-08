# Data and evidence inventory, 2026-09-08

The existing Stockholm collection does not supply a long, filing-dated quarterly
fundamental panel or validated prices and terminal outcomes for every inactive
listing. More model searches cannot fill those omissions.

The local authority-data directory contains listing history, equity notices,
report announcements, report attachments, annual filings, short-position reports,
insider reports and macro series. Their presence establishes collection, not
complete coverage or causal joins.

| Saved artifact | Observed coverage | Remaining requirement |
| --- | --- | --- |
| `var/stockholm/authority-data/nasdaq-financial-report-attachments/latest-financial-report-attachments.json` | 5,000 documents requested from 41,077 available PDF URLs; snapshot dated 2026-08-13 | Table extraction and conservative issuer, reporting-period and publication-time joins. The collector retains failures and excludes spreadsheet and XBRL attachments. |
| `var/stockholm/authority-data/esef-sweden/latest-annual-filings.json` | 1,400 annual filings from 1,415 seen; snapshot dated 2026-08-11 | Longer quarterly history. The manifest explicitly says ESEF does not supply it. Repository ingestion time can lag issuer publication. |
| `var/stockholm/authority-data/skatteverket-equity-history/latest-listing-history.json` and Nasdaq equity notices | Saved local listing and notice records | Validate stable listing identity, inactive price coverage and terminal outcomes together. |
| `var/stockholm/ib-borrow-snapshots` and existing historical fee archives | Borrow observations already collected | Historical locate quantity remains unavailable in the documented IB data contract. Fee observations alone do not prove a historical short could be opened. |

The [Stockholm design](../stockholm-portfolio-design.md) already defines
`collect-eodhd-delisted`, a filing-date quarterly fundamentals collector and
`fs-rust-stockholm-15`. The [research study](../stockholm-reward-loss-study.md)
records that those licensed paths had not run because `EODHD_API_TOKEN` was
absent. The implementation checked credential presence without displaying a
value: no EODHD token is available in the process environment or `.env`.
No purchase or provider request was made. The next collection must validate inactive Swedish listings and
as-known filing coverage before a new model comparison uses it.

The NQ evidence is available in the sibling repository at
`../trading-journal/backtest/lab/sleeve_screens.py`. Round 13 records four sleeves,
modern-era Sharpe 1.0 and maximum drawdown of $70.3k. It also records daily
correlation of 0.83 between the breakout and pullback legs, a distinction from
cross-asset diversification. Those are ledger claims, not a new reproduction.

The nearby `../trading-journal/docs/research-plan.md` describes the historical
research data. The local `backtest/data/bars` tree contains MNQ bar manifests.
The new Rust `futures-bot research-replay` exports daily NAV without records
database access. It reproduced the committed parity fixture exactly and supplies
150 common calendar days, March 3 through July 30, 2026. Session P&L is stamped
at the following UTC midnight. The export records the capital denominator,
NQ point value, slippage, commission, sleeve totals and missing-date policy.

This short sample cannot reproduce the modern-era research ledger. The sibling
five-minute archive spans 2021–2026, but the shared CME reader currently exposes
daily aggregates rather than a raw five-minute replay feed. Extending that
adapter remains necessary for the longer comparison. The current runtime also
retroactively closes two early-ending sessions at their last observed bar.
That calendar assumption, intraday margin requirements and market impact remain
unverified. Paired Monte Carlo reports retain these execution limits.
