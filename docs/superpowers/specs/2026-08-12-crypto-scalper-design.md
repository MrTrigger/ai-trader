# Crypto-Scalper Bot — Design

Date: 2026-08-12
Status: approved in brainstorming session; pending written-spec review

## Purpose

A new bot that scalps HyperLiquid perps on a minutes horizon: decisions on 1-minute
bar closes, holds of roughly 5–60 minutes, long and short. Model-based (LightGBM),
validated with the same evidence-gate discipline as the existing bots. This is an
experiment; it must not touch the capital, account, or code paths of the frozen
rank-reward crypto-portfolio strategy.

## Decisions made during brainstorming

| Question | Decision |
|---|---|
| Time horizon | 1m-bar decisions, ~5–60 min holds. Validated at taker costs; maker fills live are unmodeled upside. |
| Universe | Evidence-selected: backtest ~15–25 candidate HL perps (majors + liquid mid-caps), trade only symbols whose walk-forward edge survives their own measured costs. Refresh slowly (weekly/monthly), no intraday scanner. |
| Signal | LightGBM on engineered 1m features (house pattern), pooled across symbols. Not LSTM: at this horizon, GBTs on tabular features match or beat sequence models, and the Rust inference + training chain already exists. |
| Architecture | Approach A: standalone `crypto-scalper` crate in the futures-bot shape (long-running tokio loop, drives `VenueAdapter` directly, skips Plan/executor/runner). Extraction of a shared stream-bot engine deferred until n=2 is real. |
| Market data | WebSocket streaming from day one (user decision — no REST polling for the live loop). |

## Architecture

### New crate: `service/crates/crypto-scalper`

One long-running tokio binary handling all traded symbols. Skeleton mirrors
`futures-bot`:

- `tokio::select!` over: (a) WS event stream, (b) `records::listen_controls`
  NOTIFY (start/stop/pause/flatten from the dashboard), (c) fallback tick for
  watchdogs.
- Registered as a `bots` row: `cadence='stream'`, `asset_class='crypto'`,
  `launch='deploy/crypto-scalper.sh'`, `enabled=false` initially. The api
  supervisor and dashboard then manage it with no framework changes.
- Crash recovery: state snapshot to DB on every decision via existing
  `records::put_snapshot`/`put_sim_state`; status published every 60s and on
  change.
- Two `account_bindings`: sim first, later a dedicated HL sub-account for live —
  never the crypto-portfolio bot's account, so margin and positions cannot mix.

### New module: `hyperliquid/src/ws.rs`

WebSocket client (tokio-tungstenite) with subscriptions:

- `candle` (1m) per symbol — drives the decision cycle
- `bbo` per symbol — drives tick-level rails (stops, max-hold) and entry pricing
- `userEvents`/`userFills` (authenticated) — fill notifications

Policies:

- Reconnect with exponential backoff + full resubscribe.
- On reconnect, gap-fill missed candles via existing REST `Info::candles()` so
  incremental feature state never has holes.
- Per-subscription staleness watchdog: silent feed in active trading →
  flatten + halt (futures-bot stall-rail philosophy).

### Decision cycle

On each 1m candle close per symbol: update incremental feature state → score
with the LightGBM model → map score to target (long/short/flat + size) → hand to
execution submodule. Stops and max-hold-time are enforced on BBO ticks, not just
bar closes.

## Data, features, training

- **Historical training data**: Binance USDT-perp 1m klines via the existing
  archive pipeline, extended from spot to futures endpoints. Stored in the
  existing partitioned Parquet store at `interval_s=60`. Perp-vs-perp matches HL
  better than the spot basis the daily bot accepted.
- **Native capture from day one**: the scalper in shadow mode records HL 1m
  candles, BBO snapshots, and funding into the same store — builds the dataset
  that later validates (and eventually replaces) the Binance basis.
- **Cost measurement**: extend the `hyperliquid/examples/book_capture.rs`
  approach into a recorder for the candidate universe over several weeks →
  per-symbol spread/depth model. (The WS-only requirement applies to the live
  trading loop; this offline recorder may use REST polling or WS, whichever is
  simpler.) Backtests charge these measured costs per
  symbol. This is what settles majors-vs-alts empirically.
- **Features**: new module following house rules (Rust computes everything,
  Python only fits; incremental/trailing-state only, append-safe). Candidates:
  multi-window returns/momentum (1–60m), realized vol, volume z-scores, funding
  rate, distance-from-VWAP, time-of-day, BTC-leadership (cross-symbol lag),
  candle shape. Own `FEATURE_SET_VERSION`.
- **Model**: one pooled model across symbols, features normalized per-symbol.
  Same contract as `crypto-portfolio/src/model.rs`: JSON tree dump only, ordered
  feature list matched to the Rust catalogue, `trained_through` refusal, format
  version. Label: forward net return over a fixed horizon; the horizon is a
  hyperparameter selected in walk-forward from a small candidate set (e.g. 15m,
  30m, 60m), not tuned freely. Evaluation charges measured per-symbol costs.
  Walk-forward with purged splits.

## Execution

### HyperLiquid adapter additions (backward-compatible)

- Post-only orders (`Alo` tif)
- Reduce-only flag (currently hardcoded `false`)
- IOC limit orders with explicit price cap
- Cancel-by-cloid (removes the extra REST round trip in the current cancel path)

### Order lifecycle

- **Entries**: post-only at the touch → on timeout (~5–15s, config) reprice once
  → then abandon (marginal signal) or cross with IOC capped at max-slippage
  price.
- **Exits**: escalate faster — post-only briefly, then cross. Stop-loss exits go
  straight to IOC.
- Deterministic client order ids (`bot_id` + decision id + leg) for idempotent
  retries after crash or reconnect.

### Sizing

Fixed fractional risk: each position risks a configured fraction of NAV
(~25–50bps) via its stop distance. Caps on per-symbol notional, concurrent
position count, gross exposure. Plain config, no optimizer.

## Risk rails (all tick-enforced)

- Per-trade stop
- Max hold time
- Order-rate limiter (runaway loop must starve, not spray)
- Daily loss kill: realized + unrealized past threshold → flatten + halt until
  manual resume
- Feed-stall watchdog
- Fills-vs-model reconcile every loop; any mismatch → flatten + halt, never
  auto-repair
- Live arming: futures-bot two-flag pattern (`--live` + `HL_ALLOW_LIVE=yes`)
- Dedicated HL sub-account for live

## Modes

- `shadow`: full loop runs, decisions + hypothetical fills logged, no orders
- `paper`: sim fills charged at measured taker cost (no maker-fill optimism)
- `live`: real orders, two-flag armed

## Validation gates (in order)

1. **Backtest gate**: walk-forward on Binance perp 1m history at measured
   per-symbol HL costs + taker fee. Positive net with meaningful margin. Also
   performs universe selection.
2. **Shadow gate**: ≥3–4 weeks on live HL data. Signal distribution matches
   backtest; zero unexplained infra faults; hypothetical-fill PnL tracks
   backtest expectations.
3. **Paper gate**: ≥2 weeks of conservative sim fills, verifying execution
   logic (escalation, stops, reconcile) end to end.
4. **Live**: tiny allocation on the sub-account, worst-case daily loss trivially
   survivable; scale only on evidence.

## Testing

- WS reconnect/gap-fill unit tests against recorded frames
- Feature parity: Rust live path vs training matrix on identical data must be
  bit-identical
- Replay determinism: same tape → same decisions
- Adapter tests for new order types against HL testnet

## Out of scope (deliberately)

- Sub-minute / order-book-driven strategies, queue-position modeling
- Intraday scanner / dynamic universe
- Changes to Plan/executor/runner
- Any change to the frozen crypto-portfolio strategy or its account
