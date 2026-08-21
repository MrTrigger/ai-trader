# Directional round 2 — research synthesis (2026-08-21)

Four independent surveys (SOTA models/features; practitioner implementations;
data sources — endpoints live-verified; horizon evidence), ~50 primary
sources, full PDFs of the four key papers saved to the session scratchpad.
This document fixes what the round-2 design may use. Round 1 (naive daily
TSMOM, Sharpe 0.198, 2022 negative) is the baseline.

## 1. The horizon question (user's question, answered)

Composite Sharpe-vs-horizon curve from every direct comparison found
(Dobrynskaya's formation×holding grid; Kang & Ryu's continuous-speed BTC
study; Fieberg et al. JFQA holding-extension table; Borgards' 5m/1h/1d
prevalence; Shen et al.'s breakeven table; Liu–Tsyvinski):

| holding bucket | expected NET Sharpe (liquid names, VIP0 costs) | confidence |
|---|---|---|
| minutes | negative | measured ourselves (runs 4–6) |
| 1–6 h | ≈ 0 (−0.3..+0.2) | high — every gross edge is 3–15 bp/trade vs 5–25 bp costs |
| 6–24 h | 0..+0.3 | low — overnight drift real but migrated post-ETF, gross-only sources |
| 1–3 d | +0.1..+0.4 | low — least-documented region; survives only with sparse trading |
| 3–10 d | +0.3..+0.6 | moderate — costs stop binding at ~3–5-day holds |
| 10–30 d | +0.3..+0.6 | moderate-high — TSMOM-1M net 0.65 on 2022–24 (costed); slow beats fast on bear-inclusive samples |
| > 4–6 wk | reversal sets in | grid-replicated |

**Sweet spot: ~1 week to ~1 month effective holding.** Sub-12h has zero
published net-positive results; sub-hour is closed by our own record. The
"best horizon" is therefore not a knob to sweep at the minutes end — the
round-2 versions live at days-to-weeks, expressed through signal speed and
exit style rather than a fixed holding period.

## 2. Why round 1 scored 0.2 when the credible claims are 1.0–1.5

Three implementation differences, ranked by evidence (the first is a
controlled ablation by the claimants themselves):

1. **Long-flat, not long-short.** Zarattini et al. (full PDF): identical
   signals and sizing, top-20 net — long-only Sharpe 1.57 vs long-short
   1.24; Strix Leviathan sat in cash through the winters (shorts ≤20% NAV);
   Man/Harvey's BTC trend was "mostly long". The 1.5s earn by NOT LOSING in
   bears (MDD 11–19% vs 80%), not by shorting them. Our −15.6% fold was a
   short into the Jan-23 V that none of them would have held.
2. **Breakout entries with trailing-stop exits to cash, ensemble of speeds
   5–360d (4 of 9 members ≤30d).** Exits fire weeks before a sign flip
   does; Quantpedia's hostile 2022–24 retest: breakouts survived, dip-buying
   died. (Nuance from the horizon survey: for *symmetric sign* TSMOM, slow
   beats fast post-2021 — asymmetry of entry/exit is what makes fast
   tolerable.)
3. **Cost-aware machinery:** per-asset vol sizing with SLOW vol (90–180d),
   leverage cap, no-trade band (rebalance only on >20% drift), monthly
   liquidity-refreshed universe. Worth ~0.2–0.4 combined by their ablations.

**Honest caveat (all agents agree): nobody demonstrates Sharpe ≥ 1 on
2022–2024 alone.** Full-sample 1.5s lean on 2015–17/2020–21 bulls held
long-only. Round 2's realistic target is 0.5–1.0.

## 3. What the SOTA survey adds (and rules out)

Ruled out by replicated negative evidence: daily LSTM/transformer direction,
HMM regime timing (−1.65 OOS in the one honest paper), Fear&Greed / Google
Trends, macro faster than monthly, funding-based cross-sectional screens on
large caps. Ceiling for BTC/ETH timing in the honest literature: ~0.5–0.6
net. Promising and cheap: vol-managed sizing (two 2025 crypto papers),
breakout construction, Garg–Goulding–Harvey slow+fast turning-point blend
(JFE-published mechanism, zero crypto replication — a genuine open test).
Data bets with their own decay risk: stablecoin exchange flows (4h–1d,
paper sample ends 2023), ETF flows (15-month single-regime sample).

## 4. Data (endpoints live-verified 2026-08-21)

Usable for trained versions, all free, point-in-time acceptable:
Binance UM funding (2019-09+, in-repo for 442 assets already) and
**metrics/OI+positioning daily zips since 2020-09-01** (we had only pulled
2024-08+; backfillable); BitMEX funding 2016+; Deribit DVOL daily 2021-03+
(BTC+ETH, free API); DeFiLlama stablecoin aggregate 2017-11+ (one call);
CoinMetrics community on-chain (BTC 2009+, ETH 2015+); FRED/ALFRED macro;
Farside ETF flows 2024-01+ (scrapeable table); Wikipedia pageviews 2015+
(immutable); CFTC COT weekly 2017-12+. Excluded: Google Trends (rescaled
per request — backtest-dangerous, tooling dead), Glassnode free (no API),
CoinGecko free (365d cap), OKX history (3-month retention), Twitter/Reddit
(dead/paywalled).

## 5. Sources

Man/Harvey JPM 2022 (P164 PDF); Man "In Crypto We Trend" 2024; Zarattini–
Pagani–Barbon SSRN 5209907 (full PDF); Strix Leviathan HFJ profile; Carver
(no crypto publications; practice pattern only); Quantpedia 4955617 +
multi-timeframe study; Robot Wealth crypto alphas; Kang & Ryu Risk Mgmt
2026; Dobrynskaya SSRN 3913263; Fieberg et al. JFQA 2024 (PDF); Borgards
NAJEF 2021 (PDF); Shen–Urquhart–Wang FR 2022 (PDF, breakeven table); Liu–
Tsyvinski RFS 2021; Han–Kang–Ryu SSRN 4675565; arXiv 2602.11708 (TSMOM-1M
net 0.65, 2022–24); arXiv 2606.00060 (cost-aware filter, 27-fold WF); Chi
et al. arXiv 2411.06327 (stablecoin flows); Lim SSRN 6592830 (ETF flows);
Schmeling–Schrimpf–Todorov BIS 1087; Borri et al. arXiv 2510.14435 (carry
decay); Garg et al. JFE 2023 (turning points); Moreira–Muir JF 2017 +
Cederburg et al. JFE 2020 (vol-managed OOS caution); Baquero arXiv
2606.00071 (negative synthesis); Grayscale/VanEck/Concretum flagged vendor.
