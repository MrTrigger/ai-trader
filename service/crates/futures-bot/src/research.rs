//! Read-only daily NAV export from the same feature streams and book as replay.
//! This module never reads DATABASE_URL or calls the records API.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use chrono::{Datelike, NaiveDate, NaiveTime, Weekday};
use features_cme::{in_frame, segment, session_date, to_exchange_time, Frame, FrameStream, RawBar};
use noise_book::{book_sleeves, runtime::Book};
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Serialize)]
struct Step {
    as_of: String,
    session_date: NaiveDate,
    nav: String,
    realized_pnl: String,
    fills: usize,
}

fn daily_steps(
    start: NaiveDate,
    end: NaiveDate,
    initial_cash: Decimal,
    daily: &BTreeMap<NaiveDate, (Decimal, usize)>,
) -> Result<Vec<Step>, String> {
    let mut nav = initial_cash;
    let mut steps = Vec::new();
    let mut day = start;
    while day <= end {
        let (net, fills) = daily.get(&day).copied().unwrap_or_default();
        nav += net;
        if nav <= Decimal::ZERO {
            return Err(format!(
                "NAV is nonpositive on {day}; capital cannot fund this realized path"
            ));
        }
        let next = day.succ_opt().ok_or("date overflow")?;
        steps.push(Step {
            as_of: format!("{next}T00:00:00Z"),
            session_date: day,
            nav: nav.to_string(),
            realized_pnl: net.to_string(),
            fills,
        });
        day = next;
    }
    Ok(steps)
}

fn validate_bars(bars: &[RawBar]) -> Result<(), String> {
    for (i, bar) in bars.iter().enumerate() {
        if i > 0 && bars[i - 1].ts_utc >= bar.ts_utc {
            return Err(format!("duplicate or out-of-order bar at {}", bar.ts_utc));
        }
        if [bar.open, bar.high, bar.low, bar.close]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
            || !bar.volume.is_finite()
            || bar.volume < 0.0
            || bar.low > bar.open.min(bar.close)
            || bar.high < bar.open.max(bar.close)
            || bar.low > bar.high
        {
            return Err(format!("invalid OHLCV at {}", bar.ts_utc));
        }
    }
    Ok(())
}

fn validate_end(book: &Book, end: NaiveDate, last_clock: Option<NaiveTime>) -> Result<(), String> {
    if book.sleeves.iter().any(|s| s.position.is_some()) {
        return Err(
            "research export ends with open positions; realized P&L alone cannot mark this book"
                .into(),
        );
    }
    let flat_by = book.sleeves.iter().map(|s| s.cfg.flat_by).max().unwrap();
    if last_clock.is_none_or(|clock| clock < flat_by) {
        return Err(format!("last requested session {end} lacks a bar at or after {flat_by}; choose a complete end session"));
    }
    Ok(())
}

pub fn command(args: &[String]) -> Result<(), String> {
    let allowed = [
        "--bars",
        "--start",
        "--end",
        "--research-out",
        "--initial-cash",
        "--fixture",
        "--roll-manifest",
    ];
    let mut options = BTreeMap::new();
    for pair in args.chunks(2) {
        if pair.len() != 2 || !allowed.contains(&pair[0].as_str()) || pair[1].starts_with("--") {
            return Err(format!(
                "unknown research option or missing value: {}",
                pair[0]
            ));
        }
        if options.insert(pair[0].as_str(), pair[1].as_str()).is_some() {
            return Err(format!("duplicate option {}", pair[0]));
        }
    }
    let required = |name: &str| {
        options
            .get(name)
            .copied()
            .ok_or_else(|| format!("{name} is required"))
    };
    let path = required("--bars")?;
    let out = required("--research-out")?;
    let start = super::parse_date(required("--start")?)?;
    let end = super::parse_date(required("--end")?)?;
    if end < start {
        return Err("--end precedes --start".into());
    }
    let initial_cash = Decimal::from_str(required("--initial-cash")?)
        .map_err(|e| format!("--initial-cash: {e}"))?;
    if initial_cash <= Decimal::ZERO {
        return Err("--initial-cash must be positive".into());
    }
    let roll_manifest = options
        .get("--roll-manifest")
        .map(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{path}: {e}"))?)
                    .map_err(|e| format!("{path}: {e}"))?;
            let dates = value["stitch"]["roll_days_excluded"]
                .as_array()
                .ok_or("roll manifest lacks stitch.roll_days_excluded")?;
            dates
                .iter()
                .map(|date| super::parse_date(date.as_str().ok_or("roll date must be a string")?))
                .collect::<Result<BTreeSet<_>, String>>()
        })
        .transpose()?
        .unwrap_or_default();
    let bars = super::read_bars(path)?;
    validate_bars(&bars)?;
    let frames: Vec<_> = book_sleeves().iter().map(|s| s.frame).collect();
    let mut book = Book::new(book_sleeves());
    // Default replay economics are $20 per point even though Book labels MNQ.
    book.instrument = "NQ".into();
    let mut globex = FrameStream::new(Frame::Globex);
    let mut rth = FrameStream::new(Frame::Rth);
    let mut observed = BTreeMap::<NaiveDate, NaiveTime>::new();
    let mut first_halt = None;
    for raw in &bars {
        let exchange = to_exchange_time(raw.ts_utc);
        let session = session_date(exchange);
        let g_bar = globex.on_bar(raw);
        let r_bar = in_frame(segment(exchange), Frame::Rth).then(|| rth.on_bar(raw));
        if session < start || session > end {
            continue;
        }
        if exchange.date_naive() == session {
            observed
                .entry(session)
                .and_modify(|clock| *clock = (*clock).max(exchange.time()))
                .or_insert(exchange.time());
        }
        for (idx, frame) in frames.iter().enumerate() {
            match frame {
                Frame::Globex => book.on_bar(idx, &g_bar),
                Frame::Rth => {
                    if let Some(bar) = &r_bar {
                        book.on_bar(idx, bar);
                    }
                }
            }
        }
        if first_halt.is_none() && book.halted.is_some() {
            first_halt = Some(raw.ts_utc);
        }
    }
    if let Some(fixture) = options.get("--fixture") {
        let fixture_value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if fixture_value["window_start"].as_str() != Some(&start.to_string())
            || fixture_value["window_end"].as_str() != Some(&end.to_string())
        {
            return Err("requested dates differ from parity fixture dates".into());
        }
        super::check_fixture(fixture, &book.sleeve_totals())?;
    }
    validate_end(&book, end, observed.get(&end).copied())?;
    let mut no_bar_days = Vec::new();
    let mut day = start;
    while day <= end {
        if !observed.contains_key(&day) {
            if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun)
                && !roll_manifest.contains(&day)
            {
                return Err(format!("no observed bars for weekday {day}; cannot assume a data gap is a closed exchange session"));
            }
            no_bar_days.push(day);
        }
        day = day.succ_opt().ok_or("date overflow")?;
    }
    let mut daily = BTreeMap::<NaiveDate, (Decimal, usize)>::new();
    for fill in &book.fills {
        if !fill.dollars.is_finite() {
            return Err("nonfinite fill dollars".into());
        }
        let session_end = fill
            .session_date
            .succ_opt()
            .ok_or("date overflow")?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        if fill.exit_ts >= session_end {
            return Err(format!(
                "{} fill remains open past UTC day boundary",
                fill.sleeve
            ));
        }
        let amount = Decimal::from_str(&fill.dollars.to_string()).map_err(|e| e.to_string())?;
        let entry = daily.entry(fill.session_date).or_default();
        entry.0 += amount;
        entry.1 += 1;
    }
    let steps = daily_steps(start, end, initial_cash, &daily)?;
    let flat_by = book.sleeves.iter().map(|s| s.cfg.flat_by).max().unwrap();
    let early_sessions: BTreeSet<_> = observed
        .iter()
        .filter(|(_, clock)| **clock < flat_by)
        .map(|(date, _)| *date)
        .collect();
    let early_flat_exits = book
        .fills
        .iter()
        .filter(|fill| fill.reason == "flat_by" && to_exchange_time(fill.exit_ts).time() < flat_by)
        .count();
    let report = serde_json::json!({
        "schema_version": 1,
        "initial_cash": initial_cash.to_string(),
        "steps": steps,
        "metadata": {
            "strategy": "NQ four-sleeve noise book",
            "source_bars": path,
            "source_rows": bars.len(),
            "source_first_timestamp": bars.first().unwrap().ts_utc,
            "source_last_timestamp": bars.last().unwrap().ts_utc,
            "session_start": start,
            "session_end": end,
            "exchange_timezone": "America/New_York",
            "nav_timestamp": "session date plus one day at UTC midnight, after the session closes",
            "capital_policy": "fixed one NQ contract per active sleeve; no reinvestment or sizing by NAV",
            "initial_cash": initial_cash.to_string(),
            "accounting_instrument": book.instrument,
            "contracts_per_sleeve": book.units,
            "max_simultaneous_sleeves": book.sleeves.len(),
            "point_value_dollars": book.costs.point_value,
            "tick_size": book.costs.tick_size,
            "slippage_ticks_per_side": book.costs.slippage_ticks,
            "round_trip_commission_per_contract": book.costs.commission,
            "pnl_basis": "sum of actual replay Fill.dollars by exchange session date, net of embedded slippage and commission",
            "holding_rule": format!("intraday; flat by {flat_by} exchange time; runtime retroactively closes early-ending sessions on their final bar"),
            "early_ending_sessions": early_sessions,
            "retroactive_early_flat_exits": early_flat_exits,
            "flat_calendar_days_without_bars": no_bar_days,
            "roll_manifest": options.get("--roll-manifest"),
            "documented_flat_roll_dates": roll_manifest.iter().filter(|date| **date >= start && **date <= end).collect::<Vec<_>>(),
            "halt": book.halted,
            "first_halt_timestamp": first_halt,
            "fills": book.fills.len(),
            "sleeve_totals": book.sleeve_totals(),
            "records_database_access": false,
            "limitations": [
                "Realized daily NAV is valid only for the simulated end-of-day flat book. It does not measure intraday drawdown or margin sufficiency.",
                "The core treats a next-session rollover as evidence to flatten an early-ending session at its last observed close. Missing late-session bars can therefore resemble an exchange early close; this export reports those sessions but cannot verify their exchange calendar.",
                "No order-size impact, limit-book liquidity, variation margin, interest on cash, or financing charge is added beyond the replay fill model.",
                "Calendar weekend days and explicitly documented excluded roll dates have zero P&L. Other missing weekdays are rejected because this command has no verified exchange holiday calendar.",
                "The legacy Book default names MNQ while pricing at NQ $20 per point. Research output explicitly names NQ and preserves the fixture's economics."
            ]
        }
    });
    std::fs::write(
        out,
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{out}: {e}"))?;
    eprintln!(
        "{} calendar days, {} completed fills -> {out}",
        steps.len(),
        book.fills.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    #[test]
    fn export_ignores_database_configuration() {
        let directory =
            std::env::temp_dir().join(format!("futures-research-db-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let bars = directory.join("bars.jsonl");
        std::fs::write(&bars, concat!(
            "{\"ts_utc\":\"2026-07-10T14:00:00Z\",\"open\":100,\"high\":101,\"low\":99,\"close\":100,\"volume\":1}\n",
            "{\"ts_utc\":\"2026-07-10T19:35:00Z\",\"open\":100,\"high\":101,\"low\":99,\"close\":100,\"volume\":1}\n"
        )).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "research::tests::database_export_child"])
            .env("DATABASE_URL", "this is deliberately not a database URL")
            .env("FUTURES_RESEARCH_TEST_DIRECTORY", &directory)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("daily.json")).unwrap()).unwrap();
        assert_eq!(report["metadata"]["records_database_access"], false);
        assert_eq!(report["steps"][0]["nav"], "100000");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn database_export_child() {
        let Ok(directory) = std::env::var("FUTURES_RESEARCH_TEST_DIRECTORY") else {
            return;
        };
        let args = vec![
            "--bars".into(),
            format!("{directory}/bars.jsonl"),
            "--start".into(),
            "2026-07-10".into(),
            "--end".into(),
            "2026-07-10".into(),
            "--research-out".into(),
            format!("{directory}/daily.json"),
            "--initial-cash".into(),
            "100000".into(),
        ];
        command(&args).unwrap();
    }

    #[test]
    fn pnl_uses_next_midnight_and_keeps_flat_weekends() {
        let start = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let end = start + TimeDelta::days(3);
        let daily = BTreeMap::from([
            (start, (Decimal::from(-100), 2)),
            (end, (Decimal::from(25), 1)),
        ]);
        let steps = daily_steps(start, end, Decimal::from(1000), &daily).unwrap();
        assert_eq!(
            steps.iter().map(|s| s.nav.as_str()).collect::<Vec<_>>(),
            ["900", "900", "900", "925"]
        );
        assert_eq!(steps[0].as_of, "2026-07-11T00:00:00Z");
        assert_eq!(steps[1].fills, 0);
    }
    #[test]
    fn incomplete_final_session_is_rejected_even_when_flat() {
        let book = Book::new(book_sleeves());
        let day = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();
        assert!(
            validate_end(&book, day, Some(NaiveTime::from_hms_opt(2, 30, 0).unwrap())).is_err()
        );
        assert!(validate_end(
            &book,
            day,
            Some(NaiveTime::from_hms_opt(15, 35, 0).unwrap())
        )
        .is_ok());
    }
    #[test]
    fn insufficient_realized_capital_is_rejected() {
        let day = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let daily = BTreeMap::from([(day, (Decimal::from(-1000), 1))]);
        assert!(daily_steps(day, day, Decimal::from(1000), &daily).is_err());
    }
}
