//! Build a decision-time cohort without requiring the following valuation bar.
use crypto_portfolio::{config::Config, lighter_history::History, store, universe};
use std::path::Path;

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "usage: lighter-decision-universe RAW SIGNAL OUTPUT CONFIG AS_OF_RFC3339".into(),
        );
    }
    let history = History::load(Path::new(&args[0]))?;
    let cfg = Config::load(Path::new(&args[3]))?;
    if cfg.interval_s != 86_400 || cfg.min_history_bars == 0 {
        return Err("daily config with positive history is required".into());
    }
    let as_of = args[4].parse().map_err(|e| format!("as_of: {e}"))?;
    history.validate_period(as_of, as_of)?;
    let bars = store::read(Path::new(&args[1]), cfg.interval_s as i32)?;
    let members = history.members_at(&bars, &cfg, as_of)?;
    let eligible = members.iter().filter(|m| m.eligible).count();
    let path = universe::write(
        Path::new(&args[2]),
        as_of,
        "native prior-day activity and completed external liquidity; decision only, no valuation prices",
        members,
        false,
    )?;
    std::fs::write(
        path.with_extension("inputs.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "native_page_sha256":history.page_sha256,
            "signal_bars_sha256":store::content_hash(&bars),
            "as_of":as_of,
            "eligible":eligible,
            "valuation":"none; this artifact supplies membership only"
        }))
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("{eligible} eligible assets -> {}", path.display());
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lighter-decision-universe: {error}");
        std::process::exit(1);
    }
}
