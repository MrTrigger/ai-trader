mod binance_costs;
mod binance_micro;
mod binance_um;
mod books;
mod costs;
mod exits;
mod gate;
mod matrix;
mod micro_join;
mod universe;

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, Datelike, Utc};
use crypto_portfolio::store;
use hyperliquid::{Info, MAINNET};

use binance_um::{
    binance_um_symbol, fetch_um_day, fetch_um_month, open_month_days, parse_um_klines_zip,
};
use books::BookSnapshot;
use costs::{summarize, CostSummary};
use universe::select_candidates;

const USAGE: &str = "\
usage: scalper-data <command>

  pull-binance-perp --data-root <dir> --assets BTC,ETH,... --start YYYY-MM-DD --end YYYY-MM-DD
      Bulk-load Binance USDT-perp (UM futures) monthly 1m kline archives into
      the shared Parquet bar store (interval_s=60). Assets with no UM listing
      are skipped with a warning rather than silently substituted with spot.
      Expects its own root (use --data-root data/perp), separate from the
      frozen bot's daily spot store; record-books, summarize-costs, and
      universe use the shared data/ root.

  pull-binance-micro --data-root <dir> --assets BTC,ETH,... --start YYYY-MM-DD --end YYYY-MM-DD [--sources book,flow,funding,metrics]
      Bulk-load Binance UM microstructure archives (bookDepth, aggTrades,
      fundingRate, metrics - all free at data.binance.vision) into
      {data-root}/binance-micro/, keyed by BINANCE SYMBOL (not the HL store
      key - the matrix layer joins the two via the universe file). bookDepth
      downsamples to one snapshot per minute (last <= minute close, ±0.2%
      and ±1.0% bands only); aggTrades is streamed into per-minute
      spread/taker-flow aggregates and the raw trade tape is never stored;
      fundingRate merges into one append-safe-by-ts file per symbol;
      metrics rows pass through as-is. 404-vs-error discipline identical to
      pull-binance-perp. Assets with no UM listing are skipped with a
      warning. --sources restricts which of the four archives to pull
      (default: all).

  record-books --data-root <dir> --seconds N --interval N [--assets A,B | --top N]
      Poll Hyperliquid L2 books at --interval seconds and append one JSON
      line per snapshot per coin to {data-root}/books/{YYYY-MM-DD}.jsonl
      (UTC day, flushed every round). The run is bounded by wall clock, not
      round count: no new round starts once --seconds has elapsed, but an
      in-flight round is allowed to finish, so a run can overrun --seconds
      by up to one round's fetch latency. --top N picks the N largest
      markets by day_volume_usd() instead of a fixed --assets list. A
      coin's fetch or book error is a warning, not a reason to stop the run.

  summarize-costs --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD [--notionals 1000,5000,20000]
      Read every {data-root}/books/{YYYY-MM-DD}.jsonl in [--start, --end]
      (inclusive, UTC days) and turn the recorded snapshots into a per-coin
      spread and depth-walked cross-cost summary. Writes the pretty-printed
      summary to {data-root}/costs/summary-{start}-{end}.json and prints a
      table sorted by spread. A missing day file is skipped with a note, not
      a fatal error.

  universe --data-root <dir> --top N [--exclude A,B,...]
      List the top N candidates by day volume from Hyperliquid, excluding any
      in --exclude. Writes {data-root}/scalper-universe.json and prints a table
      with a NO-BINANCE marker on unmapped coins.

  training-matrix --data-root <dir> --universe <path> --start YYYY-MM-DD --end YYYY-MM-DD --out <path> [--stride 5] [--horizons 15,30,60] [--micro-root <dir>]
      Build the trainer's JSONL matrix: for each universe candidate with a
      Binance UM listing, read 1m bars from {data-root}/perp (store key =
      coin.to_uppercase()), compute features-scalper's fs-2 feature set (BTC
      context from store key BTC - a hard requirement, since btc_ret_5 and
      rel_ret_5 need it), join with forward returns at the given horizons,
      keep every --stride-th fully-warm row, and write a manifest line
      followed by one JSON row per line to --out. A candidate with no bars
      in the store is skipped with a warning, not a fatal error.
      --micro-root <dir> joins each candidate's Binance book/flow/metrics/
      funding files (read from {micro-root}/binance-micro/, same layout
      pull-binance-micro writes) into the 12 fs-2 microstructure features;
      omit it to run fs-2 with those 12 features all None (still a valid,
      just uninformative, matrix). Prints per-asset 'kept N of M (book from
      DATE|none, flow from DATE|none, metrics from DATE|none, funding from
      DATE|none)' - one first-coverage date per source, since funding's
      unlimited lookback covers from day one while book/flow/metrics (the
      sources that actually gate row survival) typically start later.

  binance-costs --data-root <dir> --universe <path> --start YYYY-MM-DD --end YYYY-MM-DD --out <path> [--notional 5000]
      Turn {data-root}/binance-micro/{book,flow}/{SYMBOL}/ (pull-binance-micro's
      output) into a per-asset, per-day cost estimate: spread_bps_p75 (p75 of
      that day's minute spread estimates) and impact_bps (a pre-registered
      walk-cost model over the day's median ±0.2% band depth, None when the
      book was too thin to absorb --notional that day). Joins Binance symbol
      to HL coin via --universe (same join training-matrix uses) and writes
      output keyed by coin: {asset: {day: {spread_bps_p75, impact_bps,
      samples}}}. A candidate with no Binance UM listing is skipped.

  gate --matrix <path> --folds <path/folds.json> (--costs <path> | --binance-costs <path> --fee-taker-bps F --fee-maker-bps F) [--threshold-mult 1.5] [--notional 5000] [--exit time|atr] [--data-root <dir>] --out <path>
      Walk-forward gate: for each fold in folds.json, load fold-N.json (a
      LightGBM JSON dump), predict every matrix row in that fold's test
      window via lightgbm-json, and simulate a threshold-gated long/short
      strategy net of measured round-trip costs. --costs uses the plan-2
      flat per-asset cost summary (2*4.5bps taker + spread/2 + cross, same
      every day). --binance-costs uses binance-costs' time-varying per-day
      output instead: round_trip_bps = 2*fee_taker_bps + spread_bps_p75 +
      2*impact_bps, looked up by the trade's ENTRY day, falling back to the
      nearest PRIOR day within 14 days when the entry day is missing or
      thin (impact_bps null); no day found within that window makes the
      asset untradeable that day (reported per-asset as
      days_without_costs). --fee-maker-bps is recorded in the report for
      the protocol's future use but unused by today's taker-only
      simulation (both legs are charged the taker fee). The two cost flags
      are mutually exclusive; exactly one is required. Stitches per-fold
      daily P&L into one series and reports the annualized Sharpe gate
      (> 2.0 to PASS), plus overall.projected_30d_volume_usd and
      overall.fee_bps_used, to --out.

      --exit time (default) is Amendment 1's unchanged fixed-horizon exit -
      byte-identical to every gate run before Amendment 3. --exit atr
      applies Amendment 3's pre-registered ATR(14) Wilder stop/target
      instead (stop = 4*ATR, target = 1.2*stop, stop-wins-ties, time exit
      as fallback at the horizon) and requires --data-root <dir> to read
      each asset's 1m bars from {data-root}/perp (store key =
      coin.to_uppercase(), same rule as training-matrix) to resolve each
      accepted entry's realized exit. Adds overall.exit_mode and
      exit_stats (stops/targets/time_exits/mean_bars_held/skipped_no_atr)
      to the report; both are omitted entirely under --exit time; the
      knobs (k=4, R:R=1.2, ATR period 14) are fixed by
      docs/scalper-research.md Amendment 3 and are not CLI flags.
";

pub(crate) fn get(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|v| v == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub(crate) fn need(args: &[String], name: &str) -> Result<String, String> {
    get(args, name).ok_or_else(|| format!("{name} is required"))
}

pub(crate) fn parse_date(text: &str) -> Result<DateTime<Utc>, String> {
    format!("{text}T00:00:00Z")
        .parse()
        .map_err(|e| format!("bad date {text:?}: {e}"))
}

/// Resolve one `--assets` token to its store key and Binance UM symbol.
///
/// The two derivations must read the input at different points: HL's `k`
/// prefix (kPEPE, kBONK, ...) means "thousandths" and `binance_um_symbol`
/// only recognises it in its original lowercase-`k` form - uppercasing
/// first turns `kPEPE` into `KPEPE`, silently missing the 1000-prefix
/// mapping and (wrongly) treating the coin as unlisted. But `Bar::validate`
/// requires the canonical uppercase asset, so the store key `KPEPE` is what
/// every partition and file name use. So: symbol lookup from the original
/// case, store key uppercased.
fn resolve_asset(input: &str) -> (String, Option<String>) {
    (input.to_uppercase(), binance_um_symbol(input))
}

/// (year, month) pairs from `start`'s month through the month containing the
/// instant just before `end` - the same exclusive-end-boundary convention as
/// `crypto_portfolio::binance_archive::months`, so `--end 2026-08-01` does
/// not reach into August.
fn months(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<(i32, u32)> {
    let mut year = start.year();
    let mut month = start.month();
    let last_included = end - chrono::Duration::microseconds(1);
    let end_key = (last_included.year(), last_included.month());
    let mut out = Vec::new();
    while (year, month) <= end_key {
        out.push((year, month));
        if month == 12 {
            year += 1;
            month = 1;
        } else {
            month += 1;
        }
    }
    out
}

/// True when `root` already holds the frozen bot's daily spot store: any
/// `bars/asset=*/interval_s=86400` partition under it. Perp minutes
/// (interval_s=60) belong in their own root - writing them alongside the
/// frozen bot's daily spot bars would let `pull-binance-perp` silently
/// commingle data the frozen bot reads with data it was never tuned against.
fn is_frozen_spot_store(root: &std::path::Path) -> bool {
    let bars_dir = root.join("bars");
    let Ok(entries) = std::fs::read_dir(&bars_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let is_asset_dir = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("asset="));
        if is_asset_dir && path.join("interval_s=86400").exists() {
            return true;
        }
    }
    false
}

async fn cmd_pull_binance_perp(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    if is_frozen_spot_store(&root) {
        return Err(
            "this data-root holds the daily spot store the frozen bot reads; perp minutes \
             belong in their own root (use --data-root data/perp)"
                .into(),
        );
    }
    // Kept in original case: resolve_asset needs the un-uppercased token to
    // recognise HL's lowercase-`k` 1000-prefix coins (kPEPE, kBONK, ...).
    let assets = need(args, "--assets")?
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if assets.is_empty() {
        return Err("no assets given".into());
    }
    let start = parse_date(&need(args, "--start")?)?;
    let end = parse_date(&need(args, "--end")?)?;
    if start >= end {
        return Err("--start must precede --end".into());
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("ai-trader-rust/0.1 (public archive)")
        .build()
        .map_err(|e| e.to_string())?;

    // Binance only publishes a MONTHLY archive zip once the month closes, so
    // the month `today_utc` falls in (and any later enumerated month, which
    // can only arise from a future --end) can never be fetched that way -
    // it's pulled a day at a time from the daily archive instead.
    let today_utc = Utc::now().date_naive();
    let current_month = (today_utc.year(), today_utc.month());

    let months = months(start, end);
    let mut total = 0usize;
    for asset in &assets {
        let (store_asset, symbol) = resolve_asset(asset);
        let Some(symbol) = symbol else {
            println!("{store_asset}: skipped (not listed on Binance UM)");
            continue;
        };
        for &(year, month) in &months {
            if (year, month) >= current_month {
                for day in open_month_days(year, month, today_utc) {
                    let bytes = fetch_um_day(&client, &symbol, day).await?;
                    record_fetch(
                        &root,
                        &store_asset,
                        &day.to_string(),
                        "not yet published",
                        bytes,
                        start,
                        end,
                        &mut total,
                    )?;
                }
            } else {
                let label = format!("{year:04}-{month:02}");
                let bytes = fetch_um_month(&client, &symbol, year, month).await?;
                record_fetch(
                    &root,
                    &store_asset,
                    &label,
                    "not listed",
                    bytes,
                    start,
                    end,
                    &mut total,
                )?;
            }
        }
    }
    println!("wrote {total} bars total");
    Ok(())
}

/// Parse and store one fetched zip (or print the 404 skip line), sharing the
/// bookkeeping between the monthly and daily fetch paths.
#[allow(clippy::too_many_arguments)]
fn record_fetch(
    root: &std::path::Path,
    store_asset: &str,
    label: &str,
    not_found_reason: &str,
    bytes: Option<Vec<u8>>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    total: &mut usize,
) -> Result<(), String> {
    match bytes {
        None => println!("{store_asset} {label}: skipped (404: {not_found_reason})"),
        Some(bytes) => {
            let bars = parse_um_klines_zip(&bytes, store_asset, start, end)?;
            println!("{store_asset} {label}: {} bars", bars.len());
            if !bars.is_empty() {
                *total += bars.len();
                store::write(root, &bars)?;
            }
        }
    }
    Ok(())
}

/// `--sources` default: pull every microstructure source. Order matches the
/// plan's product list and the order rows print in.
const DEFAULT_MICRO_SOURCES: &str = "book,flow,funding,metrics";

fn parse_micro_sources(args: &[String]) -> Result<Vec<String>, String> {
    let raw = get(args, "--sources").unwrap_or_else(|| DEFAULT_MICRO_SOURCES.to_string());
    let sources: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();
    for s in &sources {
        if !["book", "flow", "funding", "metrics"].contains(&s.as_str()) {
            return Err(format!(
                "unknown --sources value {s:?} (expected book, flow, funding, metrics)"
            ));
        }
    }
    if sources.is_empty() {
        return Err("--sources produced no values".into());
    }
    Ok(sources)
}

/// Overwrite `path` with one JSON line per row, creating parent
/// directories as needed. Used for the daily book/flow/metrics files, which
/// are wholly regenerated from that day's archive each pull (no merge
/// needed - the source day is immutable once published).
///
/// Writes go to a `.tmp` sibling first and `rename` into place at the end,
/// so a process killed mid-write leaves either the old complete file or no
/// file at all - never a truncated one. That matters here specifically
/// because the backfill's skip rule trusts any file that parses, so a
/// partial file from a killed write would be silently treated as complete
/// forever.
fn write_jsonl<T: serde::Serialize>(path: &std::path::Path, rows: &[T]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp_path = tmp_sibling(path);
    {
        let mut file =
            std::fs::File::create(&tmp_path).map_err(|e| format!("{}: {e}", tmp_path.display()))?;
        for row in rows {
            writeln!(
                file,
                "{}",
                serde_json::to_string(row).map_err(|e| e.to_string())?
            )
            .map_err(|e| format!("{}: {e}", tmp_path.display()))?;
        }
    }
    std::fs::rename(&tmp_path, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// `{path}.tmp` in the same directory as `path`, so the tmp-then-rename in
/// `write_jsonl` and the other whole-file writers below stays on the same
/// filesystem (a cross-filesystem rename is not atomic).
pub(crate) fn tmp_sibling(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Read a symbol's existing funding file (if any) so a fresh pull can merge
/// into it rather than clobbering earlier months. A missing file is an
/// empty history, not an error.
fn read_funding_jsonl(path: &std::path::Path) -> Result<Vec<binance_micro::FundingRow>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", path.display())))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

async fn cmd_pull_binance_micro(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let assets = need(args, "--assets")?
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if assets.is_empty() {
        return Err("no assets given".into());
    }
    let start = parse_date(&need(args, "--start")?)?;
    let end = parse_date(&need(args, "--end")?)?;
    if start >= end {
        return Err("--start must precede --end".into());
    }
    let sources = parse_micro_sources(args)?;

    // aggTrades days can run tens of MB compressed for a busy symbol - a
    // longer timeout than the perp puller's, since a slow connection
    // shouldn't abort a download that would otherwise succeed.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .user_agent("ai-trader-rust/0.1 (public archive)")
        .build()
        .map_err(|e| e.to_string())?;

    let micro_root = root.join("binance-micro");
    let days = binance_micro::days(start, end);
    let month_list = months(start, end);

    let mut totals: BTreeMap<&str, usize> = BTreeMap::new();

    for asset in &assets {
        let (store_asset, symbol) = resolve_asset(asset);
        let Some(symbol) = symbol else {
            println!("{store_asset}: skipped (not listed on Binance UM)");
            continue;
        };

        if sources.iter().any(|s| s == "book") {
            for &day in &days {
                let bytes = binance_micro::fetch_daily(
                    &client,
                    binance_micro::KIND_BOOK_DEPTH,
                    &symbol,
                    day,
                )
                .await?;
                match bytes {
                    None => println!("{symbol} book {day}: skipped (404: not yet published)"),
                    Some(bytes) => {
                        let rows = binance_micro::parse_book_depth_zip(&bytes, &symbol)?;
                        println!("{symbol} book {day}: {} minute(s)", rows.len());
                        let path = micro_root
                            .join("book")
                            .join(&symbol)
                            .join(format!("{day}.jsonl"));
                        write_jsonl(&path, &rows)?;
                        *totals.entry("book").or_default() += rows.len();
                    }
                }
            }
        }

        if sources.iter().any(|s| s == "flow") {
            for &day in &days {
                let bytes = binance_micro::fetch_daily(
                    &client,
                    binance_micro::KIND_AGG_TRADES,
                    &symbol,
                    day,
                )
                .await?;
                match bytes {
                    None => println!("{symbol} flow {day}: skipped (404: not yet published)"),
                    Some(bytes) => {
                        let rows = binance_micro::parse_agg_trades_zip(&bytes, &symbol)?;
                        println!("{symbol} flow {day}: {} minute(s)", rows.len());
                        let path = micro_root
                            .join("flow")
                            .join(&symbol)
                            .join(format!("{day}.jsonl"));
                        write_jsonl(&path, &rows)?;
                        *totals.entry("flow").or_default() += rows.len();
                    }
                }
            }
        }

        if sources.iter().any(|s| s == "metrics") {
            for &day in &days {
                let bytes =
                    binance_micro::fetch_daily(&client, binance_micro::KIND_METRICS, &symbol, day)
                        .await?;
                match bytes {
                    None => println!("{symbol} metrics {day}: skipped (404: not yet published)"),
                    Some(bytes) => {
                        let rows = binance_micro::parse_metrics_zip(&bytes, &symbol)?;
                        println!("{symbol} metrics {day}: {} row(s)", rows.len());
                        let path = micro_root
                            .join("metrics")
                            .join(&symbol)
                            .join(format!("{day}.jsonl"));
                        write_jsonl(&path, &rows)?;
                        *totals.entry("metrics").or_default() += rows.len();
                    }
                }
            }
        }

        if sources.iter().any(|s| s == "funding") {
            let path = micro_root.join("funding").join(format!("{symbol}.jsonl"));
            let mut existing = read_funding_jsonl(&path)?;
            for &(year, month) in &month_list {
                let bytes = binance_micro::fetch_monthly(
                    &client,
                    binance_micro::KIND_FUNDING_RATE,
                    &symbol,
                    year,
                    month,
                )
                .await?;
                match bytes {
                    None => {
                        println!("{symbol} funding {year:04}-{month:02}: skipped (404: not listed)")
                    }
                    Some(bytes) => {
                        let rows = binance_micro::parse_funding_rate_zip(&bytes, &symbol)?;
                        println!(
                            "{symbol} funding {year:04}-{month:02}: {} row(s)",
                            rows.len()
                        );
                        existing = binance_micro::merge_funding_rows(existing, rows);
                    }
                }
            }
            write_jsonl(&path, &existing)?;
            *totals.entry("funding").or_default() += existing.len();
        }
    }

    for (source, n) in &totals {
        println!("{source}: {n} row(s) total");
    }
    Ok(())
}

/// How many levels of each side we keep per snapshot. Not exposed on the
/// CLI: the cost-summary consumer (Task 4) walks at most a handful of
/// levels to price a cross, and 20 is generous headroom for that without
/// writing the whole book.
const BOOK_DEPTH: usize = 20;

async fn cmd_record_books(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let seconds: u64 = need(args, "--seconds")?
        .parse()
        .map_err(|e| format!("bad --seconds: {e}"))?;
    let interval: u64 = need(args, "--interval")?
        .parse()
        .map_err(|e| format!("bad --interval: {e}"))?;
    if interval == 0 {
        return Err("--interval must be > 0".into());
    }

    let info = Info::new(MAINNET).map_err(|e| e.to_string())?;

    let coins = match (get(args, "--assets"), get(args, "--top")) {
        (Some(list), _) => list
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        (None, Some(n)) => {
            let n: usize = n.parse().map_err(|e| format!("bad --top: {e}"))?;
            let mut ctxs = info.asset_ctxs().await.map_err(|e| e.to_string())?;
            // asset_ctxs() reports every market the venue lists, delisted
            // ones included (see its doc comment) - a delisted market has no
            // real trading and no volume, so filtering on positive volume is
            // the cheap stand-in for a delisted check without a second
            // round trip to raw `meta()` for the flag itself.
            ctxs.retain(|(_, ctx)| ctx.day_volume_usd().unwrap_or(0.0) > 0.0);
            ctxs.sort_by(|a, b| {
                b.1.day_volume_usd()
                    .unwrap_or(0.0)
                    .total_cmp(&a.1.day_volume_usd().unwrap_or(0.0))
            });
            ctxs.into_iter().take(n).map(|(name, _)| name).collect()
        }
        (None, None) => return Err("need --assets A,B,C or --top N".into()),
    };
    if coins.is_empty() {
        return Err("no assets to record".into());
    }

    for coin in &coins {
        if binance_um_symbol(coin).is_none() {
            println!("{coin}: no Binance UM coverage (fine for book recording, matters once this is paired with bar history)");
        }
    }

    let books_dir = root.join("books");
    std::fs::create_dir_all(&books_dir).map_err(|e| e.to_string())?;

    let total = std::time::Duration::from_secs(seconds);
    let interval_dur = std::time::Duration::from_secs(interval);
    let approx_rounds = (seconds / interval).max(1);
    println!(
        "recording {} coin(s) every {interval}s for up to {seconds}s (~{approx_rounds} rounds)",
        coins.len()
    );

    // Bounded by wall clock, not round count: fetch latency is additive per
    // round, so a fixed round count drifts the run arbitrarily long past
    // --seconds (see docs/scalper-research.md for the incident this fixed).
    // The guarantee is "no new round starts after the deadline" - an
    // in-flight round always finishes, so the process can still overrun
    // --seconds by up to one round's fetch latency.
    let started = std::time::Instant::now();
    while started.elapsed() < total {
        let round_started = std::time::Instant::now();

        // Recomputed every round rather than once up front: a run that
        // straddles UTC midnight should roll onto the next day's file, not
        // keep appending to yesterday's.
        let day = Utc::now().format("%Y-%m-%d");
        let path = books_dir.join(format!("{day}.jsonl"));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;

        for coin in &coins {
            let book = match info.l2_book(coin).await {
                Ok(book) => book,
                Err(e) => {
                    eprintln!("{coin}: {e}");
                    continue;
                }
            };
            let ts_ms = Utc::now().timestamp_millis();
            match books::snapshot_from_book(coin, ts_ms, &book, BOOK_DEPTH) {
                Ok(snapshot) => {
                    let line = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
                    if let Err(e) = writeln!(file, "{line}") {
                        eprintln!("{coin}: write failed: {e}");
                    }
                }
                Err(e) => eprintln!("{e}"),
            }
        }
        file.flush()
            .map_err(|e| format!("{}: {e}", path.display()))?;

        let sleep_for = next_sleep(interval_dur, round_started.elapsed());
        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
    }
    println!("done");
    Ok(())
}

/// The pacing decision between rounds: sleep only the remainder of
/// `interval` after a round took `round_elapsed`, so cadence self-corrects
/// instead of drifting when fetch latency eats into the interval. A round
/// that overran the interval entirely sleeps zero rather than going
/// negative.
fn next_sleep(
    interval: std::time::Duration,
    round_elapsed: std::time::Duration,
) -> std::time::Duration {
    interval.saturating_sub(round_elapsed)
}

/// `--notionals` default: round dollar sizes spanning a scalper's likely
/// clip size, small enough that a healthy book should absorb the smallest
/// without complaint.
const DEFAULT_NOTIONALS: &str = "1000,5000,20000";

fn cmd_summarize_costs(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let start_str = need(args, "--start")?;
    let end_str = need(args, "--end")?;
    let start = parse_date(&start_str)?;
    let end = parse_date(&end_str)?;
    if start > end {
        return Err("--start must not be after --end".into());
    }
    let notionals: Vec<f64> = get(args, "--notionals")
        .unwrap_or_else(|| DEFAULT_NOTIONALS.to_string())
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<f64>()
                .map_err(|e| format!("bad --notionals value {v:?}: {e}"))
        })
        .collect::<Result<_, _>>()?;
    if notionals.is_empty() {
        return Err("--notionals produced no values".into());
    }

    let books_dir = root.join("books");
    let mut snapshots = Vec::new();
    let mut day = start;
    while day <= end {
        let path = books_dir.join(format!("{}.jsonl", day.format("%Y-%m-%d")));
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for (i, line) in text.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let snap: BookSnapshot = serde_json::from_str(line)
                        .map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
                    snapshots.push(snap);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("{}: no data (skipped)", path.display());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        day += chrono::Duration::days(1);
    }
    if snapshots.is_empty() {
        return Err("no book snapshots found in range".into());
    }

    let summary = summarize(&snapshots, &notionals);

    let costs_dir = root.join("costs");
    std::fs::create_dir_all(&costs_dir).map_err(|e| e.to_string())?;
    let out_path = costs_dir.join(format!("summary-{start_str}-{end_str}.json"));
    let json = serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?;
    let tmp_path = tmp_sibling(&out_path);
    std::fs::write(&tmp_path, &json).map_err(|e| format!("{}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;

    print_cost_table(&summary);
    println!("wrote {}", out_path.display());
    Ok(())
}

/// Sorted by spread (tightest first): the columns a reader scans to pick
/// which coins are cheap enough to trade at scalper size.
fn print_cost_table(summary: &BTreeMap<String, CostSummary>) {
    let mut rows: Vec<(&String, &CostSummary)> = summary.iter().collect();
    rows.sort_by(|a, b| a.1.spread_bps_median.total_cmp(&b.1.spread_bps_median));

    println!(
        "{:<8} {:>6} {:>11} {:>11} {:>16}  cross_bps",
        "coin", "n", "spread_bps", "p75_bps", "top_depth_usd"
    );
    for (coin, s) in rows {
        let cross: Vec<String> = s
            .cross_bps
            .iter()
            .map(|(notional, bps)| match bps {
                Some(v) => format!("{notional}=>{v:.2}"),
                None => format!("{notional}=>thin"),
            })
            .collect();
        println!(
            "{:<8} {:>6} {:>11.3} {:>11.3} {:>16.0}  {}",
            coin,
            s.samples,
            s.spread_bps_median,
            s.spread_bps_p75,
            s.top_depth_usd_median,
            cross.join(" "),
        );
    }
}

async fn cmd_universe(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let top: usize = need(args, "--top")?
        .parse()
        .map_err(|e| format!("bad --top: {e}"))?;

    let exclude: Vec<String> = get(args, "--exclude")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect();

    let info = Info::new(MAINNET).map_err(|e| e.to_string())?;
    let pairs = info.asset_ctxs().await.map_err(|e| e.to_string())?;

    let candidates = select_candidates(&pairs, top, &exclude);

    // Write JSON
    let universe_dir = root.clone();
    std::fs::create_dir_all(&universe_dir).map_err(|e| e.to_string())?;
    let out_path = universe_dir.join("scalper-universe.json");
    let json = serde_json::to_string_pretty(&candidates).map_err(|e| e.to_string())?;
    let tmp_path = tmp_sibling(&out_path);
    std::fs::write(&tmp_path, &json).map_err(|e| format!("{}: {e}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;

    // Print table
    println!("{:<12} {:>15}  {}", "coin", "day_volume_usd", "binance_um");
    for candidate in &candidates {
        let binance_marker = candidate.binance_um.as_deref().unwrap_or("NO-BINANCE");
        println!(
            "{:<12} {:>15.0}  {}",
            candidate.coin, candidate.day_volume_usd, binance_marker
        );
    }

    println!("wrote {}", out_path.display());
    Ok(())
}

/// `--horizons` / `--stride` defaults: a scalper cares about the next
/// 15-60 minutes, and a 5-minute stride keeps adjacent training rows from
/// sharing almost all of their trailing-window inputs.
const DEFAULT_HORIZONS: &str = "15,30,60";
const DEFAULT_STRIDE: usize = 5;

#[derive(serde::Serialize)]
struct Manifest<'a> {
    kind: &'static str,
    feature_set_version: &'static str,
    features: &'a [&'static str],
    horizons_min: &'a [i64],
    stride_min: usize,
    assets: &'a [String],
}

/// The first bar's date where each of the four micro sources has ALL of
/// its own fields populated - "first date this source could have
/// contributed to a surviving row". Reported separately per source rather
/// than as one combined date because funding has unlimited lookback (per
/// the plan's causal rule, "latest rate at or before ts", no staleness
/// window) and so is typically present from bar one once a single funding
/// pull has landed - collapsing that into a single "micro coverage from"
/// date would read as the whole matrix being warm from the very start,
/// when book/flow (120s tolerance) and metrics (10m tolerance) - the
/// sources that actually gate most rows - start covering much later.
struct CoverageStarts {
    book: Option<chrono::NaiveDate>,
    flow: Option<chrono::NaiveDate>,
    metrics: Option<chrono::NaiveDate>,
    funding: Option<chrono::NaiveDate>,
}

fn coverage_starts(
    bars: &[features_crypto::Bar],
    micro: &[Option<features_scalper::MicroMinute>],
) -> CoverageStarts {
    let first_where = |pred: &dyn Fn(&features_scalper::MicroMinute) -> bool| {
        bars.iter()
            .zip(micro.iter())
            .find(|(_, m)| m.as_ref().is_some_and(|mm| pred(mm)))
            .map(|(b, _)| b.ts_utc.date_naive())
    };
    CoverageStarts {
        book: first_where(&|mm| {
            mm.bid_02.is_some() && mm.ask_02.is_some() && mm.bid_10.is_some() && mm.ask_10.is_some()
        }),
        flow: first_where(&|mm| mm.spread_bps.is_some() && mm.taker_buy_ratio.is_some()),
        metrics: first_where(&|mm| mm.oi_value.is_some() && mm.taker_ls_ratio.is_some()),
        funding: first_where(&|mm| mm.funding_rate.is_some()),
    }
}

fn coverage_date_or_none(d: Option<chrono::NaiveDate>) -> String {
    d.map(|d| d.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn cmd_training_matrix(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let universe_path = PathBuf::from(need(args, "--universe")?);
    let start = parse_date(&need(args, "--start")?)?;
    let end = parse_date(&need(args, "--end")?)?;
    if start >= end {
        return Err("--start must precede --end".into());
    }
    let out_path = PathBuf::from(need(args, "--out")?);
    let stride: usize = match get(args, "--stride") {
        Some(v) => v.parse().map_err(|e| format!("bad --stride: {e}"))?,
        None => DEFAULT_STRIDE,
    };
    let horizons: Vec<i64> = get(args, "--horizons")
        .unwrap_or_else(|| DEFAULT_HORIZONS.to_string())
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<i64>()
                .map_err(|e| format!("bad --horizons value {v:?}: {e}"))
        })
        .collect::<Result<_, _>>()?;
    if horizons.is_empty() {
        return Err("--horizons produced no values".into());
    }
    let micro_root: Option<PathBuf> = get(args, "--micro-root").map(PathBuf::from);

    let text = std::fs::read_to_string(&universe_path)
        .map_err(|e| format!("{}: {e}", universe_path.display()))?;
    let candidates: Vec<universe::Candidate> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", universe_path.display()))?;

    let perp_root = root.join("perp");

    // BTC context (btc_ret_5, rel_ret_5) is required for every asset's
    // feature row, including BTC's own - so this is a hard failure, not a
    // skip-with-warning like a missing per-asset store dir below.
    let btc_bars = store::read_asset(&perp_root, 60, "BTC")?;
    if btc_bars.is_empty() {
        return Err(format!(
            "no BTC bars in {} - BTC context (btc_ret_5, rel_ret_5) is required for every \
             asset's feature row; pull-binance-perp --assets BTC first",
            perp_root.display()
        ));
    }

    let start_ts = start.timestamp();
    let end_ts = end.timestamp();

    let mut assets: Vec<String> = Vec::new();
    let mut all_rows: Vec<matrix::MatrixRow> = Vec::new();

    for candidate in &candidates {
        if candidate.binance_um.is_none() {
            continue;
        }
        let store_key = candidate.coin.to_uppercase();
        let bars = store::read_asset(&perp_root, 60, &store_key)?;
        if bars.is_empty() {
            println!(
                "{}: skipped (no bars in {})",
                candidate.coin,
                perp_root.display()
            );
            continue;
        }

        let micro: Vec<Option<features_scalper::MicroMinute>> = match &micro_root {
            Some(root) => {
                // `binance_um.is_none()` skipped this candidate above, so
                // the symbol is always present here.
                let symbol = candidate.binance_um.as_ref().expect("checked above");
                micro_join::load_micro_series(root, symbol, &bars)?
            }
            None => vec![None; bars.len()],
        };
        let cov = coverage_starts(&bars, &micro);

        let feature_rows = features_scalper::compute(&bars, &btc_bars, &micro)?;
        let fwd = matrix::forward_returns_bps(&bars, &horizons);
        let mut rows = matrix::matrix_rows(&feature_rows, &fwd, stride, &candidate.coin);
        rows.retain(|r| r.ts >= start_ts && r.ts < end_ts);

        // "M" = every stride-sampled bar in the requested window, before
        // the fully-warm/micro-coverage filter matrix_rows applies - so a
        // low N/M ratio reads as "coverage/warm-up ate rows", not "the
        // window itself is small".
        let m = bars
            .iter()
            .enumerate()
            .filter(|(idx, b)| {
                idx % stride.max(1) == 0
                    && b.ts_utc.timestamp() >= start_ts
                    && b.ts_utc.timestamp() < end_ts
            })
            .count();
        println!(
            "{}: kept {} of {} (book from {}, flow from {}, metrics from {}, funding from {})",
            candidate.coin,
            rows.len(),
            m,
            coverage_date_or_none(cov.book),
            coverage_date_or_none(cov.flow),
            coverage_date_or_none(cov.metrics),
            coverage_date_or_none(cov.funding),
        );
        assets.push(candidate.coin.clone());
        all_rows.extend(rows);
    }

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    // tmp-then-rename: this matrix is a gate input, so a process killed
    // mid-write must not leave a partial-but-parseable file behind (see
    // write_jsonl's doc comment for the failure mode this avoids).
    let tmp_path = tmp_sibling(&out_path);
    {
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("{}: {e}", tmp_path.display()))?;
        let manifest = Manifest {
            kind: "manifest",
            feature_set_version: features_scalper::FEATURE_SET_VERSION,
            features: &features_scalper::FEATURE_NAMES,
            horizons_min: &horizons,
            stride_min: stride,
            assets: &assets,
        };
        writeln!(
            file,
            "{}",
            serde_json::to_string(&manifest).map_err(|e| e.to_string())?
        )
        .map_err(|e| format!("{}: {e}", tmp_path.display()))?;
        // Rows are written one asset block at a time (the order `all_rows`
        // was built in above), ts ascending within each block. That order
        // carries no meaning for training: the trainer shuffles/splits by
        // time itself.
        for row in &all_rows {
            writeln!(
                file,
                "{}",
                serde_json::to_string(row).map_err(|e| e.to_string())?
            )
            .map_err(|e| format!("{}: {e}", tmp_path.display()))?;
        }
    }
    std::fs::rename(&tmp_path, &out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;

    println!(
        "wrote {} rows for {} asset(s) to {}",
        all_rows.len(),
        assets.len(),
        out_path.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("pull-binance-perp") => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::FAILURE;
                }
            };
            runtime.block_on(cmd_pull_binance_perp(&args[1..]))
        }
        Some("pull-binance-micro") => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::FAILURE;
                }
            };
            runtime.block_on(cmd_pull_binance_micro(&args[1..]))
        }
        Some("record-books") => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::FAILURE;
                }
            };
            runtime.block_on(cmd_record_books(&args[1..]))
        }
        Some("universe") => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("error: {error}");
                    return ExitCode::FAILURE;
                }
            };
            runtime.block_on(cmd_universe(&args[1..]))
        }
        Some("summarize-costs") => cmd_summarize_costs(&args[1..]),
        Some("training-matrix") => cmd_training_matrix(&args[1..]),
        Some("binance-costs") => binance_costs::cmd_binance_costs(&args[1..]),
        Some("gate") => gate::cmd_gate(&args[1..]),
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown command {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_sleep_sleeps_the_remainder_of_the_interval() {
        assert_eq!(
            next_sleep(
                std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(3)
            ),
            std::time::Duration::from_secs(7)
        );
    }

    #[test]
    fn next_sleep_sleeps_zero_when_the_round_overran_the_interval() {
        assert_eq!(
            next_sleep(
                std::time::Duration::from_secs(10),
                std::time::Duration::from_secs(12)
            ),
            std::time::Duration::from_secs(0)
        );
    }

    #[test]
    fn write_jsonl_leaves_no_tmp_behind_and_the_target_parses() {
        let dir = std::env::temp_dir().join(format!(
            "scalper-write-jsonl-{}-{}",
            std::process::id(),
            "clean"
        ));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("2026-08-13.jsonl");

        write_jsonl(&path, &[1u32, 2, 3]).unwrap();

        assert!(path.exists(), "target file must exist after a clean write");
        assert!(
            !tmp_sibling(&path).exists(),
            "the .tmp sibling must not survive a successful write"
        );
        let rows: Vec<u32> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows, vec![1, 2, 3]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Simulates a kill mid-write: a `.tmp` file is already sitting next to
    /// the target (as if a previous process died after `File::create` but
    /// before `rename`). A fresh `write_jsonl` call must still land a
    /// correct target and replace that stale `.tmp` - the tmp-then-rename
    /// shape, not the OS's atomicity guarantee itself, is what's under test.
    #[test]
    fn write_jsonl_replaces_a_stale_tmp_left_by_an_interrupted_write() {
        let dir = std::env::temp_dir().join(format!(
            "scalper-write-jsonl-{}-{}",
            std::process::id(),
            "interrupted"
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-08-13.jsonl");
        std::fs::write(&path, "{\"stale\":true}\n").unwrap();
        std::fs::write(tmp_sibling(&path), "garbage from a killed write").unwrap();

        write_jsonl(&path, &[42u32]).unwrap();

        assert!(
            !tmp_sibling(&path).exists(),
            "a fresh write must consume/replace any leftover .tmp"
        );
        let rows: Vec<u32> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows, vec![42], "the target must hold the new write, not the stale content");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_asset_reads_the_k_prefix_before_uppercasing_the_store_key() {
        assert_eq!(
            resolve_asset("kPEPE"),
            ("KPEPE".to_string(), Some("1000PEPEUSDT".to_string()))
        );
        assert_eq!(
            resolve_asset("BTC"),
            ("BTC".to_string(), Some("BTCUSDT".to_string()))
        );
        assert_eq!(resolve_asset("HYPE").1, None);
        // Capital-K tickers (KAITO, KAVA, ...) are real HL coins, not the
        // 1000-prefix shorthand - a case-insensitive `k` match would corrupt
        // them into a nonexistent "1000AITOUSDT" symbol.
        assert_eq!(resolve_asset("KAITO").1, Some("KAITOUSDT".to_string()));
    }

    fn bar_at(ts: DateTime<Utc>) -> features_crypto::Bar {
        features_crypto::Bar {
            ts_utc: ts,
            asset: "TEST".into(),
            interval_s: 60,
            open: 99.9,
            high: 100.1,
            low: 99.8,
            close: 100.0,
            volume: 5.0,
            quote_volume: Some(500.0),
            trades: Some(10),
        }
    }

    /// Funding has unlimited lookback (the plan's "latest rate at or
    /// before ts", no staleness window), so it is typically present from
    /// the very first bar once a single funding pull has landed - but
    /// book/flow/metrics (the sources that actually gate row survival)
    /// start covering later. `coverage_starts` must report each source's
    /// own first-covered date, not let funding's early date collapse the
    /// whole diagnostic into "covered from day one".
    #[test]
    fn coverage_starts_reports_each_source_independently_not_collapsed_by_funding() {
        use chrono::TimeZone;
        let day1 = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let day2 = Utc.with_ymd_and_hms(2026, 6, 2, 0, 0, 0).unwrap();
        let day3 = Utc.with_ymd_and_hms(2026, 6, 3, 0, 0, 0).unwrap();
        let bars = vec![bar_at(day1), bar_at(day2), bar_at(day3)];

        let micro = vec![
            Some(features_scalper::MicroMinute {
                ts_s: day1.timestamp(),
                funding_rate: Some(0.0001),
                ..Default::default()
            }),
            Some(features_scalper::MicroMinute {
                ts_s: day2.timestamp(),
                funding_rate: Some(0.0001),
                spread_bps: Some(3.0),
                taker_buy_ratio: Some(0.5),
                ..Default::default()
            }),
            Some(features_scalper::MicroMinute {
                ts_s: day3.timestamp(),
                funding_rate: Some(0.0001),
                spread_bps: Some(3.0),
                taker_buy_ratio: Some(0.5),
                bid_02: Some(10.0),
                ask_02: Some(9.0),
                bid_10: Some(50.0),
                ask_10: Some(48.0),
                oi_value: Some(1_000.0),
                taker_ls_ratio: Some(1.1),
            }),
        ];

        let cov = coverage_starts(&bars, &micro);
        assert_eq!(
            cov.funding,
            Some(day1.date_naive()),
            "funding's unlimited lookback covers from bar one"
        );
        assert_eq!(cov.flow, Some(day2.date_naive()));
        assert_eq!(cov.book, Some(day3.date_naive()));
        assert_eq!(cov.metrics, Some(day3.date_naive()));
        assert_ne!(
            cov.book, cov.funding,
            "funding's early coverage must not collapse into book's later start"
        );
    }

    #[test]
    fn coverage_starts_is_none_for_a_source_that_never_appears() {
        use chrono::TimeZone;
        let day1 = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let bars = vec![bar_at(day1)];
        let micro = vec![Some(features_scalper::MicroMinute {
            ts_s: day1.timestamp(),
            funding_rate: Some(0.0001),
            ..Default::default()
        })];
        let cov = coverage_starts(&bars, &micro);
        assert_eq!(cov.funding, Some(day1.date_naive()));
        assert_eq!(cov.book, None);
        assert_eq!(cov.flow, None);
        assert_eq!(cov.metrics, None);
    }

    #[tokio::test]
    async fn pull_binance_perp_refuses_a_data_root_that_holds_the_frozen_spot_store() {
        let root = std::env::temp_dir().join(format!("scalper-data-guard-{}", std::process::id()));
        std::fs::create_dir_all(root.join("bars").join("asset=BTC").join("interval_s=86400"))
            .unwrap();

        let args = vec![
            "--data-root".to_string(),
            root.to_string_lossy().to_string(),
            "--assets".to_string(),
            "BTC".to_string(),
            "--start".to_string(),
            "2026-01-01".to_string(),
            "--end".to_string(),
            "2026-01-02".to_string(),
        ];
        let err = cmd_pull_binance_perp(&args).await.unwrap_err();
        assert!(
            err.contains("spot store"),
            "expected the guard's error to mention the spot store, got: {err}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    fn synthetic_bars(asset: &str, base: DateTime<Utc>, n: i64) -> Vec<features_crypto::Bar> {
        (0..n)
            .map(|i| {
                let ts = base + chrono::Duration::minutes(i);
                let close = 100.0 + i as f64 * 0.01;
                features_crypto::Bar {
                    ts_utc: ts,
                    asset: asset.to_string(),
                    interval_s: 60,
                    open: close * 0.999,
                    high: close * 1.001,
                    low: close * 0.998,
                    close,
                    volume: 5.0,
                    quote_volume: Some(close * 5.0),
                    trades: Some(10),
                }
            })
            .collect()
    }

    /// Writes book/flow/metrics rows for every minute `[base, base+n)` and
    /// one funding row well before `base`, so `micro_join::load_micro_series`
    /// finds full coverage across the whole span - fs-2 needs every one of
    /// the 12 microstructure features Some to keep a matrix row, so a test
    /// exercising "rows survive" needs this, not just bars.
    fn write_full_micro_coverage(
        data_root: &std::path::Path,
        symbol: &str,
        base: DateTime<Utc>,
        n: i64,
    ) {
        let micro_root = data_root.join("binance-micro");
        let day = base.date_naive();
        let mut book_lines = String::new();
        let mut flow_lines = String::new();
        let mut metrics_lines = String::new();
        for i in 0..n {
            let ts = (base + chrono::Duration::minutes(i)).timestamp();
            let mut bands = std::collections::BTreeMap::new();
            bands.insert("-0.2".to_string(), 100.0);
            bands.insert("0.2".to_string(), 90.0);
            bands.insert("-1.0".to_string(), 500.0);
            bands.insert("1.0".to_string(), 480.0);
            let book = binance_micro::BookMinute { ts_s: ts, bands };
            book_lines.push_str(&serde_json::to_string(&book).unwrap());
            book_lines.push('\n');

            let flow = binance_micro::FlowMinute {
                ts_s: ts,
                spread_bps_med: Some(4.0),
                n_spread_samples: 10,
                distinct_bids: 5,
                taker_buy_ratio: 0.55,
                n_trades: 50,
                notional: 1_000.0,
            };
            flow_lines.push_str(&serde_json::to_string(&flow).unwrap());
            flow_lines.push('\n');

            let metrics = binance_micro::MetricsRow {
                ts_s: ts,
                sum_open_interest: Some(1.0),
                sum_open_interest_value: Some(5_000.0 + i as f64),
                count_toptrader_long_short_ratio: Some(1.0),
                sum_toptrader_long_short_ratio: Some(1.0),
                count_long_short_ratio: Some(1.0),
                sum_taker_long_short_vol_ratio: Some(1.2),
            };
            metrics_lines.push_str(&serde_json::to_string(&metrics).unwrap());
            metrics_lines.push('\n');
        }

        let book_dir = micro_root.join("book").join(symbol);
        std::fs::create_dir_all(&book_dir).unwrap();
        std::fs::write(book_dir.join(format!("{day}.jsonl")), book_lines).unwrap();

        let flow_dir = micro_root.join("flow").join(symbol);
        std::fs::create_dir_all(&flow_dir).unwrap();
        std::fs::write(flow_dir.join(format!("{day}.jsonl")), flow_lines).unwrap();

        let metrics_dir = micro_root.join("metrics").join(symbol);
        std::fs::create_dir_all(&metrics_dir).unwrap();
        std::fs::write(metrics_dir.join(format!("{day}.jsonl")), metrics_lines).unwrap();

        let funding_dir = micro_root.join("funding");
        std::fs::create_dir_all(&funding_dir).unwrap();
        let funding = binance_micro::FundingRow {
            ts_s: (base - chrono::Duration::hours(1)).timestamp(),
            funding_interval_hours: 8.0,
            funding_rate: 0.0001,
        };
        std::fs::write(
            funding_dir.join(format!("{symbol}.jsonl")),
            serde_json::to_string(&funding).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn training_matrix_skips_a_universe_candidate_with_no_bars_in_the_store() {
        use chrono::TimeZone;

        let root = std::env::temp_dir().join(format!(
            "scalper-training-matrix-{}-{}",
            std::process::id(),
            "skip-warn"
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();

        let perp_root = root.join("perp");
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        store::write(&perp_root, &synthetic_bars("BTC", base, 300)).unwrap();
        store::write(&perp_root, &synthetic_bars("KTEST", base, 300)).unwrap();
        // "KMISSING" deliberately has no bars written anywhere: its
        // asset=KMISSING directory never gets created, exercising the
        // skip-with-warning path rather than a hard error.
        write_full_micro_coverage(&root, "BTCUSDT", base, 300);
        write_full_micro_coverage(&root, "TESTUSDT", base, 300);

        let universe_path = root.join("scalper-universe.json");
        let universe_json = serde_json::json!([
            {"coin": "BTC", "day_volume_usd": 3.0, "binance_um": "BTCUSDT"},
            {"coin": "kTEST", "day_volume_usd": 2.0, "binance_um": "TESTUSDT"},
            {"coin": "kMISSING", "day_volume_usd": 1.0, "binance_um": "MISSINGUSDT"},
        ]);
        std::fs::write(
            &universe_path,
            serde_json::to_string(&universe_json).unwrap(),
        )
        .unwrap();

        let out_path = root.join("matrix.jsonl");
        let args = vec![
            "--data-root".to_string(),
            root.to_string_lossy().to_string(),
            "--universe".to_string(),
            universe_path.to_string_lossy().to_string(),
            "--start".to_string(),
            "2026-06-01".to_string(),
            "--end".to_string(),
            "2026-06-02".to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
            "--stride".to_string(),
            "5".to_string(),
            "--horizons".to_string(),
            "15".to_string(),
            "--micro-root".to_string(),
            root.to_string_lossy().to_string(),
        ];
        cmd_training_matrix(&args).expect("a missing store dir must be a warning, not a failure");

        let content = std::fs::read_to_string(&out_path).unwrap();
        let mut lines = content.lines();
        let manifest: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(manifest["kind"], "manifest");
        assert_eq!(manifest["assets"], serde_json::json!(["BTC", "kTEST"]));

        let rows: Vec<matrix::MatrixRow> =
            lines.map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(
            !rows.is_empty(),
            "kTEST has enough bars and full micro coverage to be fully warm"
        );
        assert!(rows.iter().any(|r| r.asset == "kTEST"));
        assert!(rows.iter().all(|r| r.asset == "BTC" || r.asset == "kTEST"));
        assert!(
            rows.iter().all(|r| r.features.len() == 38),
            "fs-2 rows carry all 38 features, not fs-1's 26"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Without `--micro-root`, fs-2's matrix requires all 38 features Some
    /// to keep a row, and the 12 microstructure features are None
    /// everywhere with no micro data - so the matrix builds successfully
    /// (not an error) but keeps zero rows. This is the intended, if harsh,
    /// behavior: a matrix built with no microstructure coverage is not
    /// silently missing 12 columns, it produces no trainable rows at all.
    #[test]
    fn training_matrix_without_micro_root_keeps_zero_rows() {
        use chrono::TimeZone;

        let root = std::env::temp_dir().join(format!(
            "scalper-training-matrix-{}-{}",
            std::process::id(),
            "no-micro-root"
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();

        let perp_root = root.join("perp");
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        store::write(&perp_root, &synthetic_bars("BTC", base, 300)).unwrap();

        let universe_path = root.join("scalper-universe.json");
        std::fs::write(
            &universe_path,
            serde_json::to_string(&serde_json::json!([
                {"coin": "BTC", "day_volume_usd": 3.0, "binance_um": "BTCUSDT"},
            ]))
            .unwrap(),
        )
        .unwrap();

        let out_path = root.join("matrix.jsonl");
        let args = vec![
            "--data-root".to_string(),
            root.to_string_lossy().to_string(),
            "--universe".to_string(),
            universe_path.to_string_lossy().to_string(),
            "--start".to_string(),
            "2026-06-01".to_string(),
            "--end".to_string(),
            "2026-06-02".to_string(),
            "--out".to_string(),
            out_path.to_string_lossy().to_string(),
            "--stride".to_string(),
            "5".to_string(),
            "--horizons".to_string(),
            "15".to_string(),
        ];
        cmd_training_matrix(&args).expect("no --micro-root is a valid, just uninformative, run");

        let content = std::fs::read_to_string(&out_path).unwrap();
        let mut lines = content.lines();
        let manifest: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(manifest["feature_set_version"], "fs-rust-scalper-3");
        let rows: Vec<matrix::MatrixRow> =
            lines.map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(
            rows.is_empty(),
            "no micro coverage anywhere -> no fully-warm rows"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
