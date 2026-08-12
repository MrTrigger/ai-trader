//! What crossing the spread actually costs, measured instead of assumed.
//!
//! The planner's cost model carries a 0.50bp spread and an impact coefficient
//! that the plan itself discloses as assumed rather than fitted. Paper trading
//! cannot settle that — a paper fill is the real mark moved by a configured
//! `slippage_bps`, so fitting a cost model to paper fills recovers the constant
//! that was put in. The venue's resting book, however, is observable right now
//! and owes us nothing: the cost of crossing a given size is a property of the
//! book, and reading it risks no capital.
//!
//! What this still cannot see is our OWN impact — the book's reaction to being
//! hit. That needs real fills, and stays a Phase 3 measurement.
//!
//!   cargo run -p hyperliquid --example book_capture -- \
//!     --plan var/plan.json --seconds 300 --interval 10 --out var/book.jsonl
//!
//! `--plan` takes the traded names and their order sizes from a real plan, so
//! the depth is walked for the quantities actually sent. `--assets A,B` sizes
//! every name at `--notional` instead.

use std::collections::BTreeMap;
use std::time::Duration;

use hyperliquid::{Info, MAINNET};

#[derive(Clone)]
struct Target {
    asset: String,
    /// Quote-currency size of the order this run would send, one side.
    notional: f64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let seconds: u64 = get("--seconds").and_then(|v| v.parse().ok()).unwrap_or(300);
    let interval: u64 = get("--interval").and_then(|v| v.parse().ok()).unwrap_or(10);
    let out = get("--out");
    // The executor sends each order in slices an hour apart, and the book
    // refills between them, so the size that meets the book is the order
    // divided by the slice count. Walking the whole order in one clip prices
    // an execution we do not do.
    let clips: f64 = get("--clips").and_then(|v| v.parse().ok()).unwrap_or(1.0);

    let targets = match (get("--plan"), get("--assets")) {
        (Some(path), _) => from_plan(&path)?,
        (None, Some(list)) => {
            let notional: f64 = get("--notional")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5_000.0);
            list.split(',')
                .filter(|s| !s.is_empty())
                .map(|a| Target {
                    asset: a.trim().to_string(),
                    notional,
                })
                .collect()
        }
        _ => return Err("need --plan FILE or --assets A,B,C".into()),
    };
    if targets.is_empty() {
        return Err("no assets to sample".into());
    }

    let info = Info::new(MAINNET).map_err(|e| e.to_string())?;
    let rounds = (seconds / interval).max(1);
    eprintln!(
        "sampling {} names every {interval}s for {seconds}s ({rounds} rounds), \
         {clips} clip(s) per order",
        targets.len()
    );

    // Per asset: every observation, so the summary can report a spread's
    // spread. One snapshot of one name is a number; a distribution is evidence.
    let mut spread_bps: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut cross_bps: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut top_depth: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut sink = out.as_ref().map(|_| String::new());

    for round in 0..rounds {
        for t in &targets {
            let book = match info.l2_book(&t.asset).await {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{}: {e}", t.asset);
                    continue;
                }
            };
            let bids = side(&book, 0);
            let asks = side(&book, 1);
            let (Some(&(bid, _)), Some(&(ask, _))) = (bids.first(), asks.first()) else {
                continue;
            };
            let mid = (bid + ask) / 2.0;
            if mid <= 0.0 {
                continue;
            }
            spread_bps
                .entry(t.asset.clone())
                .or_default()
                .push((ask - bid) / mid * 1e4);
            // A buy crosses the asks. The half-spread is in here, which is
            // correct: it is what the order pays.
            if let Some(vwap) = walk(&asks, t.notional / clips.max(1.0)) {
                cross_bps
                    .entry(t.asset.clone())
                    .or_default()
                    .push((vwap - mid) / mid * 1e4);
            }
            top_depth
                .entry(t.asset.clone())
                .or_default()
                .push(asks.first().map(|(p, s)| p * s).unwrap_or(0.0));

            if let Some(s) = sink.as_mut() {
                s.push_str(&format!(
                    "{{\"round\":{round},\"asset\":\"{}\",\"bid\":{bid},\"ask\":{ask},\
                     \"notional\":{},\"levels_bid\":{},\"levels_ask\":{}}}\n",
                    t.asset,
                    t.notional,
                    bids.len(),
                    asks.len()
                ));
            }
        }
        if round + 1 < rounds {
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }

    if let (Some(path), Some(body)) = (out.as_ref(), sink.as_ref()) {
        std::fs::write(path, body).map_err(|e| e.to_string())?;
        eprintln!("raw snapshots -> {path}");
    }

    println!(
        "\n{:<8}{:>10}{:>12}{:>10}{:>12}{:>14}{:>8}",
        "ASSET", "ORDER $", "SPREAD bp", "med", "CROSS bp", "TOP LVL $", "n"
    );
    let mut w_spread = 0.0;
    let mut w_cross = 0.0;
    let mut w_total = 0.0;
    for t in &targets {
        let Some(sp) = spread_bps.get(&t.asset) else {
            continue;
        };
        let cr = cross_bps.get(&t.asset).cloned().unwrap_or_default();
        let dp = top_depth.get(&t.asset).cloned().unwrap_or_default();
        let (sm, cm, dm) = (median(sp), median(&cr), median(&dp));
        println!(
            "{:<8}{:>10.0}{:>12.2}{:>10.2}{:>12.2}{:>14.0}{:>8}",
            t.asset,
            t.notional,
            mean(sp),
            sm,
            cm,
            dm,
            sp.len()
        );
        w_spread += sm * t.notional;
        w_cross += cm * t.notional;
        w_total += t.notional;
    }
    if w_total > 0.0 {
        println!(
            "\nnotional-weighted: spread {:.2} bp, cost of crossing at order size {:.2} bp",
            w_spread / w_total,
            w_cross / w_total
        );
        println!(
            "cost of one full rebalance at these sizes: ${:.2} on ${:.0} traded",
            w_cross / w_total / 1e4 * w_total,
            w_total
        );
    }
    Ok(())
}

/// Order sizes from a real plan, priced at the marks the plan itself carries.
fn from_plan(path: &str) -> Result<Vec<Target>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let nav: f64 = v["nav"]["total"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or("plan has no nav.total")?;
    // price = weight * nav / qty, the same recovery the runner uses.
    let mut price: BTreeMap<String, f64> = BTreeMap::new();
    for c in v["current"].as_array().into_iter().flatten() {
        let (Some(a), Some(q), Some(w)) = (
            c["asset"].as_str(),
            c["qty"].as_str().and_then(|s| s.parse::<f64>().ok()),
            c["weight"].as_str().and_then(|s| s.parse::<f64>().ok()),
        ) else {
            continue;
        };
        if q != 0.0 {
            price.insert(a.to_string(), (w * nav / q).abs());
        }
    }
    let mut out = Vec::new();
    for o in v["orders"].as_array().into_iter().flatten() {
        let (Some(a), Some(q)) = (
            o["asset"].as_str(),
            o["qty"].as_str().and_then(|s| s.parse::<f64>().ok()),
        ) else {
            continue;
        };
        // A name being opened has no price in `current`; the book will give us
        // one, so carry the quantity and size it at the first ask.
        let notional = price.get(a).map(|p| q * p).unwrap_or(0.0);
        out.push(Target {
            asset: a.to_string(),
            notional,
        });
    }
    Ok(out)
}

fn side(book: &hyperliquid::L2Book, i: usize) -> Vec<(f64, f64)> {
    book.levels
        .get(i)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| Some((l.px.parse().ok()?, l.sz.parse().ok()?)))
                .collect()
        })
        .unwrap_or_default()
}

/// VWAP of eating `notional` quote-currency worth of one side of the book.
/// None when the book is too thin to fill it at all — which is itself a finding.
fn walk(levels: &[(f64, f64)], notional: f64) -> Option<f64> {
    if notional <= 0.0 {
        return levels.first().map(|(p, _)| *p);
    }
    let (mut left, mut spent, mut got) = (notional, 0.0, 0.0);
    for (px, sz) in levels {
        let avail = px * sz;
        let take = avail.min(left);
        spent += take;
        got += take / px;
        left -= take;
        if left <= 0.0 {
            break;
        }
    }
    (left <= 0.0 && got > 0.0).then(|| spent / got)
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.iter().sum::<f64>() / v.len() as f64
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[s.len() / 2]
}
