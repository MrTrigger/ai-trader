//! The executor process: the only thing here that can move capital.
//!
//! ```text
//! bot --config bot.json run --plan plan.json
//! bot --config bot.json status
//! bot --config bot.json history --limit 10
//! bot --config bot.json halt   --reason "drawdown" --by magnus
//! bot --config bot.json resume --reason "reviewed"  --by magnus
//! bot --config bot.json pause  --reason "thin book" --by magnus
//! bot --config bot.json flatten --reason "weekend" --by magnus --confirm
//! ```
//!
//! # One process per run, not a daemon
//!
//! Cron starts it, it does one thing, it exits. A long-lived process would have
//! to be told to reload its controls, would hold venue state only it could see,
//! and would need its own supervision story. A process that exits has none of
//! those problems: every piece of state it cares about is on disk before it
//! goes, and the next invocation reads it fresh.
//!
//! The execution window is the one exception — `run` stays alive across its
//! slices, because that *is* the run.
//!
//! # It decides nothing
//!
//! Per design spec §3.3 the planner decides and the executor executes. This
//! binary reads a plan someone else produced, and refuses it if it is stale, if
//! it has run before, or if the risk gate did not pass. It does not re-price,
//! re-size, or re-plan. The one thing it originates is `flatten`, which only
//! ever reduces exposure and only ever with a named human attached.
//!
//! # Credentials
//!
//! None yet: the only venue implemented is `paper`. When a live adapter lands,
//! its key belongs to this process alone and must be trade-scoped with no
//! withdrawal rights.

mod config;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use config::BotConfig;
use paper::{PaperConfig, PaperVenue};
use runner::{ControlFile, RunStore, Runner};
use venue::{ManualPrices, SystemClock, VenueAdapter};

type Venue = PaperVenue<Arc<ManualPrices>, SystemClock>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bot: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
usage: bot --config <file> <command>

commands:
  run --plan <file>                  execute a plan across its slices
  status                             health, controls and the last run
  history [--limit N]                recent runs, newest first
  positions                          what the venue says we hold
  halt   --reason R --by WHO         stop executing (planning continues)
  pause  --reason R --by WHO         risk-reducing orders only
  resume --reason R --by WHO         permit trading again
  flatten --reason R --by WHO --confirm
                                     close every open position at market
";

fn run(args: Vec<String>) -> Result<(), String> {
    let mut config_path: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => {
                config_path = Some(PathBuf::from(
                    it.next().ok_or("--config needs a path".to_string())?,
                ))
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => {
                rest.push(a);
                rest.extend(it);
                break;
            }
        }
    }

    let cfg =
        BotConfig::load(&config_path.ok_or_else(|| format!("--config is required\n{USAGE}"))?)?;
    let command = rest.first().cloned().unwrap_or_default();
    let flags = Flags::parse(&rest[rest.len().min(1)..]);

    match command.as_str() {
        "run" => {
            let plan_path = flags.need("--plan")?;
            block_on(cmd_run(&cfg, PathBuf::from(plan_path)))
        }
        "status" => cmd_status(&cfg),
        "history" => cmd_history(&cfg, flags.get("--limit").and_then(|s| s.parse().ok())),
        "positions" => block_on(cmd_positions(&cfg)),
        "halt" => cmd_control(&cfg, true, false, &flags),
        "pause" => cmd_control(&cfg, false, true, &flags),
        "resume" => cmd_control(&cfg, false, false, &flags),
        "flatten" => {
            if !flags.has("--confirm") {
                return Err("flatten closes every open position at market. Re-run with \
                            --confirm if that is what you mean."
                    .into());
            }
            block_on(cmd_flatten(&cfg, &flags))
        }
        "" => Err(format!("no command given\n{USAGE}")),
        other => Err(format!("unknown command '{other}'\n{USAGE}")),
    }
}

/// A tokio runtime, built here rather than by `#[tokio::main]`.
///
/// Half these commands never touch the network; giving them a thread pool at
/// startup buys nothing and slows the ones a human is waiting on.
fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}

struct Flags(Vec<String>);

impl Flags {
    fn parse(args: &[String]) -> Self {
        Self(args.to_vec())
    }

    fn get(&self, name: &str) -> Option<String> {
        self.0
            .iter()
            .position(|a| a == name)
            .and_then(|i| self.0.get(i + 1))
            .cloned()
    }

    fn has(&self, name: &str) -> bool {
        self.0.iter().any(|a| a == name)
    }

    fn need(&self, name: &str) -> Result<String, String> {
        self.get(name).ok_or_else(|| format!("{name} is required"))
    }
}

// --- the venue --------------------------------------------------------------

/// Load the venue, restoring whatever it knew last time.
///
/// The paper venue keeps its books in memory, so without the snapshot every
/// invocation would start flat and the executor would reconcile a real
/// intention against a book that had forgotten everything.
fn open_venue(cfg: &BotConfig) -> Result<Venue, String> {
    let marks = config::read_marks(&cfg.marks_path())?;
    let prices = Arc::new(ManualPrices::new());
    for (asset, price) in &marks {
        prices.set(asset, *price);
    }

    let paper_cfg = PaperConfig {
        quote_currency: cfg.quote_currency.clone(),
        initial_cash: cfg.initial_cash,
        taker_fee_bps: cfg.taker_fee_bps,
        maker_fee_bps: cfg.maker_fee_bps,
        slippage_bps: cfg.slippage_bps,
        quote_decimals: 8,
    };

    let path = cfg.venue_state_path();
    match std::fs::read_to_string(&path) {
        Ok(snapshot) => {
            PaperVenue::restore(paper_cfg, cfg.markets(), prices, SystemClock, &snapshot).map_err(
                |e| {
                    // Never silently start fresh. A venue state file that exists but
                    // cannot be read means the book is unknown, and an unknown book is
                    // the one thing that must never be traded against.
                    format!(
                "venue state at {} is unreadable ({e}). Refusing to start with an empty book: \
                 that would look like a flat account and trade as one.",
                path.display()
            )
                },
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(PaperVenue::new(
            paper_cfg,
            cfg.markets(),
            prices,
            SystemClock,
        )),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write the venue's books back out. Called whether the run succeeded or not:
/// a failed run may still have submitted orders, and those fills are real.
async fn save_venue(cfg: &BotConfig, venue: &Venue) -> Result<(), String> {
    let path = cfg.venue_state_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Write-then-rename, so a crash mid-write cannot leave a truncated book.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, venue.snapshot().await)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

// --- commands ---------------------------------------------------------------

async fn cmd_run(cfg: &BotConfig, plan_path: PathBuf) -> Result<(), String> {
    let text = std::fs::read_to_string(&plan_path)
        .map_err(|e| format!("cannot read plan {}: {e}", plan_path.display()))?;
    let plan = plan::Plan::parse(&text).map_err(|e| format!("plan is not valid: {e}"))?;

    let venue = open_venue(cfg)?;
    let store = RunStore::new(&cfg.state_dir);
    let clock = SystemClock;
    let runner = Runner {
        venue: &venue,
        clock: &clock,
        store: &store,
        controls_path: cfg.controls_path(),
        schedule: cfg.schedule,
        max_plan_age_minutes: cfg.max_plan_age_minutes,
    };

    let outcome = runner.run(&plan).await;
    save_venue(cfg, &venue).await?;

    match outcome {
        Ok(record) => {
            println!("{}", serde_json::to_string_pretty(&record).unwrap());
            if record.is_clean() {
                Ok(())
            } else {
                // A partial run exits non-zero. Cron mails on failure, and a
                // half-applied plan is exactly what should reach a human.
                Err(record
                    .detail
                    .unwrap_or_else(|| "the run did not complete".into()))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

fn cmd_status(cfg: &BotConfig) -> Result<(), String> {
    let controls = ControlFile::read(&cfg.controls_path());
    let store = RunStore::new(&cfg.state_dir);
    let now = time::OffsetDateTime::now_utc();
    let health = runner::health(&controls, &store, cfg.cadence_hours, now)
        .map_err(|e| format!("cannot assess health: {e}"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "health": health,
            "controls": controls,
            "schedule": cfg.schedule,
        }))
        .unwrap()
    );
    // Exit code carries the answer too, so a monitoring script does not have to
    // parse anything to know something is wrong.
    if health.ok {
        Ok(())
    } else {
        Err(health.notes.join("; "))
    }
}

fn cmd_history(cfg: &BotConfig, limit: Option<usize>) -> Result<(), String> {
    let store = RunStore::new(&cfg.state_dir);
    let runs = store
        .recent(limit.unwrap_or(20))
        .map_err(|e| format!("cannot read run history: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&runs).unwrap());
    Ok(())
}

async fn cmd_positions(cfg: &BotConfig) -> Result<(), String> {
    let venue = open_venue(cfg)?;
    let positions = venue
        .get_positions()
        .await
        .map_err(|e| format!("cannot read positions: {e}"))?;
    let balances = venue
        .get_balances()
        .await
        .map_err(|e| format!("cannot read balances: {e}"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "positions": positions,
            "balances": balances,
        }))
        .unwrap()
    );
    Ok(())
}

fn cmd_control(
    cfg: &BotConfig,
    kill_switch: bool,
    paused: bool,
    flags: &Flags,
) -> Result<(), String> {
    // Both are required. A control change with no name and no reason is the
    // thing the next operator cannot act on.
    let reason = flags.need("--reason")?;
    let by = flags.need("--by")?;
    let control = ControlFile {
        kill_switch,
        paused,
        reason: Some(reason),
        set_by: Some(by),
        set_at: Some(
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        ),
    };
    control
        .write(&cfg.controls_path())
        .map_err(|e| format!("cannot write controls: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&control).unwrap());
    Ok(())
}

async fn cmd_flatten(cfg: &BotConfig, flags: &Flags) -> Result<(), String> {
    let reason = flags.need("--reason")?;
    let by = flags.need("--by")?;
    let venue = open_venue(cfg)?;
    let store = RunStore::new(&cfg.state_dir);
    let clock = SystemClock;
    let runner = Runner {
        venue: &venue,
        clock: &clock,
        store: &store,
        controls_path: cfg.controls_path(),
        schedule: cfg.schedule,
        max_plan_age_minutes: cfg.max_plan_age_minutes,
    };

    let outcome = runner.flatten(&reason, &by).await;
    save_venue(cfg, &venue).await?;

    let record = outcome.map_err(|e| format!("flatten failed: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&record).unwrap());
    if record.orders_skipped > 0 {
        return Err(record
            .detail
            .unwrap_or_else(|| "some positions are still open".into()));
    }
    Ok(())
}
