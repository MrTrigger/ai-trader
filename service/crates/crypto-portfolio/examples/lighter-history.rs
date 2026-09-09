//! cargo run -p crypto-portfolio --example lighter-history -- RAW SIGNAL OUT CONFIG START END
use crypto_portfolio::{config, lighter_history};

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        return Err(
            "usage: lighter-history RAW SIGNAL OUT CONFIG START_RFC3339 END_RFC3339".into(),
        );
    }
    let history = lighter_history::History::load(std::path::Path::new(&args[0]))?;
    let cfg = config::Config::load(std::path::Path::new(&args[3]))?;
    let coverage = history.build_universes(
        std::path::Path::new(&args[1]),
        std::path::Path::new(&args[2]),
        &cfg,
        args[4].parse().map_err(|e| format!("start: {e}"))?,
        args[5].parse().map_err(|e| format!("end: {e}"))?,
    )?;
    println!(
        "{} native candles, {} selected markets, {} decision days",
        history.daily.len(),
        coverage.selected_markets.len(),
        coverage.decisions.len()
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lighter-history: {error}");
        std::process::exit(1);
    }
}
