//! Venue-aware ranker matrix with external or native forward-return labels.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{Duration, NaiveDate};
use crypto_portfolio::{config::Config, lighter_history::History, training, universe};

fn date(value: &str) -> Result<NaiveDate, String> {
    value
        .parse()
        .map_err(|e| format!("invalid date {value:?}: {e}"))
}

fn holding_hours(value: Option<&str>) -> Result<i64, String> {
    let hours = value
        .unwrap_or("24")
        .parse::<i64>()
        .map_err(|_| "HOLD_HOURS must be an integer multiple of 24 within 24..=168")?;
    if !(24..=168).contains(&hours) || hours % 24 != 0 {
        return Err("HOLD_HOURS must be an integer multiple of 24 within 24..=168".into());
    }
    Ok(hours)
}

#[derive(Debug, PartialEq)]
struct Options {
    hold_hours: i64,
    market_context: bool,
    hourly_root: Option<PathBuf>,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut args = args.iter().peekable();
    let hold_hours = if args.peek().is_some_and(|value| !value.starts_with("--")) {
        holding_hours(args.next().map(String::as_str))?
    } else {
        24
    };
    let mut result = Options {
        hold_hours,
        market_context: false,
        hourly_root: None,
    };
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--market-context" if !result.market_context => result.market_context = true,
            "--hourly-root" if result.hourly_root.is_none() => {
                let root = args
                    .next()
                    .filter(|value| !value.starts_with("--"))
                    .ok_or("--hourly-root requires a path")?;
                result.hourly_root = Some(PathBuf::from(root));
            }
            _ => return Err(format!("unexpected or duplicate training option {flag:?}")),
        }
    }
    Ok(result)
}

fn quarantine_offsets(hold_hours: i64) -> std::ops::RangeInclusive<i64> {
    -(hold_hours / 24)..=30
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 7 || !matches!(args[0].as_str(), "external" | "native" | "native-hourly") {
        return Err("usage: lighter-training-matrix external|native|native-hourly RAW_DAILY SIGNAL_ROOT CONFIG START END NEW_OUT_DIR [HOLD_HOURS] [--market-context] [--hourly-root ROOT]".into());
    }
    let kind = &args[0];
    let Options {
        hold_hours,
        market_context,
        hourly_root,
    } = options(&args[7..])?;
    if kind == "native" && hold_hours != 24 {
        return Err("native daily labels require HOLD_HOURS=24".into());
    }
    let history = History::load(Path::new(&args[1]))?;
    let hourly_history = match (kind.as_str(), hourly_root.as_ref()) {
        ("native-hourly", Some(root)) => Some(crypto_portfolio::lighter_hourly::LabelMarks::load(
            root,
            &history.market_ids,
        )?),
        ("native-hourly", None) => {
            return Err("native-hourly labels require --hourly-root ROOT".into())
        }
        (_, Some(_)) => return Err("--hourly-root is only valid for native-hourly labels".into()),
        (_, None) => None,
    };
    let signal_root = Path::new(&args[2]);
    let cfg = Config::load(Path::new(&args[3]))?;
    let start = date(&args[4])?;
    let end = date(&args[5])?;
    let out = PathBuf::from(&args[6]);
    if out.exists() {
        return Err(format!("output already exists: {}", out.display()));
    }
    std::fs::create_dir_all(out.join("universe")).map_err(|e| e.to_string())?;

    // Omit whole cross-sections whose forward label or 30-day native
    // liquidity window can include a quarantined candle.
    let mut omitted = BTreeSet::new();
    for q in &history.quarantined {
        for offset in quarantine_offsets(hold_hours) {
            omitted.insert((q.date + Duration::days(offset)).date_naive());
        }
    }
    let signal_bars = crypto_portfolio::store::read(signal_root, 86_400)?;
    let mut cohort_sizes = Vec::new();
    let mut candidate_days = Vec::new();
    let mut omitted_small_cohorts = Vec::new();
    let mut day = start;
    while day <= end {
        if !omitted.contains(&day) {
            let stamp = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
            let members = history.members_at(&signal_bars, &cfg, stamp)?;
            let eligible = members.iter().filter(|m| m.eligible).count();
            if eligible < 15 {
                omitted.insert(day);
                omitted_small_cohorts.push(serde_json::json!({"date":day,"eligible":eligible}));
                day += Duration::days(1);
                continue;
            }
            universe::write(&out, stamp, "lighter-training-cohort-v1", members, false)?;
            cohort_sizes.push(serde_json::json!({"date":day,"eligible":eligible}));
            candidate_days.push(day);
        }
        day += Duration::days(1);
    }
    let (lag, labels) = match hourly_history.as_ref() {
        Some(hourly) => (1, training::LabelPrices::NativeHourly(hourly)),
        None if kind == "external" => (1, training::LabelPrices::ExternalHourly),
        None => (0, training::LabelPrices::NativeDaily(&history.daily)),
    };
    let matrix = training::build_for_universe(
        signal_root,
        &out,
        &cfg,
        start,
        end,
        lag,
        hold_hours,
        features_crypto::FundingWindow::Trailing,
        true,
        24,
        labels,
        &omitted,
        15,
        true,
        market_context,
    )?;
    let matrix_path = out.join("matrix.jsonl");
    let represented: BTreeSet<_> = matrix.rows.iter().map(|row| row.date).collect();
    let omitted_after_matrix_requirements: Vec<_> = candidate_days
        .into_iter()
        .filter(|day| !represented.contains(day))
        .collect();
    let mut writer =
        std::io::BufWriter::new(std::fs::File::create(&matrix_path).map_err(|e| e.to_string())?);
    let mut manifest = serde_json::json!({
        "kind":"manifest", "feature_set_version":features_crypto::FEATURE_SET_VERSION,
        "features":matrix.features, "target_contract":training::TARGET_CONTRACT,
        "label_horizon_hours":hold_hours, "label_lag_hours":lag,
        "training_cohort":"causal-lighter-activity-then-external-liquidity-top30",
        "label_prices":kind, "quarantine_policy":format!("omit whole decisions q-{} through q+30", hold_hours / 24)
    });
    if let Some(contract) = matrix.market_context {
        manifest["market_context"] = contract.into();
    }
    serde_json::to_writer(&mut writer, &manifest).map_err(|e| e.to_string())?;
    writer.write_all(b"\n").map_err(|e| e.to_string())?;
    for row in &matrix.rows {
        serde_json::to_writer(&mut writer, row).map_err(|e| e.to_string())?;
        writer.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())?;
    if let Some(hourly) = hourly_history.as_ref() {
        std::fs::write(out.join("native-hourly-inputs.json"), serde_json::to_vec_pretty(
            &serde_json::json!({"validation_scope":"source provenance and all timestamps; OHLC at 01:00 UTC only; not execution history","root":hourly_root,"page_sha256":hourly.page_sha256,"label_prices":"native-hourly","label_lag_hours":lag,"label_horizon_hours":hold_hours,"missing_label_policy":"fail; never fall back to external prices"})
        ).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    }
    std::fs::write(out.join("cohort.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "schema":"lighter-ranker-cohort-1", "start":start, "end":end,
        "label_prices":kind, "omitted_decisions":omitted.iter().filter(|d| **d>=start && **d<=end).collect::<Vec<_>>(),
        "cohort_sizes":cohort_sizes, "rows":matrix.rows.len(),
        "omitted_small_cohorts":omitted_small_cohorts,
        "omitted_after_matrix_requirements":omitted_after_matrix_requirements,
        "native_input_sha256":history.page_sha256,
        "native_market_count":history.market_ids.len(),
        "quarantined_native_candles":history.quarantined,
    })).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    println!(
        "wrote {} rows to {}",
        matrix.rows.len(),
        matrix_path.display()
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("lighter-training-matrix: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_context_flag_follows_optional_holding_period() {
        let parse =
            |args: &[&str]| options(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        let expected = |hold_hours, market_context| Options {
            hold_hours,
            market_context,
            hourly_root: None,
        };
        assert_eq!(parse(&[]).unwrap(), expected(24, false));
        assert_eq!(parse(&["--market-context"]).unwrap(), expected(24, true));
        assert_eq!(
            parse(&["24", "--market-context"]).unwrap(),
            expected(24, true)
        );
        assert_eq!(
            parse(&["120", "--market-context"]).unwrap(),
            expected(120, true)
        );
        assert!(parse(&["--market-context", "24"]).is_err());
        assert!(parse(&["24", "--unknown"]).is_err());
        assert_eq!(
            parse(&["24", "--market-context", "--hourly-root", "raw"]).unwrap(),
            Options {
                hold_hours: 24,
                market_context: true,
                hourly_root: Some(PathBuf::from("raw")),
            }
        );
        for invalid in [
            vec!["--hourly-root"],
            vec!["--hourly-root", "--market-context"],
            vec!["--market-context", "--market-context"],
            vec!["--hourly-root", "one", "--hourly-root", "two"],
        ] {
            assert!(parse(&invalid).is_err());
        }
    }

    #[test]
    fn default_and_supported_holding_periods() {
        assert_eq!(holding_hours(None).unwrap(), 24);
        for hours in [24, 48, 72, 96, 120, 144, 168] {
            assert_eq!(holding_hours(Some(&hours.to_string())).unwrap(), hours);
        }
    }

    #[test]
    fn invalid_holding_periods_fail() {
        for value in ["0", "-24", "25", "119", "169", "192", "24.0", "abc"] {
            assert!(holding_hours(Some(value)).is_err(), "{value}");
        }
    }

    #[test]
    fn quarantine_covers_forward_labels_and_liquidity_history() {
        assert_eq!(quarantine_offsets(24), -1..=30);
        assert_eq!(quarantine_offsets(120), -5..=30);
        assert_eq!(quarantine_offsets(168), -7..=30);
    }
}
