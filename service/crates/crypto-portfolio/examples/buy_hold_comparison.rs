//! Export continuous spot buy-and-hold NAV on an existing replay's daily grid.
use chrono::{DateTime, Duration, Utc};
use crypto_portfolio::store;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: buy_hold_comparison DATA_ROOT STRATEGY_FOLDS OUTPUT_DIR".into());
    }
    let root = Path::new(&args[1]);
    let source = Path::new(&args[2]);
    let out = Path::new(&args[3]);
    let mut paths: Vec<_> = fs::read_dir(source)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|path| {
        path.file_name()
            .unwrap()
            .to_str()
            .is_some_and(|s| s.starts_with("fold-") && s.ends_with(".json"))
    });
    paths.sort();
    if paths.is_empty() {
        return Err("no reference folds".into());
    }
    let mut folds = Vec::new();
    let mut sources = Vec::new();
    for path in paths {
        let bytes = fs::read(&path)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let dates: Vec<DateTime<Utc>> = value["steps"]
            .as_array()
            .ok_or("missing steps")?
            .iter()
            .map(|step| {
                step["as_of"]
                    .as_str()
                    .ok_or("missing date")?
                    .parse()
                    .map_err(|_| "invalid date")
            })
            .collect::<Result<_, _>>()?;
        if dates.len() < 2 {
            return Err("empty or short fold".into());
        }
        sources.push(json!({"file":path,"sha256":format!("{:x}",Sha256::digest(&bytes))}));
        folds.push(dates);
    }
    folds.sort_by_key(|dates| dates[0]);
    let dates: Vec<_> = folds.iter().flatten().copied().collect();
    if !dates
        .windows(2)
        .all(|pair| pair[1] - pair[0] == Duration::days(1))
    {
        return Err("reference grid is not contiguous daily observations".into());
    }
    let mut prices = Vec::new();
    let mut price_sources = Vec::new();
    for asset in ["BTC", "ETH"] {
        let bars = store::read_asset(root, 86400, asset)?;
        price_sources.push(json!({"asset":asset,"bar_content_sha256":store::content_hash(&bars)}));
        let by_date: BTreeMap<_, _> = bars.iter().map(|bar| (bar.ts_utc, bar.open)).collect();
        let marks: Vec<f64> = dates
            .iter()
            .map(|date| by_date.get(date).copied().ok_or("missing benchmark open"))
            .collect::<Result<_, _>>()?;
        if !marks.iter().all(|v| v.is_finite() && *v > 0.0) {
            return Err("invalid benchmark price".into());
        }
        prices.push(marks);
    }
    fs::create_dir_all(out)?;
    let mut summaries = Vec::new();
    for (id, btc_weight) in [("btc", 1.0), ("eth", 0.0), ("btc-eth-50-50", 0.5)] {
        // Shares are bought once at the first timestamp; no fold-boundary rebalancing.
        let wealth: Vec<f64> = (0..dates.len())
            .map(|i| {
                btc_weight * prices[0][i] / prices[0][0]
                    + (1.0 - btc_weight) * prices[1][i] / prices[1][0]
            })
            .collect();
        let dir = out.join(id);
        fs::create_dir_all(&dir)?;
        let mut offset = 0;
        let mut chained = 1.0;
        for (i, fold) in folds.iter().enumerate() {
            // Rescaling preserves overnight returns across artificial fold boundaries.
            let base = if offset == 0 { 1.0 } else { wealth[offset - 1] };
            let steps: Vec<_> = fold
                .iter()
                .enumerate()
                .map(|(j, date)| json!({"as_of":date,"nav":100000.0*wealth[offset+j]/base}))
                .collect();
            chained *= wealth[offset + fold.len() - 1] / base;
            let file = dir.join(format!("fold-{}.json", i + 1));
            let document = json!({"initial_cash":100000.0,"steps":steps,
                "disclosures":["Continuous spot holdings; first-day return zero; zero one-off entry/exit fees, no funding or staking. Fold NAV rescaling does not rebalance holdings."]});
            fs::write(file, serde_json::to_vec_pretty(&document)?)?;
            offset += fold.len();
        }
        if (chained - wealth.last().unwrap()).abs() > 1e-10 {
            return Err("segmented return failed endpoint parity".into());
        }
        summaries.push(json!({"id":id,"initial_btc_weight":btc_weight,"first_btc_open":prices[0][0],
            "last_btc_open":prices[0].last(),"first_eth_open":prices[1][0],"last_eth_open":prices[1].last(),
            "total_return":chained-1.0,"endpoint_parity":true}));
    }
    fs::write(
        out.join("inputs.json"),
        serde_json::to_vec_pretty(&json!({
            "first_date":dates[0],"last_date":dates.last(),"observations":dates.len(),
            "reference_folds":sources,"price_sources":price_sources,"benchmarks":summaries,
            "method":"Daily UTC opens match strategy marks. Spot shares bought once at first observation. No liquidation at end. Report mark-to-market gross of one-off trading fees; no funding or staking. Strategy retains its existing reset-fold return sequence."
        }))?,
    )?;
    Ok(())
}
