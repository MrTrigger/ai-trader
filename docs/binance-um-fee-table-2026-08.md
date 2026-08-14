# Binance USDⓈ-M futures fee schedule snapshot (2026-08)

Retrieved: 2026-08-14 (UTC, this session). This is a snapshot for the gate
protocol's fee fixed-point rule (`docs/scalper-research.md`, Amendment 1) —
not a live-updated reference. Re-fetch before any future gate run that needs
a tier this snapshot doesn't cover (see "Gap" below).

## What's verified (fetched directly from a binance.com page, this session)

Source: `https://www.binance.com/en/support/faq/binance-futures-fee-structure-fee-calculations-360033544231`
(also mirrored at the `en-IN` locale, same content), fetched 2026-08-14. This
static FAQ page — unlike the interactive fee-rate page below — returned real
content to an unauthenticated fetch.

| Tier | Maker | Taker |
|---|---|---|
| Regular user (VIP0) | 0.0200% (2.00 bps) | 0.0500% (5.00 bps) |

The page also states, verbatim in substance: users receive a **10% discount**
on standard USDⓈ-M Futures trading fees when fees are paid in BNB (BNB held
in / deducted from the USDⓈ-M Futures wallet).

**Derived** (arithmetic on the two verified numbers above, not itself a
quoted figure):

| Tier | Maker | Taker |
|---|---|---|
| VIP0 + BNB discount | 0.0180% (1.80 bps) | 0.0450% (4.50 bps) |

This VIP0+BNB taker figure (**4.50 bps**) is the number the protocol's run 1
charges.

## What's cited from secondary sources (NOT independently verified)

The interactive schedule at `https://www.binance.com/en/fee/futureFee` — the
page that would normally carry the full VIP0-VIP9 table — is entirely
client-rendered and reads "No records found" without an authenticated
session; this held for a direct fetch, multiple user-agent strings including
a Googlebot UA, and a Wayback Machine capture from 2026-08-07 (which also
captured the unrendered "No records" shell). No `bapi.binance.com`-style
JSON endpoint guess for the fee table returned data (all 404). See the fetch
log below.

Falling back to secondary aggregator sites for the top tier only, since it's
the one figure that was consistent across independent sources:

| Tier | Maker | Taker | Qualification (as cited) |
|---|---|---|---|
| VIP9 | 0.0000% (0 bps) | 0.0170% (1.70 bps) | ≥$30,000,000,000 30-day futures volume (or ≥$4,000,000,000 30-day spot volume) **and** 5,500 BNB balance |

Cross-confirmed identically across 4 independent secondary sites
(bitdegree.org, datawallet.com, binancemakertakerfee.org,
makertakerbinance.org). None of these is Binance itself, so this row is
cited, not verified — treat it as directional only.

## Gap: VIP1 through VIP8 not transcribed

I did not transcribe the eight intermediate tiers. The secondary sources for
them didn't just vary in precision, they actively disagreed, and at least
one repeated figure is internally impossible:

- Multiple sites (tradersunion.com, a Bitget-hosted "2026 guide", others)
  state VIP1 = **0.09% maker / 0.10% taker** — higher than the Regular-user
  rate of 0.02%/0.05% verified above. A volume tier can't charge *more* than
  the no-tier rate, so this is almost certainly contamination (possibly a
  transcription of an unrelated fee schedule, or an AI-content-mill error
  propagating across sites that cite each other rather than Binance).
- The 30-day-volume and BNB-balance thresholds quoted for VIP1 alone ranged
  from ($250k / 25 BNB) to ($1M / 25 BNB) to ($5M / 5 BNB) to ($15M / 25 BNB,
  described by one source as since lowered by a "March 2026 update") — no
  two sources agreed, and none is a Binance-authored page.

Per instruction, I did not invent numbers to fill this table. **Implication
for the protocol:** the fee fixed-point rule's run 1 (VIP0+BNB taker, fully
covered above) can proceed as pre-registered. If a real gate run's
`projected_30d_volume_usd` maps to anything other than VIP0 or VIP9, the
operator must re-fetch Binance's fee schedule from an authenticated account
session before performing the rule's one allowed re-run — this snapshot
cannot resolve VIP1-VIP8.

## Fetch log

- `https://www.binance.com/en/fee/futureFee` — HTTP 202, empty body (direct
  curl, default UA); HTTP 202 empty body (Googlebot UA); WebFetch tool:
  "No records found", login required. Wayback Machine capture
  `20260807201302` of the same URL: HTTP 200, 203,748 bytes, but the captured
  HTML is the same unrendered client shell (0 occurrences of `VIP0`-`VIP9`,
  "No records" present).
- `https://www.binance.com/bapi/futures/v1/public/future/common/fee-rate` — 404.
- `https://www.binance.com/bapi/futures/v1/public/future/vip/level` — 404.
- `https://www.binance.com/bapi/futures/v1/public/future/fee/vip/level` — 404.
- `https://www.binance.com/bapi/composite/v1/public/marketing/tradingFee/getFeeRate` — 404.
- `https://www.binance.com/en/support/faq/binance-futures-fee-structure-fee-calculations-360033544231` — HTTP 200, real content, used above.
- `https://www.binance.com/en-IN/support/faq/binance-futures-fee-structure-fee-calculations-360033544231` — HTTP 200, same content.
- `https://www.binance.com/en/support/announcement/binance-futures-fee-update-improved-fee-structure-vip-discounts-360037673592` — HTTP 200, no fee table (nav/footer shell only).
- Secondary sites checked for the VIP1-VIP9 table: tradersunion.com (403 to
  WebFetch), bitdegree.org (200, table incomplete for futures), plus
  WebSearch synthesis over ~10 more SEO/aggregator pages — see "Gap" above.
