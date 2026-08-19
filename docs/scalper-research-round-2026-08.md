# Research round, 2026-08-19: what the literature and the industry say about the scalper's battlefield

**Purpose.** After six gate runs (four clean) across three pre-registered
programs, every clean result lands at pooled IC 0.02–0.04 and out-of-sample
Sharpe 0.1–0.8 for a 1-minute-bar Binance UM perp scalper. Before proposing
anything else, this document asks two outside questions: (1) does the
2021–2026 literature support the existence of the edge we have been looking
for — a net-of-cost Sharpe > 2 at 15–60-minute horizons from public data — and
(2) how do the firms that demonstrably make money in crypto make it, and with
what? It then draws the consequence for a $50k–$500k systematic account.

**Method.** Four independent surveys were run on 2026-08-19 (short-horizon
predictability; market-making and execution economics; industry practice and
infrastructure; medium-frequency evidence), ~200 web fetches, primary sources
preferred (arXiv/journals, exchange documentation, SEC/CFTC filings, company
statements). Sources are cited inline; items marked *abstract only* could not
be read in full. Nothing below relies on a vendor's marketing claim without
saying so. Our own record (`docs/scalper-gate-run-{4,5,6}.md`) is treated as
one more data point, not as the conclusion.

---

## 1. Short-horizon predictability: the literature agrees with our record

**What careful studies find at 1–60 minutes on Binance perps.**

- The most rigorous minute-bar study found — Pindza, *Microstructure alpha…*,
  Frontiers in Blockchain 2026 (Binance spot+perps, 1-min bars, 5-min horizon,
  OFI/depth-imbalance/VPIN/Kyle-λ features, purged and embargoed CV) — reports
  OLS out-of-sample R² **1.2%**, LightGBM **negative** (overfits), and net of
  VIP-0 fees + half-spread every long-short variant is deeply negative at
  120–200× daily turnover: "genuine but weak information content… not
  exploitable at standard retail fee levels". Signal concentrates in calm
  regimes and vanishes in stress.
  https://www.frontiersin.org/journals/blockchain/articles/10.3389/fbloc.2026.1811716/full
- Order-flow imbalance explains a lot of *contemporaneous* price change
  (10–37% at 500 ms across venues — Albers et al. 2022, arXiv:2108.09750) but
  the *forecast* R² decays to ~1% by 5 min and is not reported positive at
  15–60 min in any crypto paper found. Contemporaneous impact is often
  mis-cited as predictability. Kolm–Turiel–Westray (Math. Finance 2023,
  equities) put the useful forecast range at "about two average price
  changes" ahead.
- Deep-learning LOB models on crypto (TLOB, arXiv:2502.15757; Jha et al.
  arXiv:2010.01241) report classification F1 at 1–10 s that is near chance by
  10 s, and the authors of TLOB state it is "not sufficiently mature for
  practical deployment"; thresholding labels by the spread collapses
  performance. Briola–Bartolucci–Aste (Quantitative Finance 2025,
  arXiv:2403.09267) is the field's own critique: forecasting power does not
  translate into actionable signals. The only fee-adjusted crypto DL-LOB
  result found (Bieganowski & Ślepaczuk 2026, arXiv:2602.00776, 3-second
  horizon, Binance perps) gives BTC **taker IR ≈ 0.25**; its large alt numbers
  rest on a maker fill model without queue position or adverse selection.
- Hourly BTC ML with 27-fold walk-forward and 10 bps costs (Bysik & Ślepaczuk
  2026, arXiv:2606.00060): naive sign strategies fail at 10 bps; only cost-aware
  filters restore profit in *selected* long-only configurations, Sharpe "above
  one" in a bull sample — largely beta.
- Cross-venue and futures→spot lead-lag exists but at 100 ms–seconds
  (Cosenza & Stalder SSRN 4983566; *Economics Letters* 2026, first 100 ms of
  each second carries disproportionate price discovery; Shynkevich, JFM 2026);
  BTC→altcoin lags are 16–118 s (PeerJ CS 2025). Nothing supports a
  15–60-minute cross-asset edge from public data — consistent with our run 6,
  where BTC tick context took 16–31% of model gain and added no OOS IC.
- Predictability of the tradeable kind lives in illiquid alts, where spread,
  fees and adverse selection are also largest (Bieganowski & Ślepaczuk; Jeon
  2026 arXiv:2607.09230 — OFI adds robust value only for ETH, "isolated
  passes" for BTC; Kurihara & Matsumoto 2026, lag is a liquidity function).
- One real, replicated *periodic* effect exists: the quarter-hour /
  turn-of-the-candle imbalance (Kim & Hansen 2026 arXiv:2607.09426; Shanaev
  et al., Heliyon 2023) — bot-driven, ~1 bp/min at :00/:15/:30/:45, predictive
  over 4–12 h, requiring maker fills to keep.

**Bottom line for §1.** No peer-reviewed or preprint evidence was found for a
net-positive taker Sharpe at 1–60 minutes on liquid Binance perps from public
bar/trade data, let alone Sharpe > 2. Where fee-adjusted profit appears it is
at sub-second horizons, passively, with real infrastructure. Our own numbers
(IC 0.02–0.04; net/trade +5 to +30 bps only after a 1.5×-cost entry filter;
Sharpe < 0.8) are what the literature would predict for exactly this setup.
Six pre-registered runs replicated the published result rather than
contradicting it.

## 2. Execution economics: why the maker path could not rescue it

- Albers, Cucuringu, Howison, Shestopaloff, *The Market Maker's Dilemma*
  (2025, arXiv:2502.18625) is the decisive reference: they judged Binance L2
  data unable to make a maker backtest honest ("accurately tracking an order's
  queue position… is impossible") and instead placed **~400,000 live
  minimum-size maker orders on BTCUSDT-perp** at best-tier fees (maker
  −0.5 bp): naive top-of-book quoting lost ~60% in 3.3 days (Sharpe −109);
  imbalance-following makers −0.47 bp/round trip; back-of-queue fills
  averaged −0.78 bp markout vs −0.06 bp front-of-queue; only a learned
  *reversal* signal earned ~+0.7 bp/round trip at ~327 trades/day at
  minimum size, "likely not scalable". Fills coincide with adverse moves by
  construction (DeLise 2024, arXiv:2407.16527, "the negative drift of a limit
  order fill"); market makers without a short-term alpha are "driven out of
  the market" (Cartea–Jaimungal–Ricci 2014).
- Our Program 2 measured exactly this with the honest fill rule we could
  afford: 89–90% of orders filled, adversely; net/trade fell from +30 bps
  (taker) to +2–6 bps (maker) despite a ~40% cheaper round trip. That is not a
  modelling artefact; it is the documented mechanism.
- Fee arithmetic: retail maker 2.0 bps (1.8 with BNB) vs professional
  liquidity-provider tiers at 0 to −0.5 bp (Binance USDⓈ-M LP Program requires
  ≥$100M/30d), i.e. a 2–2.5 bp/side handicap in a game whose entire naive edge
  is ±0.5–1 bp per round trip. Latency: Binance matches in AWS Tokyo; a Tokyo
  VPS gets ~1–2 ms network RTT (Zenlayer 2026) — the network gap is cheap to
  close, the fee and queue gaps are not.
- **Data correction (verified 2026-08-19).** Binance did publish a UM
  `bookTicker` (best bid/ask change) archive: BTCUSDT from **2023-05-16 to
  2024-03-31**, ~45M rows/day, then discontinued (404 thereafter — the date my
  Amendment 5 probe hit). So a top-of-book queue-model backtest in the style
  of hftbacktest (RiskAverse queue: fill only when traded volume at your price
  exceeds the queue ahead) is possible for ~10.5 months, as an *upper bound*
  (no depth behind the touch, no measured latency, no failed cancels).
  hftbacktest's own docs show results moving materially with the queue model
  and warn to reconcile against live fills before believing any of it.
  Amendment 5's "no tick-level book" statement stands for our 24-month
  record; a shorter maker study is now known to be possible but the live
  evidence above says what it would find.

## 3. Where the professionals' money actually comes from

Across every firm with any disclosure — Wintermute (UK filings, Forbes 2022;
CoinDesk 2026), Jump/Tai Mo Shan (SEC settlement Dec 2024, $123M over 2021
UST peg support in exchange for discounted LUNA), Cumberland/DRW (SEC
complaint Oct 2024, unregistered dealer in >$2B OTC), Flow Traders (public:
FICC incl. crypto 51% of 2024 NTI, ETP market making), B2C2 and Wintermute
(each ~11–12% of Robinhood's transaction revenue via crypto flow rebates,
Robinhood 10-K FY2024), Jane Street (~$1B spot-ETF holdings consistent with
AP/create-redeem arbitrage), Alameda (CFTC complaint: "high-frequency digital
asset arbitrage"; debtors' filings: ~$3.7B lifetime loss) — the documented
P&L drivers are:

1. spread capture at scale with millisecond cross-venue hedging (60+ venues,
   colocation in AWS Tokyo/Singapore/HK, kernel bypass, rebate tiers,
   60,000-orders/min rate limits);
2. internalising broker/OTC/ETF flow (Robinhood, SBI, ETF AP work);
3. token-issuer economics (interest-free token loans plus call options,
   discounted-token deals);
4. crisis and depeg trades (UST 2022);
5. CEX–DEX/MEV arbitrage at industrial scale (three builder-integrated
   searchers took ~90% of $234M by Q1 2025 — arXiv:2507.13023);
6. options market making.

No principal statement, filing or credible report cites minute-scale
directional bar-feature models as a P&L pillar. Jump's Dave Olsen frames the
job as connectivity + capital + "your best prediction of what should the
price be" at quote-update speed; Wintermute's Gaevoy as infrastructure
"expensive to build and difficult to replicate". Margins have compressed hard
since 2022 (Wintermute revenue collapse 2022; ChainCatcher 2025 "too many
monks and too little meat"; Wintermute's stated pivot to >50% non-crypto
revenue by 2027; GSR/Keyrock/B2C2 diversifying into broker-dealer and OTC).
The 2021 "genius quant" narratives were, on the documented record, mostly
deal-structured or flow-internalisation revenue.

**Consequence.** The professionals' edge is speed, queue, rebates, inventory
and flow — none of which a $50–500k, non-colocated, VIP-0 participant has —
and even they are not making money at 1-minute directional prediction. This is
the structural reason our three programs found IC 0.02–0.04: that is what is
left after the fast money has been paid.

## 4. What does have credible evidence for a $50k–$500k account (no colocation)

Evidence quality and *net* Sharpe as reported by the sources (all figures the
sources' own; "gross" flagged):

| Strategy class | Horizon | Evidence | Realistic net Sharpe (single sleeve) | Notes |
|---|---|---|---|---|
| Time-series trend on ~10–20 liquid coins, daily/6h bars, vol-targeted | days–weeks | Medium-high: Man Group *In Crypto We Trend* (2024, costs and shorting modelled; risk-adjusted return peaks at 10–15 coins); Zarattini–Pagani–Barbon *Catching Crypto Trends* (SSRN 5209907, Sharpe >1.5 net, top-20); low-tier academic 0.4–0.6 gross | **0.6–1.2**, multi-year flat/negative spells (2022–23) | OHLCV only; simple; the best-supported slow edge |
| Perp funding carry (short perp / long spot; cross-venue funding) | 8h–weeks | High but decaying: He–Manela–Ross–von Wachter (arXiv:2212.06888) BTC 6.4%/yr Sharpe 1.8, ETH 2.6 with retail costs, in market 15–35% of the time, spreads narrowing ~11%/yr; BIS WP 1087: spot-ETF launch cut carry ~3pp; Borri–Liu–Tsyvinski–Wu 2025: Sharpe 4 in 2024, **negative in 2025** | **0.5–1.5, episodic**; fat left tail (2022), ADL/liquidation risk in the perp leg | A yield sleeve, not an alpha engine |
| Large-cap cross-sectional momentum, weekly rebalance | 1–4 wk | Medium: Liu–Tsyvinski–Wu (J. Finance 2022); still ~+2%/wk long-short post-2020 gross (arXiv:2510.14435); large-cap only (Cong et al.; Fičura & Colak) | 0.3–0.8 on top-20/30 perps; crash-prone | Overlaps with trend |
| Time-of-day / weekly seasonality (21–23 UTC, Monday Asia open) | hours–1 day | Medium, replicated by Quantpedia and Concretum (gross Sharpe ~1.6 intraday trend ensemble) — all gross | ~0–0.5 stand-alone after two taker round trips/day | Use as a filter/overlay, not a strategy |
| Short-term reversal / liquidity provision | 1h–1d | Medium but wrong universe: premium sits in small/illiquid pairs (Bianchi et al., JBF 2022/2025) | ≈0 as taker on liquid perps | Maker in thin pairs = adverse-selection risk |
| Cross-sectional ML composites on liquid perps | daily | Low post-2022: Junior 2026 (SSRN 6701738) net Sharpe −3 on 10 Binance perps | ≤0 on tradeable universe | |
| Minute-scale directional (our Programs 1–3) | 15–60 min | Negative: §1 above and our six runs | 0.1–0.8 | Closed |
| Top-of-book maker on majors | seconds | Negative live evidence (Albers et al.), §2 | negative at retail fees/latency | |
| Liquidation-cascade backstop (e.g. Hyperliquid HLP) | tail events | Documented returns (HLP ~15–30% APR, +$41.5M on 2025-10-10) | event-driven, not a daily edge | Different risk class |
| CEX–DEX arb / MEV | ms | Closed to small entrants on mainnet (90% to 3 searchers) | — | |

Marketing claims of "2+ Sharpe" daily crypto factor portfolios (e.g. Unravel/
Aperiodic 2025) are gross, non-walk-forward, and their own footnotes concede
the live portfolio is below 2. Realistic combined net Sharpe from the
positively-evidenced classes at these horizons is **~0.8–1.5** — meaningful,
not 2–6.

## 5. Recommendation

1. **Close the 1-minute scalper line as a research direction, not just as
   three programs.** The gate did its job: three programs, one frozen 24-month
   record, no tuning, and an outcome that the independent literature and the
   industry's own disclosures predict. Continuing to search for a Sharpe-2
   taker/maker scalper on Binance perps from free data would now be
   contradicting evidence rather than testing a hypothesis. The one
   closure-named direction never run (a different label) addresses a
   constraint that isn't binding (80% of h15 exits are time exits).

2. **If the goal is a systematic crypto book on this capital, the
   evidence-supported design is medium-frequency and modest:** a
   vol-targeted trend/momentum sleeve on ~10–20 liquid perps at daily (or
   6-hour) bars, plus an opportunistic funding-carry sleeve that is only in
   the market when the premium clears a pre-registered threshold. Expected
   net Sharpe from the sources: ~0.8–1.5 combined; expected 15–35% annual
   return at 15–20% vol targeting, with multi-quarter drawdowns and negative
   years. That is a different project (different horizon, different gate,
   different risk), not an amendment to the scalper protocol — and its gate
   should be pre-registered before its first number exactly as this one's
   was. It can reuse most of what exists: the Rust perp store and puller,
   fold/gate machinery, the funding archive, the cost model (with far lower
   turnover), and the discipline.

3. **What would change §1–2's conclusion (and what it would cost):** paid
   full-depth L2 history (Tardis ~$150–600/month; Kaiko ~$10–55k/yr) plus a
   Tokyo VPS plus live minimum-size probing — the Albers et al. path. The
   best public evidence says a sophisticated version of that barely breaks
   even at minimum size with pro fees and zero latency. I do not recommend
   spending on it.

4. **On the disappointment.** The honest reading of the six runs is not "the
   engineering could not find the edge" — the engineering found, cleanly and
   without fooling itself, exactly the small edge the field's best studies
   find at this horizon, and it kept a leak-contaminated Sharpe-17 result from
   ever being believed. The target — Sharpe > 2, 1-minute horizon, taker or
   honest maker, free data, no colocation — is not one that the literature
   or the industry supports for anyone. Moving the horizon out to where the
   evidence is (days, not minutes) and the return expectation down to what it
   supports (~1, not 2+) is the change that would make the next program
   winnable.

## 6. Source list (as consulted; abstract-only items marked)

Short-horizon: arXiv 1808.03668; 2010.01241; 2502.15757; 2403.09267;
2602.00776; 2606.25986; SSRN 3900141; Digital Finance 1:191 (2019);
arXiv 2108.09750; Frontiers Blockchain 10.3389/fbloc.2026.1811716;
arXiv 2607.09230; 2607.09426; SSRN 5020002; SSRN 6938742 (abstract);
arXiv 2112.13213; SSRN 4983566 (abstract); Economics Letters
S016517652600220X (abstract); JFM 46(5) 2026; arXiv 2506.08718; APFM
10.1007/s10690-026-09589-z; arXiv 2606.00060; 2607.04958; SSRN 4814346;
PeerJ CS 3810.
Execution: Avellaneda–Stoikov 2008; arXiv 1105.3115; 1605.01862; SSRN
1964781; arXiv 2004.06985; 2207.09951; 2410.14504; 2204.13265; 2605.06405;
2502.18625; SSRN 4677989; arXiv 2407.16527; 2403.02572; 2307.04863; SSRN
3991930; RIBAF 81 (2026); hftbacktest docs (order fill, queue models,
live-discrepancy, MM program); Binance fee/LP-program/rate-limit pages;
Zenlayer 2026; data.binance.vision bucket listing (bookTicker 2023-05-16 →
2024-03-31, verified).
Industry: Forbes 2022-12-20; CoinDesk 2026-08-12; arXiv 2507.13023;
Wintermute OTC review 2024; Robinhood 10-K FY2024; SEC press release
2024-169 and complaint; DL News/CryptoSlate on Tai Mo Shan settlement;
CFTC FTX complaint; Flow Traders 4Q24; ChainCatcher 2025; Ledger Insights
2025; TP ICAP 2022; arXiv 2510.14435; 2608.03616; CCXT/Everstrike/Robot
Wealth practitioner posts (flagged as testimony).
Medium-frequency: SSRN 3379131 / NBER w25882; arXiv 2510.14435; SSRN
3985631; SSRN 3239670; JBF S0378426625000317; SSRN 4378429; SSRN 4601972;
IRFA S1057521924007415 (abstract); QF 23(12) 2023; FMPM 2025; SSRN 6701738;
CMU carry WP 2022; arXiv 2212.06888; BIS WP 1087; SSRN 5036933; Quantpedia
seasonality; SSRN 4955617 (abstract); Concretum 2025; Heliyon PMC10015199;
NAJEF S1062940822000833 (abstract); SSRN 3974583; JEF S0927539823000956
(abstract); Man Group *In Crypto We Trend* 2024; SSRN 5209907; arXiv
2009.12155; 2602.11708 (flagged in-sample); Presto Research 2024; Robot
Wealth carry/turnover posts.
