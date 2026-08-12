mod binance_um;

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, Datelike, Utc};
use crypto_portfolio::store;

use binance_um::{binance_um_symbol, fetch_um_month, parse_um_klines_zip};

const USAGE: &str = "\
usage: scalper-data <command>

  pull-binance-perp --data-root <dir> --assets BTC,ETH,... --start YYYY-MM-DD --end YYYY-MM-DD
      Bulk-load Binance USDT-perp (UM futures) monthly 1m kline archives into
      the shared Parquet bar store (interval_s=60). Assets with no UM listing
      are skipped with a warning rather than silently substituted with spot.
";

fn get(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|v| v == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn need(args: &[String], name: &str) -> Result<String, String> {
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

async fn cmd_pull_binance_perp(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
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

    let months = months(start, end);
    let mut total = 0usize;
    for asset in &assets {
        let (store_asset, symbol) = resolve_asset(asset);
        let Some(symbol) = symbol else {
            println!("{store_asset}: skipped (not listed on Binance UM)");
            continue;
        };
        for &(year, month) in &months {
            let label = format!("{year:04}-{month:02}");
            match fetch_um_month(&client, &symbol, year, month).await? {
                None => println!("{store_asset} {label}: skipped (404: not listed)"),
                Some(bytes) => {
                    let bars = parse_um_klines_zip(&bytes, &store_asset, start, end)?;
                    println!("{store_asset} {label}: {} bars", bars.len());
                    if !bars.is_empty() {
                        total += bars.len();
                        store::write(&root, &bars)?;
                    }
                }
            }
        }
    }
    println!("wrote {total} bars total");
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
}
