# Lighter public market data

`Lighter::new(base)` creates a public REST client. `MAINNET` is
`https://mainnet.zklighter.elliot.ai`. The constructor accepts an HTTP or HTTPS
origin, with no credentials, path, query, or fragment.

The adapter implements `VenueAdapter::get_markets`,
`VenueAdapter::get_order_book`, and `PriceSource::mark_price`.
`marks()` returns a `BTreeMap<String, Decimal>` from one bulk response.
Every account query, order submission, and cancellation returns `Unsupported`.
The crate contains no signer, authentication, or POST request implementation.

## Data and constraints

[orderBookDetails](https://apidocs.lighter.xyz/reference/orderbookdetails)
provides metadata and official mark or index prices. The adapter uses
`mark_price`, or `index_price` if the mark field is absent. It never uses
`last_trade_price`. A missing official price fails the requested mark or bulk
mark batch. An invalid supplied mark fails instead of falling back to the index.

The adapter includes active perpetuals and preserves native symbols. `1000PEPE`
remains `1000PEPE`; the adapter does not offer a `PEPE` alias or rescale prices.
Spot markets and inactive perpetuals are excluded.

Tick and lot equal `10^-supported_price_decimals` and
`10^-supported_size_decimals`. `min_notional` retains `min_quote_amount` in USDC.
The adapter validates `min_base_amount` against the size grid but does not
represent it as the lot increment. The current `Market` trait has no separate
minimum-quantity field. The
[trading documentation](https://apidocs.lighter.xyz/docs/trading) says minimum
base and quote amounts apply to maker orders. This adapter supplies data for the
existing market-order paper policy; it does not model maker minimum quantity.

Maximum leverage equals `10000 / min_initial_margin_fraction`, matching the
conversion in the [official SDK](https://github.com/elliottech/lighter-python/blob/main/lighter/signer_client.py).
The adapter retains the official multiplier when supplied and otherwise uses
one. Initial margin is not a fixed amount per base unit and remains unset.

The API exposes perpetual type, but no authoritative underlying asset category.
`asset_class` retains the engine's existing `crypto` convention. The caller's
known crypto universe must restrict strategy selection because Lighter also
lists perpetuals on stocks, currencies, and metals.

[orderBookOrders](https://apidocs.lighter.xyz/reference/orderbookorders) returns
individual orders. The adapter requests 100 orders per side, aggregates their
remaining quantities at equal prices, and sorts best price first. This depth
can be insufficient for a requested trade. Empty sides, nonpositive values,
off-grid values, and crossed or locked books fail.

## Cache and request limits

Clones share one `Arc` containing the cache and request gate. Metadata has a
60-second maximum cache age. Marks have a 10-second maximum cache age. A refresh
updates both from the same response. Expired snapshots are cleared before the
refresh; network or parsing errors never return expired data.

Cache age measures time since response parsing. These REST payloads do not
supply a source-event timestamp, so the adapter cannot independently prove
exchange-side freshness. Order books are fetched on every request.

The shared gate spaces REST attempts by at least 1.3 seconds. This leaves room
under the [Standard limit of 60 requests per minute](https://apidocs.lighter.xyz/docs/rate-limits)
for the separate feed's small request budget. Separate clients or processes do
not share a gate. HTTP 429 and 405 responses impose a shared cooldown of at
least 60 seconds. Requests have a 10-second timeout and do not retry internally.

## Verification

The small fixtures retain public fields from repository captures made on
2026-09-08. `details.json` includes BTC, AAVE, 1000PEPE, inactive MKR, and one
spot entry. `aave-orders.json` retains four orders from each side of that capture.
HTTP mock tests exercise cache sharing, endpoint parameters, failed refreshes,
and missing official marks. Other tests verify constraints, native units,
book aggregation, request pacing, and unsupported account and order methods.

The ignored smoke test reads mainnet metadata, marks, and BTC depth. It submits
no order and requires no credentials.

```sh
cargo test --manifest-path service/Cargo.toml -p lighter
cargo test --manifest-path service/Cargo.toml -p lighter live_smoke -- --ignored --nocapture
```
