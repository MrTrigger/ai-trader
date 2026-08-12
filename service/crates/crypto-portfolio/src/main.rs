use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, NaiveDate, Utc};
use crypto_portfolio::config::Config;
use crypto_portfolio::liquidity;
use crypto_portfolio::{
    binance::Binance, decide, store, universe as universe_mod, write_plan, DecisionInput,
    DecisionResult, Portfolio,
};
use features_crypto::{daily, hourly, DailyRow, HourlyRow};
use plan::Mode;

const USAGE: &str = "\
usage: crypto-portfolio <command>

  plan --config <toml> --data-root <dir> --as-of <RFC3339>
       --book <json> --out <plan.json> [--for-execution]
      Compute a deterministic Plan entirely in Rust. The book is venue truth
      in the same {cash,positions} shape emitted by `bot book`.

  features --data-root <dir> --interval daily|hourly --out <jsonl>
           [--benchmark BTC]
      Emit the Rust-owned feature matrix. Python training may consume this
      output; it may not calculate or transform production features itself.

  universe-rank --config <toml> --data-root <dir> --as-of <RFC3339>
                [--end <RFC3339>] [--step-days N] [--top N]
                [--tradeable <json>] [--overwrite]
      Record a point-in-time liquidity-ranked universe using Rust store logic.

  liquidity-profile --config F --data-root D --out F [--hours 1,2] [--days 180]
                    [--spreads F]
      Median quote volume per name in the hours the bot trades, from the bar
      store, optionally merged with spreads measured off the live book by
      `hyperliquid --example book_capture --out-spreads`. This is the artefact
      that replaces the flat spread and the daily-volume impact denominator.

  universe-record --config <toml> --data-root <dir> --as-of <RFC3339>
                  [--overwrite]
      Record the configured Phase 0 universe without liquidity ranking.

  universe-list --data-root <dir>
      List recorded point-in-time universe snapshots.

  data-pull --config <toml> --data-root <dir> [--days N] [--end RFC3339]
            [--assets BTC,ETH] [--daily-only]
      Refresh Binance public daily and hourly bars into the Parquet store.

  data-archive --config <toml> --data-root <dir> --start RFC3339 --end RFC3339
               [--assets BTC,ETH | --all-listed] [--interval daily|hourly|both]
      Bulk-load Binance monthly archives, including delisted symbols, for
      survivorship-safe historical universe reconstruction.

  data-inspect --data-root <dir> [--json]
      Inventory every asset/interval partition without changing the store.

  data-verify --data-root <dir> [--interval daily|hourly] [--tolerance N]
              [--asset BTC] [--details N] [--strict-continuity]
              [--cross-interval] [--alignment-tolerance N]
      Verify UTC timestamp grids. Price continuity is reported as a warning;
      optional daily↔hourly aggregation provides stronger convention evidence.

  scores --config <toml> --data-root <dir> --as-of <RFC3339> [--by-cluster]
      Show the conventional candidate factor cross-section. This is a lens;
      neither planning nor model training consumes these scores.

  plan-verify --config <toml> --data-root <dir> --as-of <RFC3339>
              --book <json> [--runs N]
      Run one loaded decision snapshot repeatedly with varying wall-clock
      timestamps and require identical decision content and Plan IDs.

  model-check --model <json> --values <comma-separated> --as-of YYYY-MM-DD
      Evaluate an exported model vector through the Rust inference engine.

  training-matrix --config <toml> --data-root <dir> --start YYYY-MM-DD
                  --end YYYY-MM-DD --out <jsonl>
      Emit final rank-normalised model inputs and targets. A Python trainer may
      fit these rows but must not transform their feature values.

  backtest --config <toml> --data-root <dir> --start YYYY-MM-DD
           --end YYYY-MM-DD --initial-cash <quote> [--slippage-multiple N]
           [--out <json>]
      Replay the exact Rust live decision function and causal fill model.

  gate --config <toml> --data-root <dir> --start YYYY-MM-DD
       --end YYYY-MM-DD --initial-cash <quote> [--out <json>]
      Compare the candidate, stressed candidate, and named baseline. Exits
      non-zero unless every Phase 1 criterion passes.

  ic --config <toml> --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
     [--score FEATURE] [--horizons 7,14,30] [--out <json>]
      Measure rank correlation against tradeable forward returns using the
      same Rust feature rows and point-in-time universes as the planner.

  sweep --config <toml> --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
        --initial-cash <quote> --axis holdings|turnover|rebalance|constructor
        --values <comma-separated> [--out <json>]
      Sweep exactly one ordered axis and report the widest positive plateau.

  research --config <toml> --data-root <dir> --start YYYY-MM-DD --end YYYY-MM-DD
           --initial-cash <quote> --out <json>
      Build one internally consistent Rust evidence record for the declared
      window: data, universe, candidate replays, stress, walk-forward, and IC.

  report --record <json> --out <html>
      Render a self-contained disclosure-first HTML lens. Computes nothing.
";

#[derive(serde::Deserialize)]
struct UniverseDoc {
    members: Vec<UniverseMember>,
}
#[derive(serde::Deserialize)]
struct UniverseMember {
    asset: String,
    eligible: bool,
}

fn get(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|v| v == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn flag_i64(args: &[String], name: &str) -> Option<i64> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
}

fn need(args: &[String], name: &str) -> Result<String, String> {
    get(args, name).ok_or_else(|| format!("{name} is required"))
}

fn parse_as_of(text: &str) -> Result<DateTime<Utc>, String> {
    if text.len() == 10 {
        return format!("{text}T00:00:00Z")
            .parse()
            .map_err(|e| format!("bad --as-of: {e}"));
    }
    text.parse::<DateTime<Utc>>()
        .map_err(|e| format!("bad --as-of: {e}"))
}

fn universe(root: &Path, day: NaiveDate) -> Result<BTreeSet<String>, String> {
    let path = root.join("universe").join(format!("{day}.json"));
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("cannot read universe {}: {e}", path.display()))?;
    let doc: UniverseDoc =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(doc
        .members
        .into_iter()
        .filter(|m| m.eligible)
        .map(|m| m.asset.to_uppercase())
        .collect())
}

fn listings(root: &Path) -> Result<BTreeMap<String, NaiveDate>, String> {
    store::funding_listings(root)
}

struct LoadedDecision {
    config: Config,
    as_of: DateTime<Utc>,
    book: Portfolio,
    eligible: BTreeSet<String>,
    daily: Vec<DailyRow>,
    hourly: Option<Vec<HourlyRow>>,
    inputs_hash: String,
}

impl LoadedDecision {
    fn decide(&self, created_at: DateTime<Utc>, mode: Mode) -> Result<DecisionResult, String> {
        decide(DecisionInput {
            as_of: self.as_of,
            created_at,
            mode,
            config: &self.config,
            daily_features: &self.daily,
            hourly_features: self.hourly.as_deref(),
            eligible_universe: &self.eligible,
            portfolio: &self.book,
            inputs_hash: &self.inputs_hash,
        })
    }
}

fn load_decision(args: &[String]) -> Result<LoadedDecision, String> {
    let config = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let as_of = parse_as_of(&need(args, "--as-of")?)?;
    let book_path = PathBuf::from(need(args, "--book")?);
    let book: Portfolio = serde_json::from_slice(
        &std::fs::read(&book_path)
            .map_err(|e| format!("cannot read book {}: {e}", book_path.display()))?,
    )
    .map_err(|e| format!("book {}: {e}", book_path.display()))?;

    let eligible = universe(&root, as_of.date_naive())?;
    let mut needed = eligible.clone();
    needed.extend(book.positions.iter().map(|p| p.asset.clone()));
    if let Some(benchmark) = &config.benchmark {
        needed.insert(benchmark.clone());
    }
    let daily_horizon = as_of - chrono::Duration::seconds(config.interval_s);
    let bars: Vec<_> = store::read(&root, config.interval_s as i32)?
        .into_iter()
        .filter(|b| needed.contains(&b.asset) && b.ts_utc <= daily_horizon)
        .collect();
    if bars.is_empty() {
        return Err(format!("{} has no daily bars", root.display()));
    }
    let features = daily(
        &bars,
        config.benchmark.as_deref(),
        &listings(&root)?,
        &crypto_portfolio::funding::load(&root)?,
        features_crypto::FundingWindow::Trailing,
    )?;
    let (hourly_features, hourly_hash_bars) = if config.signal == "ml_ranker" {
        let hourly_horizon = as_of - chrono::Duration::hours(1);
        let bars: Vec<_> = store::read(&root, 3_600)?
            .into_iter()
            .filter(|b| needed.contains(&b.asset) && b.ts_utc <= hourly_horizon)
            .collect();
        let rows =
            features_crypto::hourly_before_daily_decision(&bars, config.benchmark.as_deref())?;
        (Some(rows), Some(bars))
    } else {
        (None, None)
    };
    let hash = match &hourly_hash_bars {
        Some(hourly) => store::content_hash_sets(&[&bars, hourly]),
        None => store::content_hash(&bars),
    };
    Ok(LoadedDecision {
        config,
        as_of,
        book,
        eligible,
        daily: features,
        hourly: hourly_features,
        inputs_hash: hash,
    })
}

fn cmd_plan(args: &[String]) -> Result<(), String> {
    let loaded = load_decision(args)?;
    let out = PathBuf::from(need(args, "--out")?);
    let result = loaded.decide(
        Utc::now(),
        if args.iter().any(|v| v == "--for-execution") {
            Mode::Live
        } else {
            Mode::Dry
        },
    )?;
    write_plan(&out, &result.plan)?;
    for note in &result.notes {
        eprintln!("note: {note}");
    }
    for skip in &result.skipped {
        eprintln!("skipped: {skip}");
    }
    println!(
        "{} {} orders -> {}",
        result.plan.status_string(),
        result.plan.orders.len(),
        out.display()
    );
    Ok(())
}

fn decision_digest(plan: &plan::Plan) -> Result<String, String> {
    use sha2::{Digest, Sha256};

    let mut value = serde_json::to_value(plan).map_err(|error| error.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or("Plan did not serialize as an object")?;
    object.remove("created_at");
    object.remove("plan_id");
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn cmd_plan_verify(args: &[String]) -> Result<(), String> {
    let loaded = load_decision(args)?;
    let runs = get(args, "--runs")
        .map(|value| value.parse::<usize>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(2);
    if runs < 2 {
        return Err("--runs must be at least 2".into());
    }
    let base = "2026-01-01T00:00:00Z"
        .parse::<DateTime<Utc>>()
        .map_err(|error| error.to_string())?;
    let mut digests = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for index in 0..runs {
        let result = loaded.decide(base + chrono::Duration::hours(index as i64), Mode::Dry)?;
        let digest = decision_digest(&result.plan)?;
        println!(
            "run {}: digest {}  plan_id {}",
            index + 1,
            &digest[..16],
            result.plan.plan_id
        );
        digests.insert(digest);
        ids.insert(result.plan.plan_id);
    }
    if digests.len() == 1 && ids.len() == 1 {
        println!("\nPASS: {runs} runs, identical decision content and Plan ID");
        Ok(())
    } else {
        Err("runs diverged: the decision path is not deterministic".into())
    }
}

trait StatusText {
    fn status_string(&self) -> &'static str;
}
impl StatusText for plan::Plan {
    fn status_string(&self) -> &'static str {
        match self.status {
            plan::Status::Accepted => "accepted",
            plan::Status::Rejected => "rejected",
            plan::Status::Superseded => "superseded",
            plan::Status::Executing => "executing",
            plan::Status::Executed => "executed",
            plan::Status::Failed => "failed",
        }
    }
}

fn write_jsonl<T: serde::Serialize>(path: &Path, rows: &[T]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    use std::io::Write;
    for row in rows {
        serde_json::to_writer(&mut out, row).map_err(|e| e.to_string())?;
        out.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())
}

fn cmd_features(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let out = PathBuf::from(need(args, "--out")?);
    let benchmark = get(args, "--benchmark").unwrap_or_else(|| "BTC".into());
    match need(args, "--interval")?.as_str() {
        "daily" => {
            let bars: Vec<_> = store::read(&root, 86_400)?
                .into_iter()
                .filter(|b| {
                    !b.asset.is_empty()
                        && b.asset.len() <= 20
                        && b.asset
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                })
                .collect();
            let rows = daily(
                &bars,
                Some(&benchmark),
                &listings(&root)?,
                &crypto_portfolio::funding::load(&root)?,
                features_crypto::FundingWindow::Trailing,
            )?;
            write_jsonl(&out, &rows)?;
            println!(
                "wrote {} Rust daily feature rows -> {}",
                rows.len(),
                out.display()
            );
        }
        "hourly" => {
            let bars: Vec<_> = store::read(&root, 3_600)?
                .into_iter()
                .filter(|b| {
                    !b.asset.is_empty()
                        && b.asset.len() <= 20
                        && b.asset
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                })
                .collect();
            let rows = hourly(&bars, Some(&benchmark))?;
            write_jsonl(&out, &rows)?;
            println!(
                "wrote {} Rust hourly feature rows -> {}",
                rows.len(),
                out.display()
            );
        }
        other => return Err(format!("unknown interval {other:?}; use daily or hourly")),
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct TradeableDoc {
    assets: Vec<String>,
}

fn cmd_universe_rank(args: &[String]) -> Result<(), String> {
    let cfg = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--as-of")?)?;
    let end = get(args, "--end")
        .map(|value| parse_as_of(&value))
        .transpose()?
        .unwrap_or(start);
    if end < start {
        return Err("--end precedes --as-of".into());
    }
    let step_days = get(args, "--step-days")
        .map(|value| value.parse::<i64>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(cfg.rebalance_every.max(1) as i64);
    if step_days <= 0 {
        return Err("--step-days must be positive".into());
    }
    let top = get(args, "--top")
        .map(|v| v.parse::<usize>().map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or(30);
    let tradeable = get(args, "--tradeable")
        .map(|path| {
            let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
            let doc: TradeableDoc =
                serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok::<BTreeSet<String>, String>(
                doc.assets.into_iter().map(|v| v.to_uppercase()).collect(),
            )
        })
        .transpose()?;
    let bars = store::read(&root, cfg.interval_s as i32)?;
    let mut as_of = start;
    let mut snapshots = 0usize;
    while as_of <= end {
        let members = universe_mod::by_liquidity(
            &bars,
            as_of,
            top,
            30,
            cfg.min_history_bars as usize,
            cfg.min_dollar_volume
                .to_string()
                .parse::<f64>()
                .map_err(|error| error.to_string())?,
            tradeable.as_ref(),
        )?;
        let eligible = members.iter().filter(|m| m.eligible).count();
        let path = universe_mod::write(
            &root,
            as_of,
            "rust-liquidity-rank",
            members,
            args.iter().any(|v| v == "--overwrite"),
        )?;
        println!("recorded {eligible} eligible assets -> {}", path.display());
        snapshots += 1;
        as_of += chrono::Duration::days(step_days);
    }
    println!("recorded {snapshots} point-in-time snapshot(s)");
    Ok(())
}

fn cmd_universe_record(args: &[String]) -> Result<(), String> {
    let config = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let as_of = parse_as_of(&need(args, "--as-of")?)?;
    let members = config
        .universe
        .iter()
        .enumerate()
        .map(|(index, asset)| universe_mod::Member {
            asset: asset.clone(),
            rank: index + 1,
            eligible: true,
            reason: "configured (Phase 0)".into(),
        })
        .collect();
    let path = universe_mod::write(
        &root,
        as_of,
        "rust-config",
        members,
        args.iter().any(|value| value == "--overwrite"),
    )?;
    println!(
        "recorded {} configured members -> {}",
        config.universe.len(),
        path.display()
    );
    Ok(())
}

fn cmd_universe_list(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let dir = root.join("universe");
    if !dir.exists() {
        println!("no universe snapshots");
        return Ok(());
    }
    let mut snapshots = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().is_none_or(|value| value != "json") {
            continue;
        }
        let snapshot: universe_mod::Snapshot = serde_json::from_slice(
            &std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?,
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
        snapshots.push(snapshot);
    }
    snapshots.sort_by_key(|snapshot| snapshot.as_of);
    for snapshot in snapshots {
        println!(
            "{}  {:>3}/{:<3} eligible  {:<24} recorded {}",
            snapshot.as_of.date_naive(),
            snapshot
                .members
                .iter()
                .filter(|member| member.eligible)
                .count(),
            snapshot.members.len(),
            snapshot.source,
            snapshot.recorded_at
        );
    }
    Ok(())
}

fn cmd_data_inspect(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let rows = crypto_portfolio::inspect::inventory(&root)?;
    if args.iter().any(|value| value == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).map_err(|error| error.to_string())?
        );
    } else if rows.is_empty() {
        println!("store is empty");
    } else {
        for row in rows {
            println!(
                "{:<8} {:>7}s  {:>8} bars  {} .. {}  {}",
                row.asset,
                row.interval_s,
                row.rows,
                row.first_ts.format("%Y-%m-%d"),
                row.last_ts.format("%Y-%m-%d"),
                row.content_hash
            );
        }
    }
    Ok(())
}

fn cmd_data_verify(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let interval = match get(args, "--interval").as_deref().unwrap_or("daily") {
        "daily" => 86_400,
        "hourly" => 3_600,
        other => return Err(format!("unknown interval {other:?}; use daily or hourly")),
    };
    let tolerance = get(args, "--tolerance")
        .map(|value| value.parse::<f64>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(0.005);
    let asset = get(args, "--asset").map(|value| value.to_uppercase());
    let bars = match &asset {
        Some(asset) => store::read_asset(&root, interval, asset)?,
        None => store::read(&root, interval)?,
    };
    if bars.is_empty() {
        return Err(format!("store has no {interval}s bars"));
    }
    let timestamp_reports = crypto_portfolio::inspect::timestamp_grid(&bars);
    let timestamp_failures = timestamp_reports
        .iter()
        .filter(|report| !report.ok())
        .count();
    println!("TIMESTAMP GRID");
    for report in timestamp_reports {
        println!("{}", report.render());
    }
    let reports = crypto_portfolio::inspect::continuity(&bars, tolerance)?;
    let continuity_failures = reports.iter().filter(|report| !report.ok()).count();
    println!("\nPRICE CONTINUITY (heuristic; thin markets may legitimately gap)");
    for report in reports {
        println!("{}", report.render());
    }
    let detail_count = get(args, "--details")
        .map(|value| value.parse::<usize>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(0);
    if detail_count > 0 {
        let breaks = crypto_portfolio::inspect::continuity_breaks(&bars, tolerance)?;
        println!("\nworst continuity breaks:");
        for item in breaks.into_iter().take(detail_count) {
            println!(
                "  {} {} -> {}: close {:.12} / open {:.12}, gap {:.4}%",
                item.asset,
                item.previous_ts,
                item.current_ts,
                item.previous_close,
                item.current_open,
                item.relative_gap * 100.0
            );
        }
    }
    let strict = args.iter().any(|value| value == "--strict-continuity");
    if continuity_failures > 0 && !strict {
        println!(
            "note: {continuity_failures} series have price gaps above tolerance; use --strict-continuity to make those fatal"
        );
    }
    let mut alignment_failures = 0;
    if args.iter().any(|value| value == "--cross-interval") {
        if interval != 86_400 {
            return Err("--cross-interval requires --interval daily".into());
        }
        let hourly = match &asset {
            Some(asset) => store::read_asset(&root, 3_600, asset)?,
            None => store::read(&root, 3_600)?,
        };
        if hourly.is_empty() {
            return Err("cross-interval verification found no hourly bars".into());
        }
        let alignment_tolerance = get(args, "--alignment-tolerance")
            .map(|value| value.parse::<f64>().map_err(|error| error.to_string()))
            .transpose()?
            .unwrap_or(0.0005);
        let alignment =
            crypto_portfolio::inspect::daily_hourly_alignment(&bars, &hourly, alignment_tolerance)?;
        if alignment.is_empty() {
            return Err("cross-interval verification found no overlapping complete days".into());
        }
        println!("\nDAILY ↔ HOURLY OHLC ALIGNMENT");
        for report in alignment {
            if !report.ok() {
                alignment_failures += 1;
            }
            println!("{}", report.render());
            for sample in &report.mismatch_samples {
                println!(
                    "  {} {}: daily {:.12}, hourly aggregate {:.12}, error {:.8}%",
                    sample.ts_utc,
                    sample.field,
                    sample.daily_value,
                    sample.hourly_aggregate,
                    sample.relative_error * 100.0
                );
            }
        }
    }
    let fatal_continuity = if strict { continuity_failures } else { 0 };
    let failures = timestamp_failures + alignment_failures + fatal_continuity;
    if failures > 0 {
        Err(format!(
            "verification failed: {timestamp_failures} timestamp, {alignment_failures} alignment, {fatal_continuity} strict continuity series"
        ))
    } else {
        Ok(())
    }
}

fn cmd_scores(args: &[String]) -> Result<(), String> {
    let config = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let as_of = parse_as_of(&need(args, "--as-of")?)?;
    let horizon = as_of - chrono::Duration::seconds(config.interval_s);
    let eligible = universe(&root, as_of.date_naive())?;
    let mut needed = eligible.clone();
    if let Some(benchmark) = &config.benchmark {
        needed.insert(benchmark.clone());
    }
    let bars: Vec<_> = store::read(&root, config.interval_s as i32)?
        .into_iter()
        .filter(|bar| needed.contains(&bar.asset) && bar.ts_utc <= horizon)
        .collect();
    if bars.is_empty() {
        return Err(format!("no bars at or before {horizon}"));
    }
    let frame = daily(
        &bars,
        config.benchmark.as_deref(),
        &listings(&root)?,
        &crypto_portfolio::funding::load(&root)?,
        features_crypto::FundingWindow::Trailing,
    )?;
    let mut latest: BTreeMap<String, &DailyRow> = BTreeMap::new();
    for row in frame.iter().filter(|row| eligible.contains(&row.asset)) {
        if latest
            .get(&row.asset)
            .is_none_or(|previous| previous.ts_utc < row.ts_utc)
        {
            latest.insert(row.asset.clone(), row);
        }
    }
    let cross = latest.into_values().collect::<Vec<_>>();
    let grouped = args.iter().any(|value| value == "--by-cluster");
    let result = crypto_portfolio::scores::baseline(
        &cross,
        grouped.then_some(&config.clusters),
        crypto_portfolio::scores::DEFAULT_MIN_GROUP_SIZE,
    );
    println!("DISCLOSURES (read before any number below)");
    println!(
        "  ! these factors are a candidate cross-section, not a chosen strategy; they claim no edge"
    );
    for disclosure in &result.disclosures {
        println!("  ! {disclosure}");
    }
    println!("\nas of {as_of}   horizon {horizon}");
    println!(
        "scoring {}   features {}",
        result.scoring_version,
        features_crypto::FEATURE_SET_VERSION
    );
    println!(
        "grouped by {}",
        if grouped {
            "configured clusters"
        } else {
            "all (one cross-section)"
        }
    );
    println!(
        "\n{:<8}{:<14}{:>10}{:>12}{:>12}{:>12}   flags",
        "asset", "group", "composite", "momentum", "low_vol", "liquidity"
    );
    for row in result.rows {
        println!(
            "{:<8}{:<14}{:>10.1}{:>12.1}{:>12.1}{:>12.1}   {}",
            row.asset,
            row.group_key,
            row.composite,
            row.momentum,
            row.low_vol,
            row.liquidity,
            row.degenerate_flags.join(", ")
        );
    }
    Ok(())
}

fn cmd_data_pull(args: &[String]) -> Result<(), String> {
    let cfg = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let end = get(args, "--end")
        .map(|v| parse_as_of(&v))
        .transpose()?
        .unwrap_or_else(|| {
            let now = Utc::now();
            now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc()
        });
    let days = get(args, "--days")
        .map(|v| v.parse::<i64>().map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or(3);
    let start = end - chrono::Duration::days(days);
    let assets = if let Some(value) = get(args, "--assets") {
        value
            .split(',')
            .filter(|v| !v.is_empty())
            .map(|v| v.trim().to_uppercase())
            .collect()
    } else {
        let known = store::known_assets(&root)?;
        if known.is_empty() {
            cfg.universe.clone()
        } else {
            known
        }
    };
    let intervals = if args.iter().any(|v| v == "--daily-only") {
        vec![cfg.interval_s as i32]
    } else {
        vec![cfg.interval_s as i32, 3_600]
    };
    let source = Binance::new()?;
    let mut written = 0;
    for interval in intervals {
        for asset in &assets {
            match source.fetch(asset, interval, start, end) {
                Ok(rows) if !rows.is_empty() => {
                    written += rows.len();
                    store::write(&root, &rows)?;
                }
                Ok(_) => eprintln!("{asset} @{interval}s: no bars"),
                Err(e) => eprintln!("{asset} @{interval}s: {e}"),
            }
        }
    }
    if written == 0 {
        return Err("no bars fetched".into());
    }
    println!("wrote {written} Binance bars");
    Ok(())
}

fn cmd_data_archive(args: &[String]) -> Result<(), String> {
    let config = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--start")?)?;
    let end = parse_as_of(&need(args, "--end")?)?;
    let source = crypto_portfolio::binance_archive::BinanceArchive::new()?;
    let explicit = get(args, "--assets");
    let all = args.iter().any(|value| value == "--all-listed");
    if explicit.is_some() && all {
        return Err("use either --assets or --all-listed, not both".into());
    }
    let assets = if all {
        source.listed_assets(false)?
    } else if let Some(value) = explicit {
        value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_uppercase)
            .collect::<Vec<_>>()
    } else {
        let known = store::known_assets(&root)?;
        if known.is_empty() {
            config.universe.clone()
        } else {
            known
        }
    };
    if assets.is_empty() {
        return Err("no archive assets selected".into());
    }
    let intervals = match get(args, "--interval").as_deref().unwrap_or("both") {
        "daily" => vec![86_400],
        "hourly" => vec![3_600],
        "both" => vec![86_400, 3_600],
        other => return Err(format!("unknown archive interval {other:?}")),
    };
    println!(
        "archive pull: {} asset(s), {}..{}, {:?}",
        assets.len(),
        start.date_naive(),
        end.date_naive(),
        intervals
    );
    let mut written = 0;
    let mut absent = 0;
    for interval in intervals {
        for asset in &assets {
            match source.fetch(asset, interval, start, end) {
                Ok(rows) if rows.is_empty() => absent += 1,
                Ok(rows) => {
                    written += rows.len();
                    store::write(&root, &rows)?;
                }
                Err(error) => eprintln!("{asset} @{interval}s: {error}"),
            }
        }
    }
    if written == 0 {
        return Err(format!("archive returned no bars ({absent} absent series)"));
    }
    println!("wrote {written} archive bars; {absent} asset/interval series absent");
    Ok(())
}

fn cmd_model_check(args: &[String]) -> Result<(), String> {
    let model = crypto_portfolio::model::Model::load(Path::new(&need(args, "--model")?))?;
    let values = need(args, "--values")?
        .split(',')
        .map(|v| v.parse::<f64>().map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let as_of = need(args, "--as-of")?
        .parse::<NaiveDate>()
        .map_err(|e| e.to_string())?;
    println!("{:.17}", model.predict(&values, as_of)?);
    Ok(())
}

fn cmd_training_matrix(args: &[String]) -> Result<(), String> {
    let cfg = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = need(args, "--start")?
        .parse::<NaiveDate>()
        .map_err(|e| e.to_string())?;
    let end = need(args, "--end")?
        .parse::<NaiveDate>()
        .map_err(|e| e.to_string())?;
    let path = PathBuf::from(need(args, "--out")?);
    let funding_window = if args.iter().any(|a| a == "--leaky-funding-diagnostic") {
        eprintln!(
            "training-matrix: FORWARD funding windows - future realised rates in the \
             features. Parity diagnostic ONLY; models fit on this matrix recite the \
             future and every number downstream of them is fiction."
        );
        features_crypto::FundingWindow::ForwardLeakyDiagnostic
    } else {
        features_crypto::FundingWindow::Trailing
    };
    // The label must match the hold: a 2-day cadence trained on a 24h target
    // takes positions on one day of signal and a day of drift. Lag stays 1h -
    // that is the execution path - but the hold is the cadence in hours.
    let lag_hours = flag_i64(args, "--lag-hours").unwrap_or(1);
    let hold_hours = flag_i64(args, "--hold-hours").unwrap_or(24);
    let matrix = crypto_portfolio::training::build(
        &root,
        &cfg,
        start,
        end,
        lag_hours,
        hold_hours,
        funding_window,
        args.iter().any(|a| a == "--include-unlisted-training"),
        flag_i64(args, "--step-hours").unwrap_or(24),
    )?;
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut out = std::io::BufWriter::new(file);
    use std::io::Write;
    serde_json::to_writer(
        &mut out,
        &serde_json::json!({
            "kind": "manifest",
            "feature_set_version": features_crypto::FEATURE_SET_VERSION,
            "features": matrix.features,
        }),
    )
    .map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())?;
    for row in &matrix.rows {
        serde_json::to_writer(&mut out, row).map_err(|e| e.to_string())?;
        out.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;
    println!(
        "wrote {} final Rust training rows -> {}",
        matrix.rows.len(),
        path.display()
    );
    Ok(())
}

fn cmd_backtest(args: &[String]) -> Result<(), String> {
    use std::str::FromStr;

    let cfg = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--start")?)?;
    let end = parse_as_of(&need(args, "--end")?)?;
    let initial_cash = rust_decimal::Decimal::from_str(&need(args, "--initial-cash")?)
        .map_err(|error| format!("bad --initial-cash: {error}"))?;
    let slippage = get(args, "--slippage-multiple")
        .map(|value| {
            rust_decimal::Decimal::from_str(&value)
                .map_err(|error| format!("bad --slippage-multiple: {error}"))
        })
        .transpose()?
        .unwrap_or(rust_decimal::Decimal::ONE);
    let funding_window = if args.iter().any(|a| a == "--leaky-funding-diagnostic") {
        eprintln!("backtest: FORWARD funding windows - parity diagnostic ONLY.");
        features_crypto::FundingWindow::ForwardLeakyDiagnostic
    } else {
        features_crypto::FundingWindow::Trailing
    };
    let result = crypto_portfolio::backtest::replay(
        &cfg,
        start,
        end,
        &root,
        initial_cash,
        slippage,
        funding_window,
        flag_i64(args, "--step-hours"),
    )?;
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if let Some(path) = get(args, "--out") {
        std::fs::write(&path, bytes).map_err(|error| format!("{path}: {error}"))?;
        println!(
            "{} rebalances, return {} -> {path}",
            result.metrics.n, result.metrics.total_return
        );
    } else {
        print!(
            "{}",
            String::from_utf8(bytes).map_err(|error| error.to_string())?
        );
    }
    Ok(())
}

fn cmd_gate(args: &[String]) -> Result<(), String> {
    use std::str::FromStr;

    let cfg = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--start")?)?;
    let end = parse_as_of(&need(args, "--end")?)?;
    let initial_cash = rust_decimal::Decimal::from_str(&need(args, "--initial-cash")?)
        .map_err(|error| format!("bad --initial-cash: {error}"))?;
    let result = crypto_portfolio::gate::run(
        &cfg,
        start,
        end,
        &root,
        initial_cash,
        "liquidity_top",
        "equal_weight",
    )?;
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if let Some(path) = get(args, "--out") {
        std::fs::write(&path, bytes).map_err(|error| format!("{path}: {error}"))?;
        println!(
            "Phase 1 gate {} -> {path}",
            if result.passed {
                "PASSED"
            } else {
                "NOT PASSED"
            }
        );
    } else {
        print!(
            "{}",
            String::from_utf8(bytes).map_err(|error| error.to_string())?
        );
    }
    if result.passed {
        Ok(())
    } else {
        Err("Phase 1 gate did not pass".into())
    }
}

fn cmd_ic(args: &[String]) -> Result<(), String> {
    let cfg = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--start")?)?;
    let end = parse_as_of(&need(args, "--end")?)?;
    let score = get(args, "--score").unwrap_or_else(|| "ret_30_skip_7".into());
    let horizons = get(args, "--horizons")
        .unwrap_or_else(|| "7,14,30".into())
        .split(',')
        .map(|value| value.parse::<i64>().map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let result = crypto_portfolio::ic::measure(&cfg, start, end, &root, &score, &horizons)?;
    let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "score": score,
        "results": result,
        "disclosures": [
            "IC measures the signal, not portfolio profitability after costs",
            "periods, not correlated assets within a cross-section, are the independent unit"
        ]
    }))
    .map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if let Some(path) = get(args, "--out") {
        std::fs::write(&path, bytes).map_err(|error| format!("{path}: {error}"))?;
        println!("wrote Rust IC evidence -> {path}");
    } else {
        print!(
            "{}",
            String::from_utf8(bytes).map_err(|error| error.to_string())?
        );
    }
    Ok(())
}

fn cmd_sweep(args: &[String]) -> Result<(), String> {
    use std::str::FromStr;

    let config = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--start")?)?;
    let end = parse_as_of(&need(args, "--end")?)?;
    let initial_cash = rust_decimal::Decimal::from_str(&need(args, "--initial-cash")?)
        .map_err(|error| format!("bad --initial-cash: {error}"))?;
    let axis = need(args, "--axis")?;
    let values = need(args, "--values")?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err("--values must contain at least one value".into());
    }
    let configs = values
        .into_iter()
        .map(|value| {
            let mut cfg = config.clone();
            match axis.as_str() {
                "holdings" => {
                    cfg.max_holdings = value.parse::<usize>().map_err(|error| error.to_string())?;
                    if cfg.max_holdings == 0 {
                        return Err("holdings values must be positive".into());
                    }
                }
                "turnover" => {
                    cfg.turnover_budget = rust_decimal::Decimal::from_str(&value)
                        .map_err(|error| error.to_string())?;
                    if cfg.turnover_budget < rust_decimal::Decimal::ZERO {
                        return Err("turnover values must be non-negative".into());
                    }
                }
                "rebalance" => {
                    cfg.rebalance_every =
                        value.parse::<usize>().map_err(|error| error.to_string())?;
                    if cfg.rebalance_every == 0 {
                        return Err("rebalance values must be positive".into());
                    }
                }
                "constructor" => cfg.constructor = value.clone(),
                other => {
                    return Err(format!(
                        "unknown sweep axis {other:?}; use holdings, turnover, rebalance, or constructor"
                    ));
                }
            }
            Ok((value, cfg))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let result =
        crypto_portfolio::validate::sweep_axis(&axis, configs, start, end, &root, initial_cash)?;
    let mut bytes = serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if let Some(path) = get(args, "--out") {
        std::fs::write(&path, bytes).map_err(|error| format!("{path}: {error}"))?;
        println!(
            "{}: plateau width {}, centre {} -> {path}",
            result.axis,
            result.plateau.width,
            result.plateau.centre.as_deref().unwrap_or("none")
        );
    } else {
        print!(
            "{}",
            String::from_utf8(bytes).map_err(|error| error.to_string())?
        );
    }
    Ok(())
}

fn cmd_research(args: &[String]) -> Result<(), String> {
    use std::str::FromStr;

    let config = Config::load(Path::new(&need(args, "--config")?))?;
    let root = PathBuf::from(need(args, "--data-root")?);
    let start = parse_as_of(&need(args, "--start")?)?;
    let end = parse_as_of(&need(args, "--end")?)?;
    let cash = rust_decimal::Decimal::from_str(&need(args, "--initial-cash")?)
        .map_err(|error| format!("bad --initial-cash: {error}"))?;
    let out = PathBuf::from(need(args, "--out")?);
    let record = crypto_portfolio::research::build(&config, &root, start, end, cash)?;
    let mut bytes = serde_json::to_vec_pretty(&record).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    std::fs::write(&out, bytes).map_err(|error| format!("{}: {error}", out.display()))?;
    println!(
        "wrote one-window Rust research record ({} runs, {} IC horizons) -> {}",
        record.runs.len(),
        record.ic.len(),
        out.display()
    );
    Ok(())
}

fn cmd_report(args: &[String]) -> Result<(), String> {
    let record = PathBuf::from(need(args, "--record")?);
    let out = PathBuf::from(need(args, "--out")?);
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&record)
            .map_err(|error| format!("cannot read {}: {error}", record.display()))?,
    )
    .map_err(|error| format!("{}: {error}", record.display()))?;
    let html = crypto_portfolio::report::render(&value)?;
    std::fs::write(&out, html.as_bytes()).map_err(|error| format!("{}: {error}", out.display()))?;
    println!(
        "wrote {} ({}kb, self-contained)",
        out.display(),
        html.len() / 1024
    );
    Ok(())
}

/// Build the measured liquidity artefact the cost model reads.
///
/// Volume comes from the bar store, taken as the median over the hours the bot
/// actually sends orders in — that is the liquidity a slice meets, and it is
/// what the participation cap is a fraction of. Spread cannot come from bars
/// at all (OHLCV has no bid or ask), so it is merged in from a live book
/// capture when one has been run, and simply left absent otherwise. Absent is
/// safe: the caller falls back to the flat model rather than assuming a name
/// is free to trade.
fn cmd_liquidity_profile(args: &[String]) -> Result<(), String> {
    let root = PathBuf::from(need(args, "--data-root")?);
    let out = PathBuf::from(need(args, "--out")?);
    let days: i64 = get(args, "--days")
        .map(|v| v.parse().map_err(|e| format!("--days: {e}")))
        .transpose()?
        .unwrap_or(180);
    let hours: Vec<u32> = get(args, "--hours")
        .unwrap_or_else(|| "1,2".into())
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse::<u32>().map_err(|e| format!("--hours: {e}")))
        .collect::<Result<_, String>>()?;
    if hours.is_empty() {
        return Err("--hours must name at least one hour".into());
    }

    let assets = store::known_assets(&root)?;
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let mut profile = liquidity::Profile {
        measured_at: chrono::Utc::now().to_rfc3339(),
        hours: hours.clone(),
        assets: Default::default(),
    };
    for asset in &assets {
        let Ok(bars) = store::read_asset(&root, 3600, asset) else {
            continue;
        };
        let mut vols: Vec<f64> = bars
            .iter()
            .filter(|b| b.ts_utc >= cutoff)
            .filter(|b| hours.contains(&chrono::Timelike::hour(&b.ts_utc)))
            .filter_map(|b| b.quote_volume.or(Some(b.volume * b.close)))
            .filter(|v| *v > 0.0)
            .collect();
        // A handful of bars is not a median. Better no number than a number
        // built from a week of a name's first month of listing.
        if vols.len() < 30 {
            continue;
        }
        vols.sort_by(f64::total_cmp);
        let median = vols[vols.len() / 2];
        profile.assets.insert(
            asset.clone(),
            liquidity::AssetLiquidity {
                hourly_quote_volume: rust_decimal::Decimal::from_f64_retain(median)
                    .map(|v| v.round_dp(2)),
                ..Default::default()
            },
        );
    }

    // Spreads, if a book capture has left any. Merged rather than replacing,
    // because the two measurements come from different places and neither is
    // a substitute for the other.
    let mut with_spread = 0usize;
    if let Some(path) = get(args, "--spreads") {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let measured: std::collections::BTreeMap<String, (f64, u32)> =
            serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        for (asset, (bps, samples)) in measured {
            let entry = profile.assets.entry(asset).or_default();
            entry.spread_bps = rust_decimal::Decimal::from_f64_retain(bps).map(|v| v.round_dp(3));
            entry.spread_samples = samples;
            with_spread += 1;
        }
    }

    profile.write(&out)?;
    println!(
        "{} names with hourly volume, {with_spread} with a measured spread -> {}",
        profile.assets.len(),
        out.display()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("plan") => cmd_plan(&args[1..]),
        Some("plan-verify") => cmd_plan_verify(&args[1..]),
        Some("features") => cmd_features(&args[1..]),
        Some("universe-rank") => cmd_universe_rank(&args[1..]),
        Some("liquidity-profile") => cmd_liquidity_profile(&args[1..]),
        Some("universe-record") => cmd_universe_record(&args[1..]),
        Some("universe-list") => cmd_universe_list(&args[1..]),
        Some("data-pull") => cmd_data_pull(&args[1..]),
        Some("data-archive") => cmd_data_archive(&args[1..]),
        Some("data-inspect") => cmd_data_inspect(&args[1..]),
        Some("data-verify") => cmd_data_verify(&args[1..]),
        Some("scores") => cmd_scores(&args[1..]),
        Some("model-check") => cmd_model_check(&args[1..]),
        Some("training-matrix") => cmd_training_matrix(&args[1..]),
        Some("backtest") => cmd_backtest(&args[1..]),
        Some("gate") => cmd_gate(&args[1..]),
        Some("ic") => cmd_ic(&args[1..]),
        Some("sweep") => cmd_sweep(&args[1..]),
        Some("research") => cmd_research(&args[1..]),
        Some("report") => cmd_report(&args[1..]),
        Some("-h" | "--help") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown command {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("crypto-portfolio: {e}");
            ExitCode::FAILURE
        }
    }
}
