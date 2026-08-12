# Crypto-Scalper Plan 2: Data Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The two datasets the scalper's research needs: deep Binance USDT-perp 1m history in the house Parquet store, and measured HyperLiquid book costs (spread + depth-walk impact) per candidate symbol.

**Architecture:** One new binary crate `service/crates/scalper-data` with four subcommands (`pull-binance-perp`, `record-books`, `summarize-costs`, `universe`), consuming `crypto_portfolio::store` and `features_crypto::Bar` as libraries (first external consumer — zero changes to the frozen strategy's code). One additive method on `hyperliquid::Info` exposes the per-asset day volume/funding the venue already sends. All parsing/stats cores are pure functions with the I/O at the edges.

**Tech Stack:** Rust, tokio, reqwest, zip, serde_json, chrono. Plan renumbering note: the original spec's "plan 2 research pipeline" is split — this plan is the data half; plan 3 becomes signal research (features/training/walk-forward/Sharpe gate); plan 4 the bot crate. Spec: `docs/superpowers/specs/2026-08-12-crypto-scalper-design.md`.

## Global Constraints

- ZERO changes to `crypto-portfolio` source (frozen strategy) — it is consumed as a library only. Additive-only changes to `hyperliquid`.
- Research bars are `f64` (the `features_crypto::Bar` convention); the Decimal-for-money rule binds the trading path, not the research store.
- Perp bars are stored under `--data-root data/perp` (→ `data/perp/bars/asset=X/interval_s=60/…`), NEVER under `data/bars` where the frozen bot's spot data lives. Book snapshots under `data/books/`, cost summaries under `data/costs/`.
- Every CLI takes an explicit `--data-root` (house convention; no env vars, no defaults).
- CLI arg parsing is hand-rolled (`get`/`need` helpers) like `crypto-portfolio/src/main.rs:121-140` — no clap (workspace convention).
- Commit messages: repo's plain descriptive style, no prefixes, Claude co-author trailer.
- Run all cargo commands from `service/` in the worktree.

---

### Task 1: Expose the venue's per-asset context

`Info::marks()` already parses `metaAndAssetCtxs` but throws away everything except the mark. The scalper needs day volume (universe selection) and funding (features, later).

**Files:**
- Modify: `crates/hyperliquid/src/info.rs`

**Interfaces:**
- Consumes: existing `AssetCtx` struct (`info.rs:463-474`, fields `mark_px`, `mid_px`, `oracle_px`, `funding`, `day_ntl_vlm`, all `Option<String>`), `MetaResponse`, the private request plumbing `marks()` uses.
- Produces: `open_interest: Option<String>` field added to `AssetCtx` (serde rename `"openInterest"`); `pub async fn Info::asset_ctxs(&self) -> Result<Vec<(String, AssetCtx)>, VenueError>` — pairs `universe[i].name` with `assetCtxs[i]`, skipping delisted markets the same way `meta()` does NOT (include them; the caller filters on `is_delisted` via `meta()` if it cares — this method reports what the venue reports). Also add `pub fn AssetCtx::day_volume_usd(&self) -> Option<f64>` (parse `day_ntl_vlm`).

- [ ] **Step 1: Write the failing test**

Find the existing fixture used by `marks()`'s tests (look in `crates/hyperliquid/tests/fixtures/` for the `metaAndAssetCtxs` capture; `grep -rl metaAndAssetCtxs crates/hyperliquid/tests`). If none exists, capture one: `curl -s -X POST https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' -d '{"type":"metaAndAssetCtxs"}' > crates/hyperliquid/tests/fixtures/meta_and_asset_ctxs.json`. Then, in `info.rs`'s test module (or the fixture-driven test file the crate already uses — follow where the existing `marks()` parse test lives):

```rust
#[test]
fn asset_ctxs_pairs_names_with_contexts() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/meta_and_asset_ctxs.json"
    ))
    .unwrap();
    let pairs = parse_asset_ctxs(&text).unwrap();
    assert!(pairs.len() > 50, "mainnet lists >50 perps, got {}", pairs.len());
    let (name, ctx) = &pairs[0];
    assert!(!name.is_empty());
    let vol = ctx.day_volume_usd().expect("universe leader has day volume");
    assert!(vol > 0.0);
}
```

The pure parse function `fn parse_asset_ctxs(text: &str) -> Result<Vec<(String, AssetCtx)>, VenueError>` is the testable core; `asset_ctxs()` fetches and delegates to it (same split `marks()` uses internally — mirror it).

- [ ] **Step 2: Run to verify it fails** — `cargo test -p hyperliquid asset_ctxs` → FAIL (function not found).

- [ ] **Step 3: Implement**

Add to `AssetCtx`:

```rust
    #[serde(rename = "openInterest", default)]
    pub open_interest: Option<String>,
```

```rust
impl AssetCtx {
    /// The venue's 24h notional volume for this market, in USD.
    pub fn day_volume_usd(&self) -> Option<f64> {
        self.day_ntl_vlm.as_deref().and_then(|s| s.parse().ok())
    }
}
```

`parse_asset_ctxs` deserializes the same two-element response `marks()` reads (`[MetaResponse, Vec<AssetCtx>]`), zips `universe` names with contexts, and errors if the lengths differ (a mismatch means the venue changed the contract — fail loudly). `asset_ctxs()` posts `{"type":"metaAndAssetCtxs"}` with the same request helper `marks()` uses and delegates.

- [ ] **Step 4: Run** — `cargo test -p hyperliquid` → PASS (all existing + new).

- [ ] **Step 5: Commit** — `git add crates/hyperliquid && git commit` — message: `Report the market context the venue already sends`

---

### Task 2: The `scalper-data` crate and Binance perp 1m ingestion

**Files:**
- Create: `crates/scalper-data/Cargo.toml`, `crates/scalper-data/src/main.rs`, `crates/scalper-data/src/binance_um.rs`
- Modify: `Cargo.toml` (workspace members + `scalper-data` is NOT added to the deps table — nothing depends on it)

**Interfaces:**
- Consumes: `features_crypto::Bar` (fields `ts_utc: DateTime<Utc>, asset: String, interval_s: i32, open/high/low/close/volume: f64, quote_volume: Option<f64>, trades: Option<i64>`, plus `validate()`), `crypto_portfolio::store::write(&root, &[Bar])` and `store::read_asset(&root, 60, asset)`.
- Produces:
  - `pub fn binance_um_symbol(asset: &str) -> Option<String>` — `BTC → BTCUSDT`; HL's k-prefixed micro-cap coins map to Binance's 1000-prefix (`kPEPE → 1000PEPEUSDT`, same for kBONK, kSHIB, kFLOKI, kLUNC); assets with no UM listing (`HYPE`, `PURR`) → `None`.
  - `pub fn parse_um_klines_zip(bytes: &[u8], asset: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<Bar>, String>` — pure.
  - `pub async fn fetch_um_month(client: &reqwest::Client, symbol: &str, year: i32, month: u32) -> Result<Option<Vec<u8>>, String>` — `None` on HTTP 404 (month predates listing or postdates delisting: not an error, survivorship depends on it).
  - CLI: `scalper-data pull-binance-perp --data-root <dir> --assets BTC,ETH,... --start YYYY-MM-DD --end YYYY-MM-DD`

**Crate manifest:**

```toml
[package]
name = "scalper-data"
description = "Research data for the crypto-scalper: Binance perp 1m history and measured HyperLiquid book costs."
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
crypto-portfolio.workspace = true
features-crypto.workspace = true
hyperliquid.workspace = true
venue.workspace = true
chrono = { workspace = true }
serde.workspace = true
serde_json.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "time"] }
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
zip = { version = "4", default-features = false, features = ["deflate"] }
```

(Check the workspace deps table for `chrono`'s exact form — `crypto-portfolio/Cargo.toml` shows the convention; mirror it. If `features-crypto` is missing from the workspace deps table, add it there — additive.)

- [ ] **Step 1: Write the failing tests** (`binance_um.rs` `mod tests`)

```rust
#[test]
fn hl_coins_map_to_um_symbols() {
    assert_eq!(binance_um_symbol("BTC").as_deref(), Some("BTCUSDT"));
    assert_eq!(binance_um_symbol("kPEPE").as_deref(), Some("1000PEPEUSDT"));
    assert_eq!(binance_um_symbol("kBONK").as_deref(), Some("1000BONKUSDT"));
    assert_eq!(binance_um_symbol("HYPE"), None, "not listed on Binance UM");
}

#[test]
fn um_kline_zips_parse_with_and_without_header_and_in_ms_or_us() {
    // Two rows: one microsecond epoch (Binance switched in 2025), one with the
    // CSV header UM monthly files carry. 12 columns per kline row.
    let csv = "open_time,open,high,low,close,volume,close_time,quote_volume,count,taker_buy_volume,taker_buy_quote_volume,ignore\n\
        1754956800000000,64000.1,64010.0,63990.0,64005.5,12.5,1754956859999999,800070.0,42,6.0,384033.0,0\n\
        1754956860000000,64005.5,64020.0,64000.0,64018.0,8.25,1754956919999999,528148.0,30,4.1,262476.0,0\n";
    let bytes = zip_with_one_file("BTCUSDT-1m-2026-08.csv", csv.as_bytes());
    let start = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let bars = parse_um_klines_zip(&bytes, "BTC", start, end).unwrap();
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[0].asset, "BTC");
    assert_eq!(bars[0].interval_s, 60);
    assert_eq!(bars[0].ts_utc.timestamp(), 1_754_956_800);
    assert_eq!(bars[0].close, 64005.5);
    assert_eq!(bars[0].quote_volume, Some(800070.0));
    assert_eq!(bars[0].trades, Some(42));
    // Millisecond epochs (pre-2025 files) parse to the same instant.
    let csv_ms = "1754956800000,64000.1,64010.0,63990.0,64005.5,12.5,1754956859999,800070.0,42,6.0,384033.0,0\n";
    let bytes_ms = zip_with_one_file("BTCUSDT-1m-2026-08.csv", csv_ms.as_bytes());
    let bars_ms = parse_um_klines_zip(&bytes_ms, "BTC", start, end).unwrap();
    assert_eq!(bars_ms[0].ts_utc, bars[0].ts_utc);
}

#[test]
fn rows_outside_the_window_are_dropped() {
    let csv = "1754956800000,1,1,1,1,1,1754956859999,1,1,1,1,0\n";
    let bytes = zip_with_one_file("BTCUSDT-1m-2026-08.csv", csv.as_bytes());
    let start = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    assert!(parse_um_klines_zip(&bytes, "BTC", start, end).unwrap().is_empty());
}
```

with a test helper building an in-memory zip:

```rust
fn zip_with_one_file(name: &str, content: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
    z.write_all(content).unwrap();
    z.finish().unwrap();
    buf.into_inner()
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p scalper-data` → FAIL (crate builds once `main.rs` exists with a stub `fn main() {}` and `mod binance_um;`; the tests fail on missing functions).

- [ ] **Step 3: Implement `binance_um.rs`**

```rust
//! Binance USDT-perp (UM futures) monthly kline archive.
//!
//! Mirrors the spot pipeline in `crypto_portfolio::binance_archive` but is a
//! fresh implementation: that crate is the frozen strategy's and stays
//! untouched. URL shape:
//!   https://data.binance.vision/data/futures/um/monthly/klines/{SYMBOL}/1m/{SYMBOL}-1m-{YYYY-MM}.zip

use chrono::{DateTime, TimeZone, Utc};
use features_crypto::Bar;
use std::io::Read;

const ARCHIVE: &str = "https://data.binance.vision";

/// The Binance UM symbol for a HyperLiquid coin. HL's k-prefix (thousandths)
/// coins trade on Binance with a 1000-prefix. `None` = not listed on UM, and
/// the caller must exclude the asset from Binance-based training rather than
/// silently substituting spot.
pub fn binance_um_symbol(asset: &str) -> Option<String> {
    let unlisted = ["HYPE", "PURR"];
    if unlisted.contains(&asset) {
        return None;
    }
    match asset.strip_prefix('k') {
        Some(rest) => Some(format!("1000{}USDT", rest.to_uppercase())),
        None => Some(format!("{}USDT", asset.to_uppercase())),
    }
}

/// One monthly zip, or `None` on 404 — a missing month is what "not listed
/// yet" and "already delisted" look like, and both are survivorship truth.
pub async fn fetch_um_month(
    client: &reqwest::Client,
    symbol: &str,
    year: i32,
    month: u32,
) -> Result<Option<Vec<u8>>, String> {
    let url = format!(
        "{ARCHIVE}/data/futures/um/monthly/klines/{symbol}/1m/{symbol}-1m-{year:04}-{month:02}.zip"
    );
    let res = client.get(&url).send().await.map_err(|e| format!("{url}: {e}"))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !res.status().is_success() {
        return Err(format!("{url}: HTTP {}", res.status()));
    }
    let bytes = res.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    Ok(Some(bytes.to_vec()))
}

/// Epochs arrive in ms (pre-2025 files) or µs (2025+). Values, not flags.
fn epoch_utc(raw: i64) -> Result<DateTime<Utc>, String> {
    let micros = if raw > 100_000_000_000_000 { raw } else { raw * 1000 };
    Utc.timestamp_micros(micros)
        .single()
        .ok_or_else(|| format!("unreadable epoch {raw}"))
}

/// Parse one monthly zip into validated 1m bars inside [start, end).
pub fn parse_um_klines_zip(
    bytes: &[u8],
    asset: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<Bar>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("{asset}: bad zip: {e}"))?;
    let mut csv = String::new();
    archive
        .by_index(0)
        .map_err(|e| format!("{asset}: empty zip: {e}"))?
        .read_to_string(&mut csv)
        .map_err(|e| format!("{asset}: unreadable csv: {e}"))?;

    let mut bars = Vec::new();
    for line in csv.lines() {
        if line.is_empty() || line.starts_with("open_time") {
            continue; // UM monthly files sometimes carry a header row
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 11 {
            return Err(format!("{asset}: kline row has {} columns: {line}", cols.len()));
        }
        let ts = epoch_utc(cols[0].parse().map_err(|e| format!("{asset}: {e}: {line}"))?)?;
        if ts < start || ts >= end {
            continue;
        }
        let f = |i: usize| -> Result<f64, String> {
            cols[i].parse().map_err(|e| format!("{asset}: col {i}: {e}: {line}"))
        };
        let bar = Bar {
            ts_utc: ts,
            asset: asset.to_string(),
            interval_s: 60,
            open: f(1)?,
            high: f(2)?,
            low: f(3)?,
            close: f(4)?,
            volume: f(5)?,
            quote_volume: Some(f(7)?),
            trades: Some(cols[8].parse().map_err(|e| format!("{asset}: col 8: {e}"))?),
        };
        bar.validate()?;
        bars.push(bar);
    }
    bars.sort_by_key(|b| b.ts_utc);
    bars.dedup_by_key(|b| b.ts_utc);
    Ok(bars)
}
```

- [ ] **Step 4: Run** — `cargo test -p scalper-data` → PASS.

- [ ] **Step 5: Wire the subcommand in `main.rs`**

Hand-rolled args like `crypto-portfolio/src/main.rs:121-140` (copy the tiny `get`/`need` helper shapes — ~15 lines, not a dependency). `pull-binance-perp` iterates assets × months (month enumeration from start to end inclusive of the end month, end-exclusive on bar timestamps), skips assets where `binance_um_symbol` is `None` with a printed warning, calls `store::write(&data_root, &bars)` per asset-month (the same call `cmd_data_archive` makes — see `crypto-portfolio/src/main.rs:813` for reference), prints one line per asset-month: `BTC 2026-08: 44640 bars` or `… skipped (404: not listed)`. A month where every asset 404s is fine; a network error is not.

Run the live verification (network): from the repo root,

```
cargo run -p scalper-data -- pull-binance-perp --data-root ../data/perp --assets BTC,ETH --start 2026-06-01 --end 2026-08-01
```

Expected: ~4 lines, ~44,640 bars per full month per asset (60×24×31), files at `data/perp/bars/asset=BTC/interval_s=60/2026-06.parquet` etc. Then verify read-back in a quick test or via a `stats` println: `store::read_asset(&root, 60, "BTC")` returns the same count, sorted, no duplicate timestamps.

- [ ] **Step 6: Commit** — message: `Pull the perp minutes the scalper will learn from`

---

### Task 3: The book recorder

**Files:**
- Create: `crates/scalper-data/src/books.rs`
- Modify: `crates/scalper-data/src/main.rs` (subcommand)

**Interfaces:**
- Consumes: `hyperliquid::{Info, L2Book, L2Level, MAINNET}`, `Info::asset_ctxs()` from Task 1, `binance_um_symbol` (to annotate coverage).
- Produces:
  - `pub struct BookSnapshot { pub ts_ms: i64, pub coin: String, pub bids: Vec<(f64, f64)>, pub asks: Vec<(f64, f64)> }` (px, sz pairs, best-first, up to `depth` levels) with serde derive.
  - `pub fn snapshot_from_book(coin: &str, ts_ms: i64, book: &L2Book, depth: usize) -> Result<BookSnapshot, String>` — pure; errors on an empty side (a one-sided book is data worth refusing, not recording as zeros).
  - CLI: `scalper-data record-books --data-root <dir> --seconds N --interval N [--assets A,B | --top N]` — `--top N` picks the N largest by `day_volume_usd()` among non-delisted markets. Appends JSONL (one `BookSnapshot` per line) to `{data-root}/books/{YYYY-MM-DD}.jsonl`, file per UTC day, flushed every round.

- [ ] **Step 1: Failing tests** (`books.rs`)

```rust
#[test]
fn a_book_becomes_a_snapshot_best_levels_first() {
    let book = L2Book {
        levels: vec![
            vec![lvl("64000", "1.5"), lvl("63999", "2.0")], // bids
            vec![lvl("64001", "0.7"), lvl("64002", "3.0")], // asks
        ],
    };
    let s = snapshot_from_book("BTC", 1_754_956_800_000, &book, 10).unwrap();
    assert_eq!(s.bids[0], (64000.0, 1.5));
    assert_eq!(s.asks[0], (64001.0, 0.7));
    assert_eq!(s.bids.len(), 2);
}

#[test]
fn depth_caps_the_levels_kept() {
    let book = L2Book {
        levels: vec![
            (0..30).map(|i| lvl(&format!("{}", 64000 - i), "1")).collect(),
            (0..30).map(|i| lvl(&format!("{}", 64001 + i), "1")).collect(),
        ],
    };
    let s = snapshot_from_book("BTC", 0, &book, 10).unwrap();
    assert_eq!(s.bids.len(), 10);
    assert_eq!(s.asks.len(), 10);
}

#[test]
fn a_one_sided_book_is_refused() {
    let book = L2Book { levels: vec![vec![], vec![lvl("1", "1")]] };
    assert!(snapshot_from_book("X", 0, &book, 10).is_err());
}
```

`fn lvl(px: &str, sz: &str) -> L2Level` test helper (construct with `n: 1`).

- [ ] **Step 2: Run to verify failure**, **Step 3: implement** (`snapshot_from_book` parses the px/sz strings to f64, truncates to `depth`; the recorder loop mirrors `book_capture.rs`'s round structure — `l2_book` per coin, `serde_json::to_string` + append with `OpenOptions::new().create(true).append(true)`, `tokio::time::sleep(interval)` between rounds; a per-coin fetch error prints a warning and skips the coin that round, never kills the run), **Step 4: tests pass**.

- [ ] **Step 5: Live smoke** — `cargo run -p scalper-data -- record-books --data-root ../data --seconds 30 --interval 10 --assets BTC,ETH,SOL` → `data/books/<today>.jsonl` gains ~9 lines of valid JSON; eyeball one line: both sides populated, prices sane, ts_ms current.

- [ ] **Step 6: Commit** — message: `Record the books we will be charged against`

---

### Task 4: Cost summaries from recorded books

**Files:**
- Create: `crates/scalper-data/src/costs.rs`
- Modify: `crates/scalper-data/src/main.rs` (subcommand)

**Interfaces:**
- Consumes: `BookSnapshot` from Task 3.
- Produces:
  - `pub struct CostSummary { pub samples: u32, pub spread_bps_median: f64, pub spread_bps_p75: f64, pub cross_bps: BTreeMap<String, Option<f64>>, pub top_depth_usd_median: f64 }` (serde; `cross_bps` keyed by notional string, `None` when the median snapshot cannot absorb that notional — thin book, and that fact IS the finding).
  - `pub fn walk_cost_bps(asks_or_bids: &[(f64, f64)], notional_usd: f64, mid: f64, is_buy: bool) -> Option<f64>` — depth-walked VWAP vs mid, in bps, `None` if the visible book can't fill it (port of `book_capture.rs`'s `walk`, generalized to both sides).
  - `pub fn summarize(snapshots: &[BookSnapshot], notionals: &[f64]) -> BTreeMap<String, CostSummary>` — pure; per-coin medians over per-snapshot values; a snapshot's cross cost is the mean of buy-side and sell-side walk (symmetric assumption, stated in a comment).
  - CLI: `scalper-data summarize-costs --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD [--notionals 1000,5000,20000]` — reads every `{data-root}/books/{day}.jsonl` in range, writes `{data-root}/costs/summary-{start}-{end}.json` (the `BTreeMap<String, CostSummary>` pretty-printed) and prints a table sorted by spread.

- [ ] **Step 1: Failing tests**

```rust
fn snap(coin: &str, bid: f64, ask: f64, sz: f64) -> BookSnapshot {
    BookSnapshot {
        ts_ms: 0,
        coin: coin.into(),
        bids: vec![(bid, sz), (bid - 1.0, sz * 10.0)],
        asks: vec![(ask, sz), (ask + 1.0, sz * 10.0)],
    }
}

#[test]
fn walking_the_book_prices_the_levels_you_eat() {
    // Buy $128,002 against asks of 1.0@64001 and 3.0@64002: eats all of level
    // one and 1.000015... of level two; VWAP sits between the two prices.
    let asks = vec![(64001.0, 1.0), (64002.0, 3.0)];
    let mid = 64000.5;
    let cost = walk_cost_bps(&asks, 128_002.0, mid, true).unwrap();
    assert!(cost > 0.0 && cost < 2.0, "got {cost} bps");
    assert!(walk_cost_bps(&asks, 500_000.0, mid, true).is_none(), "book too thin");
}

#[test]
fn summaries_carry_medians_and_thin_books_stay_visible() {
    let snaps: Vec<BookSnapshot> =
        (0..5).map(|_| snap("BTC", 64000.0, 64001.0, 2.0)).collect();
    let out = summarize(&snaps, &[5_000.0, 50_000_000.0]);
    let btc = &out["BTC"];
    assert_eq!(btc.samples, 5);
    assert!(btc.spread_bps_median > 0.0);
    assert!(btc.cross_bps["5000"].is_some());
    assert!(btc.cross_bps["50000000"].is_none(), "an unabsorbable notional reads None, not 0");
}
```

- [ ] **Step 2: verify failure**, **Step 3: implement**, **Step 4: pass** — median = midpoint of sorted values (even count: mean of middle two); mid = (best_bid + best_ask)/2 per snapshot; spread_bps = (ask−bid)/mid×1e4; `top_depth_usd` = best_ask px×sz.

- [ ] **Step 5: End-to-end check** on the Task 3 smoke data: `cargo run -p scalper-data -- summarize-costs --data-root ../data --start <today> --end <today>` prints BTC/ETH/SOL rows with plausible spreads (BTC ≈ 0.1–2 bps) and writes the JSON.

- [ ] **Step 6: Commit** — message: `Turn recorded books into the costs a backtest must charge`

---

### Task 5: Candidate universe listing

**Files:**
- Create: `crates/scalper-data/src/universe.rs`
- Modify: `crates/scalper-data/src/main.rs` (subcommand)

**Interfaces:**
- Consumes: `Info::asset_ctxs()`, `binance_um_symbol`. (No delisting check needed: `select_candidates` drops anything without positive day volume, which is what a delisted market reports.)
- Produces:
  - `pub struct Candidate { pub coin: String, pub day_volume_usd: f64, pub binance_um: Option<String> }` (serde)
  - `pub fn select_candidates(pairs: &[(String, AssetCtx)], top: usize, exclude: &[String]) -> Vec<Candidate>` — pure: keep only positive `day_volume_usd()`, drop excluded, sort by volume desc, take `top`.
  - CLI: `scalper-data universe --data-root <dir> --top 25 [--exclude A,B]` — writes `{data-root}/scalper-universe.json` (`Vec<Candidate>`) and prints a table with a `NO-BINANCE` marker on unmapped coins.

- [ ] **Step 1: Failing test** — `select_candidates` with three synthetic pairs (volumes 300, 200, 100; exclude the middle one; top 2) returns the first and third, ordered by volume, with `binance_um` populated via the real mapping.

```rust
#[test]
fn candidates_rank_by_volume_and_respect_exclusions() {
    let ctx = |v: &str| AssetCtx { /* all fields None except */ day_ntl_vlm: Some(v.into()), ..synthetic_ctx() };
    let pairs = vec![
        ("BTC".to_string(), ctx("300")),
        ("ETH".to_string(), ctx("200")),
        ("kPEPE".to_string(), ctx("100")),
    ];
    let out = select_candidates(&pairs, 2, &["ETH".to_string()]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].coin, "BTC");
    assert_eq!(out[1].coin, "kPEPE");
    assert_eq!(out[1].binance_um.as_deref(), Some("1000PEPEUSDT"));
}
```

(`synthetic_ctx()` builds an `AssetCtx` with every field `None` — if the struct's fields are not all `pub`-constructible from outside the hyperliquid crate, construct via `serde_json::from_value(json!({}))` instead; note which you needed in the report.)

- [ ] **Step 2-4: fail → implement → pass.**

- [ ] **Step 5: Live check** — `cargo run -p scalper-data -- universe --data-root ../data --top 25` prints 25 rows, BTC/ETH near the top, HYPE marked NO-BINANCE; JSON written.

- [ ] **Step 6: Final task commit** — message: `Name the candidates the evidence will choose from` — then run the full workspace suite (`cargo test`) one last time and confirm green.
