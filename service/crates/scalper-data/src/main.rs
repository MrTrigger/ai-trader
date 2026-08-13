mod binance_um;
mod books;
mod costs;
mod gate;
mod matrix;
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

  record-books --data-root <dir> --seconds N --interval N [--assets A,B | --top N]
      Poll Hyperliquid L2 books at --interval seconds for --seconds total and
      append one JSON line per snapshot per coin to
      {data-root}/books/{YYYY-MM-DD}.jsonl (UTC day, flushed every round).
      --top N picks the N largest markets by day_volume_usd() instead of a
      fixed --assets list. A coin's fetch or book error is a warning, not a
      reason to stop the run.

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

  training-matrix --data-root <dir> --universe <path> --start YYYY-MM-DD --end YYYY-MM-DD --out <path> [--stride 5] [--horizons 15,30,60]
      Build the trainer's JSONL matrix: for each universe candidate with a
      Binance UM listing, read 1m bars from {data-root}/perp (store key =
      coin.to_uppercase()), compute features-scalper's feature set (BTC
      context from store key BTC - a hard requirement, since btc_ret_5 and
      rel_ret_5 need it), join with forward returns at the given horizons,
      keep every --stride-th fully-warm row, and write a manifest line
      followed by one JSON row per line to --out. A candidate with no bars
      in the store is skipped with a warning, not a fatal error. Prints
      per-asset row counts.

  gate --matrix <path> --folds <path/folds.json> --costs <path> [--threshold-mult 1.5] [--notional 5000] --out <path>
      Walk-forward gate: for each fold in folds.json, load fold-N.json (a
      LightGBM JSON dump), predict every matrix row in that fold's test
      window via lightgbm-json, and simulate a threshold-gated long/short
      strategy net of measured round-trip costs from the plan-2 cost summary.
      Stitches per-fold daily P&L into one series and reports the
      annualized Sharpe gate (> 2.0 to PASS) to --out.
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

fn parse_date(text: &str) -> Result<DateTime<Utc>, String> {
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

    let rounds = (seconds / interval).max(1);
    println!(
        "recording {} coin(s) every {interval}s for {seconds}s ({rounds} rounds)",
        coins.len()
    );

    for round in 0..rounds {
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
        file.flush().map_err(|e| format!("{}: {e}", path.display()))?;

        if round + 1 < rounds {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        }
    }
    println!("done");
    Ok(())
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
    std::fs::write(&out_path, &json).map_err(|e| format!("{}: {e}", out_path.display()))?;

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
    std::fs::write(&out_path, &json).map_err(|e| format!("{}: {e}", out_path.display()))?;

    // Print table
    println!(
        "{:<12} {:>15}  {}",
        "coin", "day_volume_usd", "binance_um"
    );
    for candidate in &candidates {
        let binance_marker = candidate
            .binance_um
            .as_deref()
            .unwrap_or("NO-BINANCE");
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

        let feature_rows = features_scalper::compute(&bars, &btc_bars)?;
        let fwd = matrix::forward_returns_bps(&bars, &horizons);
        let mut rows = matrix::matrix_rows(&feature_rows, &fwd, stride, &candidate.coin);
        rows.retain(|r| r.ts >= start_ts && r.ts < end_ts);

        println!("{}: {} rows", candidate.coin, rows.len());
        assets.push(candidate.coin.clone());
        all_rows.extend(rows);
    }

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let mut file =
        std::fs::File::create(&out_path).map_err(|e| format!("{}: {e}", out_path.display()))?;
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
    .map_err(|e| format!("{}: {e}", out_path.display()))?;
    // Rows are written one asset block at a time (the order `all_rows` was
    // built in above), ts ascending within each block. That order carries
    // no meaning for training: the trainer shuffles/splits by time itself.
    for row in &all_rows {
        writeln!(
            file,
            "{}",
            serde_json::to_string(row).map_err(|e| e.to_string())?
        )
        .map_err(|e| format!("{}: {e}", out_path.display()))?;
    }

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
        assert_eq!(
            resolve_asset("KAITO").1,
            Some("KAITOUSDT".to_string())
        );
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
        ];
        cmd_training_matrix(&args).expect("a missing store dir must be a warning, not a failure");

        let content = std::fs::read_to_string(&out_path).unwrap();
        let mut lines = content.lines();
        let manifest: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(manifest["kind"], "manifest");
        assert_eq!(manifest["assets"], serde_json::json!(["BTC", "kTEST"]));

        let rows: Vec<matrix::MatrixRow> =
            lines.map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(!rows.is_empty(), "kTEST has enough bars to be fully warm");
        assert!(rows.iter().any(|r| r.asset == "kTEST"));
        assert!(rows.iter().all(|r| r.asset == "BTC" || r.asset == "kTEST"));
        assert!(rows.iter().all(|r| r.features.len() == 26));

        std::fs::remove_dir_all(&root).ok();
    }
}
