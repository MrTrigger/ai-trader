//! Evaluate a locked continuation without restarting its portfolio.
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use crypto_portfolio::{lighter_history, lighter_hourly, monte_carlo};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const LOCK_SHA256: &str = "edebd7cb7067afa3bc9d919344b1a80da7935026354a08b9137e1db7b92d74f6";

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read(path: &Path) -> Result<(Value, String), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let digest = hash(&bytes);
    let marker = path.with_extension("sha256");
    if marker.exists() {
        let expected = std::fs::read_to_string(marker).map_err(|e| e.to_string())?;
        if expected.split_whitespace().next() != Some(digest.as_str()) {
            return Err(format!("{}: checksum mismatch", path.display()));
        }
    }
    Ok((
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        digest,
    ))
}

fn nav(value: &Value) -> Result<Decimal, String> {
    let result: Decimal = value
        .as_str()
        .ok_or("NAV must be a decimal string")?
        .parse()
        .map_err(|e| format!("invalid NAV: {e}"))?;
    if result <= Decimal::ZERO {
        return Err("NAV must be positive".into());
    }
    Ok(result)
}

fn stamp(value: &Value) -> Result<DateTime<Utc>, String> {
    value
        .as_str()
        .ok_or("missing timestamp")?
        .parse()
        .map_err(|e| format!("invalid timestamp: {e}"))
}

fn date(value: &Value) -> Result<DateTime<Utc>, String> {
    let day: NaiveDate = value
        .as_str()
        .ok_or("missing date")?
        .parse()
        .map_err(|e| format!("invalid date: {e}"))?;
    Ok(day.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

fn prefix(reference: &[Value], continued: &[Value]) -> Result<(), String> {
    if reference.is_empty() || continued.len() <= reference.len() {
        return Err("continuation must extend a nonempty reference".into());
    }
    for (old, new) in reference.iter().zip(continued) {
        for field in [
            "as_of",
            "attribution",
            "cash",
            "fills",
            "gross_exposure",
            "nav",
            "net_exposure",
            "status",
        ] {
            if old.get(field).is_none() || old[field] != new[field] {
                return Err(format!(
                    "development prefix differs at {} field {field}",
                    old["as_of"]
                ));
            }
        }
    }
    Ok(())
}

fn segment(
    saved: &[Value],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(Value, DateTime<Utc>), String> {
    let mut previous = None;
    for step in saved {
        let at = stamp(&step["as_of"])?;
        nav(&step["nav"])?;
        if at.timestamp().rem_euclid(86400) != 0
            || at.timestamp_subsec_nanos() != 0
            || previous.is_some_and(|prior| at - prior != Duration::days(1))
        {
            return Err("saved daily path has gaps, duplicates or non-midnight timestamps".into());
        }
        previous = Some(at);
    }
    let first = saved
        .iter()
        .position(|s| stamp(&s["as_of"]).ok() == Some(start))
        .ok_or("missing confirmation start")?;
    let last = saved
        .iter()
        .position(|s| stamp(&s["as_of"]).ok() == Some(end))
        .ok_or("missing confirmation end")?;
    if first == 0 || last < first {
        return Err("confirmation requires prior saved NAV and ordered bounds".into());
    }
    let baseline = stamp(&saved[first - 1]["as_of"])? + Duration::hours(1);
    Ok((
        json!({"initial_cash":saved[first-1]["nav"], "steps":saved[first..=last]}),
        baseline,
    ))
}

fn save(path: &Path, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, &bytes).map_err(|e| e.to_string())?;
    std::fs::write(path.with_extension("sha256"), format!("{}\n", hash(&bytes)))
        .map_err(|e| e.to_string())
}

fn run(args: &[String]) -> Result<(), String> {
    if args.len() < 6 {
        return Err("usage: lighter-confirmation RAW_DAILY CONTINUED_FOLD REFERENCE_FOLD LOCK OUT HOURLY_ROOT [HOURLY_EXTENSION...]".into());
    }
    let (continued, continued_sha) = read(Path::new(&args[1]))?;
    let (reference, reference_sha) = read(Path::new(&args[2]))?;
    let (lock, lock_sha) = read(Path::new(&args[3]))?;
    if lock_sha != LOCK_SHA256 {
        return Err("confirmation lock differs from the recorded pre-outcome protocol".into());
    }
    let out = Path::new(&args[4]);
    if out.exists() {
        return Err("output exists; preserve prior evidence and use a fresh directory".into());
    }
    for fold in [&continued, &reference] {
        if fold["source_config_sha256"] != lock["config_sha256"]
            || fold["metadata"]["valuation_hour_utc"] != 1
            || fold["metadata"]["execution_venue"] != "lighter"
        {
            return Err("fold does not match locked config or native 01:00 valuation".into());
        }
    }
    let model = Path::new(lock["model_path"].as_str().ok_or("lock lacks model path")?);
    if hash(&std::fs::read(model).map_err(|e| e.to_string())?)
        != lock["model_sha256"]
            .as_str()
            .ok_or("lock lacks model hash")?
    {
        return Err("locked model checksum mismatch".into());
    }
    let saved = continued["steps"]
        .as_array()
        .ok_or("continued fold lacks steps")?;
    let prior = reference["steps"]
        .as_array()
        .ok_or("reference fold lacks steps")?;
    prefix(prior, saved)?;
    if stamp(&prior[0]["as_of"])? != date(&lock["development_period"][0])?
        || stamp(&prior.last().unwrap()["as_of"])? != date(&lock["development_period"][1])?
    {
        return Err("reference dates differ from the locked development period".into());
    }
    let start = date(&lock["confirmation_period"][0])?;
    let end = date(&lock["confirmation_period"][1])?;
    let (strategy, baseline) = segment(saved, start, end)?;
    if baseline != date(&lock["bridge_day"])? + Duration::hours(1) {
        return Err("confirmation prior NAV does not match locked bridge day".into());
    }
    let history = lighter_history::History::load(Path::new(&args[0]))?;
    history.validate_period(baseline - Duration::hours(1), end)?;
    let roots: Vec<_> = args[5..].iter().map(PathBuf::from).collect();
    let hourly = lighter_hourly::History::load_many(&roots, &history.market_ids)?;
    let price = |at| -> Result<Decimal, String> {
        hourly
            .bars
            .get(&at)
            .and_then(|bars| bars.get("BTC"))
            .map(|bar| bar.open)
            .filter(|value| *value > Decimal::ZERO)
            .ok_or_else(|| format!("missing positive BTC mark at {at}"))
    };
    let initial = nav(&strategy["initial_cash"])?;
    let qty = initial / price(baseline)?;
    let btc_steps = strategy["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            let at = stamp(&step["as_of"])?;
            Ok(json!({"as_of":at, "nav":(qty * price(at + Duration::hours(1))?).to_string()}))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let btc = json!({"initial_cash":initial.to_string(), "steps":btc_steps});
    std::fs::create_dir_all(out.join("strategy")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(out.join("btc")).map_err(|e| e.to_string())?;
    save(&out.join("strategy/fold-1.json"), &strategy)?;
    save(&out.join("btc/fold-1.json"), &btc)?;
    let observed = monte_carlo::observed_metrics(&out.join("strategy"))?;
    let benchmark = monte_carlo::observed_metrics(&out.join("btc"))?;
    let report = json!({
        "schema":"lighter-locked-confirmation-result-1",
        "lock_sha256":lock_sha,"continued_sha256":continued_sha,"reference_sha256":reference_sha,
        "development_prefix_exact_steps":prior.len(),"starting_nav":initial.to_string(),
        "confirmation_start":start,"confirmation_end":end,"valuation_start":baseline,"valuation_end":end+Duration::hours(1),
        "observations":btc_steps.len(),"strategy":observed,"btc_price":benchmark,
        "confirmation_gate_pass":observed.total_return>0.0 && observed.total_return>benchmark.total_return,
        "native_daily_sha256":history.page_sha256,"native_hourly_sha256":hourly.page_sha256,
        "executable_sha256":hash(&std::fs::read(std::env::current_exe().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?),
        "disclosures":["Positions and NAV continue through the bridge day; no cash restart.","BTC fixed quantity starts at the same prior 01:00 valuation used for the strategy's first confirmation return. Gross price benchmark excludes fees and funding.","Short confirmation Sharpe is descriptive; no block-bootstrap claim or long-run Sharpe guarantee is made from this segment."]
    });
    save(&out.join("summary.json"), &report)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn main() {
    if let Err(error) = run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("lighter-confirmation: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_preserves_prior_nav_and_rejects_missing_days() {
        let mut saved = vec![
            json!({"as_of":"2026-07-31T00:00:00Z","nav":"120000"}),
            json!({"as_of":"2026-08-01T00:00:00Z","nav":"121000"}),
            json!({"as_of":"2026-08-02T00:00:00Z","nav":"122000"}),
        ];
        let start = "2026-08-01T00:00:00Z".parse().unwrap();
        let end = start + Duration::days(1);
        let (part, baseline) = segment(&saved, start, end).unwrap();
        assert_eq!(part["initial_cash"], "120000");
        assert_eq!(part["steps"].as_array().unwrap().len(), 2);
        assert_eq!(baseline, start - Duration::hours(23));
        saved.remove(1);
        assert!(segment(&saved, start, end).is_err());
    }

    #[test]
    fn prefix_must_match_every_financial_field() {
        let step = json!({"as_of":"2026-01-01T00:00:00Z","attribution":{},"cash":"100","fills":[],"gross_exposure":"0","nav":"100","net_exposure":"0","status":"accepted"});
        let reference = vec![step.clone()];
        let mut continued = vec![step.clone(), step];
        prefix(&reference, &continued).unwrap();
        continued[0]["fills"] = json!([{"qty":"1"}]);
        assert!(prefix(&reference, &continued).is_err());
    }
}
