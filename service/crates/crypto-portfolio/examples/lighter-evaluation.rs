//! Evaluate a saved native replay and gross native buy-and-hold price paths.
//! cargo run -p crypto-portfolio --example lighter-evaluation -- RAW_DAILY STRATEGY_FOLD_JSON OUT [HOURLY_ROOT HOURLY_EXTENSION...]
use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Duration, Timelike, Utc};
use crypto_portfolio::{lighter_history::History, monte_carlo};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn positive(value: &Value) -> Result<Decimal, String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err("NAV must be a decimal string or number".into()),
    };
    let value: Decimal = text.parse().map_err(|e| format!("invalid NAV: {e}"))?;
    if value <= Decimal::ZERO {
        return Err("NAV must be positive".into());
    }
    Ok(value)
}

fn dates(steps: &[Value]) -> Result<Vec<DateTime<Utc>>, String> {
    let mut result = Vec::with_capacity(steps.len());
    for step in steps {
        positive(&step["nav"])?;
        let at: DateTime<Utc> = step["as_of"]
            .as_str()
            .ok_or("step lacks as_of")?
            .parse()
            .map_err(|e| format!("invalid as_of: {e}"))?;
        if at.time().num_seconds_from_midnight() != 0 || at.timestamp_subsec_nanos() != 0 {
            return Err(format!("{at}: expected UTC midnight"));
        }
        if result
            .last()
            .is_some_and(|previous| at - *previous != Duration::days(1))
        {
            return Err(format!("{at}: duplicate, missing or unordered daily step"));
        }
        result.push(at);
    }
    Ok(result)
}

fn segment(initial: &Value, steps: &[Value], start: usize, end: usize) -> Value {
    json!({
        "schema_version":"derived-saved-nav-evaluation-1",
        "initial_cash":if start==0 {initial.clone()} else {steps[start-1]["nav"].clone()},
        "steps":steps[start..end],
        "derivation":{
            "initial_cash_semantics":if start==0 {"original starting NAV"} else {"prior saved NAV, not fresh cash or a portfolio restart"},
            "start_index_in_original":start,
            "end_index_exclusive_in_original":end,
            "preserves_existing_positions_and_first_segment_day_return":true
        }
    })
}

fn benchmark(
    initial: Decimal,
    dates: &[DateTime<Utc>],
    btc: &[Decimal],
    eth: &[Decimal],
    btc_weight: Decimal,
) -> Vec<Value> {
    let btc_qty = initial * btc_weight / btc[0];
    let eth_qty = initial * (Decimal::ONE - btc_weight) / eth[0];
    dates
        .iter()
        .enumerate()
        .map(|(i, date)| {
            // Preserve exact entry NAV rather than manufacturing decimal division dust.
            let nav = if i == 0 {
                initial
            } else {
                btc_qty * btc[i] + eth_qty * eth[i]
            };
            json!({"as_of":date,"nav":nav.to_string()})
        })
        .collect()
}

fn write(path: &Path, value: &Value) -> Result<String, String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha = digest(&bytes);
    std::fs::write(format!("{}.sha256", path.display()), format!("{sha}\n"))
        .map_err(|e| e.to_string())?;
    Ok(sha)
}

fn valuation_hour(metadata: &Value, has_hourly_root: bool) -> Result<i64, String> {
    if metadata["execution_venue"] != "lighter" {
        return Err("expected a saved native Lighter replay".into());
    }
    match metadata["valuation"].as_str() {
        Some("native_daily_open") if !has_hourly_root => Ok(0),
        Some("native_01h_open") if has_hourly_root => Ok(1),
        Some("native_01h_open") => {
            Err("native_01h_open valuation requires explicit HOURLY_ROOT".into())
        }
        Some("native_hourly_open") if has_hourly_root => metadata["valuation_hour_utc"]
            .as_i64()
            .filter(|hour| (1..=4).contains(hour))
            .ok_or("native hourly valuation needs an explicit hour in1..=4".into()),
        Some("native_hourly_open") => {
            Err("native hourly valuation requires explicit HOURLY_ROOT".into())
        }
        Some("native_daily_open") => Err("HOURLY_ROOT supplied for a daily-open strategy".into()),
        _ => Err("unsupported native valuation clock".into()),
    }
}

fn acceptance(summaries: &[Value]) -> Result<Value, String> {
    let mut comparisons = Vec::new();
    for strategy in summaries.iter().filter(|row| row["series"] == "strategy") {
        let matches: Vec<_> = summaries
            .iter()
            .filter(|row| row["series"] == "btc-price" && row["part"] == strategy["part"])
            .collect();
        if matches.len() != 1 {
            return Err("expected exactly one BTC benchmark per strategy segment".into());
        }
        let btc = matches[0];
        for key in [
            "start",
            "end",
            "valuation_start",
            "valuation_end",
            "observations",
        ] {
            if strategy[key].is_null() || strategy[key] != btc[key] {
                return Err(format!("strategy and BTC differ in {key}"));
            }
        }
        let number = |row: &Value, key: &str| -> Result<f64, String> {
            row[key]
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| format!("missing or nonfinite {key}"))
        };
        let strategy_return = number(strategy, "total_return")?;
        let btc_return = number(btc, "total_return")?;
        let sharpe_pass = strategy["sharpe"]
            .as_f64()
            .is_some_and(|value| value.is_finite() && value > 1.5);
        let beats_btc = strategy_return > btc_return;
        comparisons.push(json!({
            "part":strategy["part"], "start":strategy["valuation_start"], "end":strategy["valuation_end"],
            "strategy_sharpe":strategy["sharpe"], "strategy_return":strategy_return,
            "btc_return":btc_return, "return_gap_percentage_points":100.0*(strategy_return-btc_return),
            "btc_rising":btc_return>0.0, "sharpe_above_1_5":sharpe_pass,
            "beats_btc":beats_btc, "joint_pass":sharpe_pass && beats_btc
        }));
    }
    let passed = |part: &str| -> Result<bool, String> {
        let rows: Vec<_> = comparisons
            .iter()
            .filter(|row| row["part"] == part)
            .collect();
        if rows.len() != 1 {
            return Err(format!("expected exactly one {part} comparison"));
        }
        Ok(rows[0]["joint_pass"] == true)
    };
    let full = passed("full")?;
    let first = passed("first-half")?;
    let second = passed("second-half")?;
    Ok(json!({
        "criteria":"strategy Sharpe strictly > 1.5 AND net strategy return strictly > gross BTC price return on matched timestamps",
        "full_period_pass":full, "full_period_and_both_halves_pass":full && first && second,
        "comparisons":comparisons,
        "scope":"Development evidence. Monthly rows are diagnostics, not independent tests or additional mandatory gates. No numerical threshold for 'much better' has been specified."
    }))
}

fn run(args: &[String]) -> Result<(), String> {
    if args.len() < 3 {
        return Err(
            "usage: lighter-evaluation RAW_DAILY STRATEGY_FOLD_JSON OUT [HOURLY_ROOT HOURLY_EXTENSION...]".into(),
        );
    }
    let raw = Path::new(&args[0]);
    let source = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    if out.exists() {
        return Err("output exists; use a fresh output directory to preserve evidence".into());
    }
    let bytes = std::fs::read(source).map_err(|e| e.to_string())?;
    let source_sha = digest(&bytes);
    let marker = source.with_extension("sha256");
    if marker.exists() {
        let expected = std::fs::read_to_string(&marker).map_err(|e| e.to_string())?;
        if expected.split_whitespace().next() != Some(source_sha.as_str()) {
            return Err("saved strategy SHA256 mismatch".into());
        }
    }
    let fold: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let valuation_hour = valuation_hour(&fold["metadata"], args.len() >= 4)?;
    let initial = positive(&fold["initial_cash"])?;
    let saved = fold["steps"].as_array().ok_or("missing saved steps")?;
    if saved.len() < 120 {
        return Err(format!(
            "need at least120 daily observations for two60-day evaluation segments, got {}",
            saved.len()
        ));
    }
    let dates = dates(saved)?;
    let history = History::load(raw)?;
    history.validate_period(dates[0], *dates.last().unwrap())?;
    let hourly_roots: Vec<_> = args[3..].iter().map(std::path::PathBuf::from).collect();
    let hourly = if hourly_roots.is_empty() {
        None
    } else {
        Some(crypto_portfolio::lighter_hourly::History::load_many(
            &hourly_roots,
            &history.market_ids,
        )?)
    };
    let opens: BTreeMap<_, _> = history
        .daily
        .iter()
        .filter(|bar| bar.asset == "BTC" || bar.asset == "ETH")
        .map(|bar| ((bar.asset.clone(), bar.ts_utc), bar.open))
        .collect();
    let prices = |asset: &str| -> Result<Vec<Decimal>, String> {
        dates
            .iter()
            .map(|date| {
                if let Some(hourly) = &hourly {
                    let stamp = *date + Duration::hours(valuation_hour);
                    return hourly
                        .bars
                        .get(&stamp)
                        .and_then(|bars| bars.get(asset))
                        .map(|bar| bar.open)
                        .ok_or_else(|| format!("missing native {asset} valuation at {stamp}"));
                }
                let price = opens
                    .get(&(asset.into(), *date))
                    .ok_or_else(|| format!("missing native {asset} open at {date}"))?;
                let result = Decimal::from_f64_retain(*price).ok_or("invalid native price")?;
                if result <= Decimal::ZERO {
                    return Err("nonpositive native price".into());
                }
                Ok(result)
            })
            .collect()
    };
    let btc = prices("BTC")?;
    let eth = prices("ETH")?;
    let series = vec![
        ("strategy", saved.clone()),
        (
            "btc-price",
            benchmark(initial, &dates, &btc, &eth, Decimal::ONE),
        ),
        (
            "eth-price",
            benchmark(initial, &dates, &btc, &eth, Decimal::ZERO),
        ),
        (
            "btc-eth-initial-50-50-price",
            benchmark(initial, &dates, &btc, &eth, Decimal::new(5, 1)),
        ),
    ];
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let split = saved.len() / 2;
    let mut segments = vec![
        ("full".to_string(), 0, saved.len()),
        ("first-half".into(), 0, split),
        ("second-half".into(), split, saved.len()),
    ];
    let mut month_start = 0;
    for end in 1..=dates.len() {
        if end == dates.len()
            || dates[end].format("%Y-%m").to_string()
                != dates[month_start].format("%Y-%m").to_string()
        {
            segments.push((
                format!("month-{}", dates[month_start].format("%Y-%m")),
                month_start,
                end,
            ));
            month_start = end;
        }
    }
    let mut summaries = Vec::new();
    let mut artifacts = BTreeMap::new();
    for (name, steps) in series {
        for (part, start, end) in &segments {
            let (start, end) = (*start, *end);
            let dir = out.join(name).join(part);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut data = segment(&fold["initial_cash"], &steps, start, end);
            data["derivation"]["strategy_source_sha256"] = json!(source_sha);
            data["derivation"]["series"] = json!(name);
            data["derivation"]["native_price_benchmark"] = json!(name != "strategy");
            data["derivation"]["valuation"] = fold["metadata"]["valuation"].clone();
            data["derivation"]["valuation_hour_utc"] = json!(valuation_hour);
            if name != "strategy" {
                data["derivation"]["benchmark_disclosure"]=json!("Gross price-only buy-and-hold entered at the first original native open. No funding, entry/exit fees, spread, impact, margin or liquidation. Fixed quantities; 50/50 describes initial capital only. Not an executable perpetual strategy.");
            }
            let file = dir.join("fold-1.json");
            artifacts.insert(
                file.strip_prefix(out).unwrap().display().to_string(),
                write(&file, &data)?,
            );
            if part.starts_with("month-") {
                let metrics = monte_carlo::observed_metrics(&dir)?;
                summaries.push(json!({"series":name,"part":part,"start":dates[start],"end":dates[end-1],
                    "valuation_start":dates[start]+Duration::hours(valuation_hour),"valuation_end":dates[end-1]+Duration::hours(valuation_hour),
                    "observations":end-start,"starting_nav":data["initial_cash"],
                    "sharpe":metrics.sharpe,"total_return":metrics.total_return,"max_drawdown":metrics.max_drawdown,
                    "disclosure":"Descriptive short segment only; no bootstrap or independent validation claim."}));
                continue;
            }
            let mc = dir.join("monte-carlo.json");
            monte_carlo::command(&[
                "--folds".into(),
                dir.display().to_string(),
                "--out".into(),
                mc.display().to_string(),
                "--block".into(),
                "20".into(),
                "--seed".into(),
                "42".into(),
                "--iterations".into(),
                "10000".into(),
                "--horizon-days".into(),
                (end - start).to_string(),
            ])?;
            let mc_bytes = std::fs::read(&mc).map_err(|e| e.to_string())?;
            artifacts.insert(
                mc.strip_prefix(out).unwrap().display().to_string(),
                digest(&mc_bytes),
            );
            let report: Value = serde_json::from_slice(&mc_bytes).map_err(|e| e.to_string())?;
            summaries.push(json!({"series":name,"part":part,"start":dates[start],"end":dates[end-1],"valuation_start":dates[start]+Duration::hours(valuation_hour),"valuation_end":dates[end-1]+Duration::hours(valuation_hour),"observations":end-start,
                "starting_nav":data["initial_cash"],"sharpe":report["original_pooled_sharpe"],"total_return":report["original_cumulative_return"],
                "max_drawdown":report["original_max_drawdown"],"sharpe_bootstrap":report["whole_sample_sharpe_bootstrap"],"report":mc.strip_prefix(out).unwrap().display().to_string()}));
        }
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    write(
        &out.join("summary.json"),
        &json!({
            "schema":"lighter-saved-path-and-price-benchmark-evaluation-2",
            "strategy_source":source,"strategy_source_sha256":source_sha,"native_daily_root":raw,
            "valuation":fold["metadata"]["valuation"],"valuation_hour_utc":valuation_hour,"native_hourly_root":args.get(3),"native_hourly_extensions":args.get(4..).unwrap_or_default(),
            "hourly_source_sha256":hourly.as_ref().map(|h|&h.page_sha256),
            "native_source_sha256":history.page_sha256,"executable_sha256":digest(&std::fs::read(executable).map_err(|e|e.to_string())?),
            "acceptance":acceptance(&summaries)?,
            "artifact_sha256":artifacts,"series":summaries,
            "disclosures":["These are slices of one saved strategy path, not fresh independent replays. Second halves begin with the immediately preceding NAV and preserve the first overnight return.","BTC, ETH and initial 50/50 are gross price benchmarks, not executable perpetual strategies. Fixed quantities drift in weight; no periodic rebalancing.","Benchmark marks use the strategy's declared valuation clock. Benchmarks enter at the first valuation, so their first return is zero. Strategy first-date costs remain in its saved NAV.","NAV observation labels retain original00:00 decision dates. For native_01h_open the actual valuation is that date at01:00 UTC, including benchmark marks.","Block20 seed42 bootstrap uses 10000 draws and365 annual periods. It conditions on observed paths, does not correct selection bias and adds no independent observations."]
        }),
    )?;
    Ok(())
}

fn main() {
    if let Err(error) = run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("lighter-evaluation: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn comparison_fixture(sharpe: Value, strategy_return: f64, btc_return: f64) -> Vec<Value> {
        ["full", "first-half", "second-half"].into_iter().flat_map(|part| {
            [("strategy", strategy_return), ("btc-price", btc_return)].map(|(series, total_return)| json!({
                "series":series,"part":part,"start":"2026-01-01","end":"2026-01-31",
                "valuation_start":"2026-01-01T01:00:00Z","valuation_end":"2026-01-31T01:00:00Z",
                "observations":31,"sharpe":sharpe,"total_return":total_return
            }))
        }).collect()
    }
    #[test]
    fn joint_gate_requires_strict_sharpe_and_return_improvement() {
        for (sharpe, strategy, btc, expected) in [
            (json!(2.0), 0.10, -0.30, true),
            (json!(1.5), 0.10, -0.30, false),
            (json!(2.0), 0.10, 0.10, false),
            (json!(2.0), 0.10, 0.20, false),
            (Value::Null, 0.10, -0.30, false),
        ] {
            let result = acceptance(&comparison_fixture(sharpe, strategy, btc)).unwrap();
            assert_eq!(result["full_period_pass"], expected);
        }
        let mut rows = comparison_fixture(json!(2.0), 0.10, -0.30);
        rows[4]["sharpe"] = json!(0.02);
        let result = acceptance(&rows).unwrap();
        assert_eq!(result["full_period_pass"], true);
        assert_eq!(result["full_period_and_both_halves_pass"], false);
    }
    #[test]
    fn joint_gate_rejects_missing_or_mismatched_benchmarks() {
        let mut rows = comparison_fixture(json!(2.0), 0.10, -0.30);
        rows[1]["valuation_end"] = json!("2026-02-01T01:00:00Z");
        assert!(acceptance(&rows).is_err());
        rows.remove(1);
        assert!(acceptance(&rows).is_err());
    }
    #[test]
    fn hourly_valuation_requires_explicit_hourly_data() {
        let hourly = json!({"execution_venue":"lighter","valuation":"native_01h_open"});
        let daily = json!({"execution_venue":"lighter","valuation":"native_daily_open"});
        assert!(valuation_hour(&hourly, false).is_err());
        assert_eq!(valuation_hour(&hourly, true).unwrap(), 1);
        assert_eq!(valuation_hour(&daily, false).unwrap(), 0);
        assert!(valuation_hour(&daily, true).is_err());
        let mut four = json!({"execution_venue":"lighter","valuation":"native_hourly_open","valuation_hour_utc":4});
        assert_eq!(valuation_hour(&four, true).unwrap(), 4);
        assert!(valuation_hour(&four, false).is_err());
        four["valuation_hour_utc"] = json!(0);
        assert!(valuation_hour(&four, true).is_err());
    }
    #[test]
    fn second_half_keeps_the_first_overnight_return() {
        let steps = vec![
            json!({"nav":"110"}),
            json!({"nav":"121"}),
            json!({"nav":"108.9"}),
            json!({"nav":"119.79"}),
        ];
        let second = segment(&json!("100"), &steps, 2, 4);
        assert_eq!(second["initial_cash"], "121");
        assert_eq!(
            positive(&second["steps"][0]["nav"]).unwrap()
                / positive(&second["initial_cash"]).unwrap()
                - Decimal::ONE,
            Decimal::new(-1, 1)
        );
    }
    #[test]
    fn initial_equal_weight_benchmark_keeps_fixed_quantities() {
        let dates = vec![
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "2026-01-02T00:00:00Z".parse().unwrap(),
            "2026-01-03T00:00:00Z".parse().unwrap(),
        ];
        let btc = vec![Decimal::from(10), Decimal::from(20), Decimal::from(10)];
        let eth = vec![Decimal::from(10); 3];
        let path = benchmark(Decimal::from(100), &dates, &btc, &eth, Decimal::new(5, 1));
        assert_eq!(positive(&path[0]["nav"]).unwrap(), Decimal::from(100));
        assert_eq!(positive(&path[1]["nav"]).unwrap(), Decimal::from(150));
        assert_eq!(positive(&path[2]["nav"]).unwrap(), Decimal::from(100));
    }
    #[test]
    fn missing_or_duplicate_days_fail() {
        let row = |at: &str| json!({"as_of":at,"nav":"100"});
        assert!(dates(&[row("2026-01-01T00:00:00Z"), row("2026-01-03T00:00:00Z")]).is_err());
        assert!(dates(&[row("2026-01-01T00:00:00Z"), row("2026-01-01T00:00:00Z")]).is_err());
        assert!(dates(&[row("2026-01-01T01:00:00Z")]).is_err());
    }
}
