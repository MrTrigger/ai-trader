# Correct the Lighter historical baseline

- [x] Read the research workflow and Principles.
- [x] Identify the mismatch: broad historical selection, static current membership,
  and filtering after ranking do not reproduce venue-first top-30 selection.
- [x] Collect dated Lighter metadata, native daily candles and verified funding.
  Include inactive contracts. Check retention, timestamp units and sign/scale.
- [x] Rebuild each daily universe before ranking using only information available
  before that decision. Exclude pre-listing contracts and disclose any incomplete
  delisting history. Preserve external signal history used by the live strategy.
- [x] Replay the fixed fold-9 ranker over November 27, 2025 through July 30, 2026,
  using the deployed portfolio limits, $100,000, and matched execution scenarios.
  Use native valuation/funding if complete; missing coverage must fail a native
  venue claim rather than silently substitute another exchange.
- [x] Verify results, causal availability and coverage gates; document corrections.

Done means a rerunnable baseline whose universe, dates, model cutoff, data sources
and execution assumptions are explicit and checked against raw evidence. A native
data gap is an incomplete native replay, not a zero return or a waived check.
No additional model optimization or policy promotion in this correction.

Root owns historical-universe generation and replay integration. The isolated
data worker owns raw public API discovery and downloading. All existing source
data and prior experiment outputs remain immutable. No credentials or orders.

Completed: 274 native daily pages, 364 hourly funding pages, 246 causal venue-first
universes. Native central Sharpe -1.410784, return -10.94%, maximum drawdown17.53%.
Matched external execution proxy Sharpe -0.290667, return -4.13%. All117Rusttests
pass, every held funding hour is present, every financial step reconciles, and
independent review found no remaining blocker. The corrected baseline is negative;
neither previous broad nor static-membership results justify a policy promotion.
