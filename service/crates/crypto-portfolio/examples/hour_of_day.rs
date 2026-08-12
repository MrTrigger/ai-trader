//! When is this book cheapest to trade, and by how much?
//!
//! The live order book says what the spread is *now*; it cannot say what it was
//! at 03:00 last March. Hourly bars can, imperfectly: dollar volume is a direct
//! liquidity proxy, and Amihud — |return| per dollar traded — is the standard
//! price-impact proxy and is exactly the quantity the cost model's impact term
//! is trying to estimate.
//!
//! Each asset is normalised by its own 24-hour median before averaging, so the
//! profile is "when is a name liquid relative to itself" rather than a ranking
//! of BTC against the tail.
//!
//!   cargo run -p crypto-portfolio --example hour_of_day -- \
//!     --data-root data --days 180
//!
//! What it cannot show: the spread, which is not in OHLCV, and which the live
//! `book_capture` measures directly for the present.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crypto_portfolio::store;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| {
        args.iter()
            .position(|a| a == k)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let root = PathBuf::from(get("--data-root").unwrap_or_else(|| "data".into()));
    let days: i64 = get("--days").and_then(|v| v.parse().ok()).unwrap_or(180);
    // Spec 8: a position's cap is the lower of max_single_position and
    // `participation_limit * hourly_volume / NAV`. The second term is linear in
    // the volume of the hour we actually send in, so the trading hour sets how
    // large the thin tail is allowed to be — a constraint, not a price.
    let nav: f64 = get("--nav")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000.0);
    let participation: f64 = get("--participation")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.05);
    let max_single: f64 = get("--max-position")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.25);
    let target_w: f64 = get("--target-weight")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.8 / 24.0);

    // The names actually traded, from the newest universe snapshot. An
    // hour-of-day profile over 600 delisted shells describes a market we do
    // not trade in.
    let assets = newest_eligible(&root)?;
    eprintln!(
        "{} eligible names, last {days} days of hourly bars",
        assets.len()
    );

    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    // hour -> per-asset relative values
    let mut rel_vol: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
    let mut rel_amihud: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
    let mut rel_range: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
    let mut used = 0;
    // asset -> median quote volume in each hour, kept for the capacity pass.
    let mut hourly_vol: BTreeMap<String, BTreeMap<u32, f64>> = BTreeMap::new();

    for asset in &assets {
        let bars = match store::read_asset(&root, 3600, asset) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let mut vol: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        let mut ami: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        let mut rng: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        for b in bars.iter().filter(|b| b.ts_utc >= cutoff) {
            let q = b.quote_volume.unwrap_or(b.volume * b.close);
            // NaN fails every comparison, which is the intent: an unpriced bar
            // is skipped rather than folded in as a zero.
            if q.is_nan() || q <= 0.0 || b.open <= 0.0 || b.close <= 0.0 {
                continue;
            }
            let h = chrono::Timelike::hour(&b.ts_utc);
            let ret = (b.close / b.open - 1.0).abs();
            vol.entry(h).or_default().push(q);
            // Amihud: price move per dollar traded. Scaled only to keep the
            // printed numbers readable; every use here is a ratio.
            ami.entry(h).or_default().push(ret / q * 1e9);
            rng.entry(h).or_default().push((b.high - b.low) / b.close);
        }
        if vol.len() < 24 {
            continue;
        }
        used += 1;
        // Normalise against this asset's own all-hours median.
        let med_of = |m: &BTreeMap<u32, Vec<f64>>| -> f64 {
            let mut all: Vec<f64> = m.values().flatten().copied().collect();
            median(&mut all)
        };
        let (bv, ba, br) = (med_of(&vol), med_of(&ami), med_of(&rng));
        hourly_vol.insert(
            asset.clone(),
            (0..24u32)
                .filter_map(|h| {
                    let mut v = vol.get(&h)?.clone();
                    Some((h, median(&mut v)))
                })
                .collect(),
        );
        for h in 0..24u32 {
            if let (Some(v), Some(a), Some(r)) = (vol.get(&h), ami.get(&h), rng.get(&h)) {
                let (mut v, mut a, mut r) = (v.clone(), a.clone(), r.clone());
                if bv > 0.0 {
                    rel_vol.entry(h).or_default().push(median(&mut v) / bv);
                }
                if ba > 0.0 {
                    rel_amihud.entry(h).or_default().push(median(&mut a) / ba);
                }
                if br > 0.0 {
                    rel_range.entry(h).or_default().push(median(&mut r) / br);
                }
            }
        }
    }
    eprintln!("{used} names had a full 24-hour profile\n");

    println!("hour   volume   impact    range     (1.00 = that name's own daily median)");
    let mut rows = Vec::new();
    for h in 0..24u32 {
        let v = mean(rel_vol.get(&h));
        let a = mean(rel_amihud.get(&h));
        let r = mean(rel_range.get(&h));
        rows.push((h, v, a, r));
        println!("{h:02}:00 {v:8.3} {a:8.3} {r:8.3}");
    }

    // Impact is the term the cost model is guessing at, so rank on it.
    let best = rows
        .iter()
        .min_by(|x, y| x.2.total_cmp(&y.2))
        .copied()
        .unwrap();
    let worst = rows
        .iter()
        .max_by(|x, y| x.2.total_cmp(&y.2))
        .copied()
        .unwrap();
    println!(
        "\ncheapest hour {:02}:00  impact {:.3}   dearest {:02}:00  impact {:.3}   spread {:.2}x",
        best.0,
        best.2,
        worst.0,
        worst.2,
        worst.2 / best.2
    );
    // The hours the executor actually sends in.
    for h in [0u32, 1, 2] {
        let row = rows[h as usize];
        println!(
            "we trade {:02}:00  impact {:.3}  = {:.2}x the cheapest hour, volume {:.2}x",
            h,
            row.2,
            row.2 / best.2,
            row.1 / best.1
        );
    }
    // --- capacity: how large may the thin tail be, at each hour ---------------
    let want = target_w * nav;
    let ceiling = max_single * nav;
    println!(
        "\ncapacity at NAV ${nav:.0}: target position ${want:.0}, \
         hard cap ${ceiling:.0}, participation {:.1}% of the hour",
        participation * 100.0
    );
    println!("hour   names capped   median cap $   book the caps allow $");
    let mut cap_rows = Vec::new();
    for h in [1u32, 2, 14] {
        let mut caps: Vec<f64> = Vec::new();
        for v in hourly_vol.values() {
            let Some(vol_h) = v.get(&h) else { continue };
            caps.push((participation * vol_h).min(ceiling));
        }
        if caps.is_empty() {
            continue;
        }
        let capped = caps.iter().filter(|c| **c < want).count();
        let allowed: f64 = caps.iter().map(|c| c.min(want)).sum();
        let mut sorted = caps.clone();
        println!(
            "{h:02}:00 {capped:>13} {:>14.0} {allowed:>23.0}",
            median(&mut sorted)
        );
        cap_rows.push((h, capped, allowed));
    }
    if let (Some(a), Some(b)) = (
        cap_rows.iter().find(|r| r.0 == 1),
        cap_rows.iter().find(|r| r.0 == 14),
    ) {
        println!(
            "\n14:00 vs 01:00: {} fewer names capped, {:.2}x the book the caps allow",
            a.1 as i64 - b.1 as i64,
            b.2 / a.2
        );
    }
    Ok(())
}

fn newest_eligible(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("universe");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let last = files.last().ok_or("no universe snapshots")?;
    let text = std::fs::read_to_string(last).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(v["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["eligible"].as_bool().unwrap_or(false))
        .filter_map(|m| m["asset"].as_str().map(str::to_string))
        .collect())
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

fn mean(v: Option<&Vec<f64>>) -> f64 {
    match v {
        Some(v) if !v.is_empty() => v.iter().sum::<f64>() / v.len() as f64,
        _ => f64::NAN,
    }
}
