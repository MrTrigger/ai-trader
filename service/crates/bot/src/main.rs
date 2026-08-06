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

mod active_venue;
mod config;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use active_venue::{Active, Env, Mode};
use config::BotConfig;
use paper::PaperConfig;
use runner::{ControlFile, Ledger, RunStore, Runner};
use venue::{ManualPrices, PriceSource, SystemClock, VenueAdapter};

type Venue = Active;

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
  reconcile                          our record against the venue's; changes nothing
  feed [--once] [--interval SECS]    keep marks.json fresh from the venue
  mode                               which venue this is pointed at
  mode set <paper|live-readonly|live> --reason R --by WHO --confirm
  adopt --reason R --by WHO --confirm [--accept-unknown-fills]
                                     take pre-existing venue positions on as ours
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

    let config_path_for_write = config_path.clone();
    if let Some(p) = &config_path {
        let _ = CONFIG_PATH.set(p.clone());
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
        "reconcile" => block_on(cmd_reconcile(&cfg)),
        "feed" => block_on(cmd_feed(
            &cfg,
            flags.has("--once"),
            flags
                .get("--interval")
                .and_then(|s| s.parse().ok())
                .unwrap_or(30),
        )),
        "mode" => {
            let target = rest.get(1).filter(|a| *a == "set").and(rest.get(2));
            match target {
                None => block_on(cmd_mode_show(&cfg)),
                Some(m) => {
                    if !flags.has("--confirm") {
                        return Err(format!(
                            "switching to '{m}' changes which venue every future run trades \
                             against. Re-run with --confirm."
                        ));
                    }
                    let path = config_path_for_write.ok_or("--config is required")?;
                    block_on(cmd_mode_set(&cfg, &path, m, &flags))
                }
            }
        }
        "adopt" => {
            if !flags.has("--confirm") {
                return Err(
                    "adopt writes the venue's current positions into our own record as an \
                     opening baseline. That is the one repair this system will not do on its \
                     own, so look at the account first and re-run with --confirm."
                        .into(),
                );
            }
            block_on(cmd_adopt(&cfg, &flags))
        }
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
/// The config path this process was started with.
///
/// Stashed rather than threaded through every command: `.env` resolution needs
/// it, and adding the parameter to a dozen call sites to carry one immutable
/// startup fact reads worse than saying so here.
static CONFIG_PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn config_path() -> PathBuf {
    CONFIG_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."))
}

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

/// Load the venue for the configured mode, restoring whatever it knew last time.
///
/// The paper venue keeps its books in a file, so without the snapshot every
/// invocation would start flat and the executor would reconcile a real
/// intention against a book that had forgotten everything. The live venue keeps
/// its own books, so there is nothing to restore.
fn open_venue(cfg: &BotConfig) -> Result<Venue, String> {
    let env = Env::load(cfg.env_path(&config_path()).as_deref());

    // Marks. A live feed is the default because a paper run on frozen prices
    // measures nothing; the file feed stays for fixtures and offline work.
    let prices: Arc<dyn PriceSource> = match cfg.feed.as_str() {
        "hyperliquid" => env.price_source()?,
        _ => {
            let m = config::read_marks(&cfg.marks_path())?;
            let manual = Arc::new(ManualPrices::new());
            for (asset, price) in &m {
                manual.set(asset, *price);
            }
            manual
        }
    };

    let paper_cfg = PaperConfig {
        quote_currency: cfg.quote_currency.clone(),
        initial_cash: cfg.initial_cash,
        taker_fee_bps: cfg.taker_fee_bps,
        maker_fee_bps: cfg.maker_fee_bps,
        slippage_bps: cfg.slippage_bps,
        quote_decimals: 8,
    };

    // Markets from the config if it lists any, otherwise from the venue itself.
    // Listing them by hand is the safe default for paper; against a real venue
    // its own metadata is authoritative and a stale local copy is how an order
    // gets rejected for a lot size that changed last month.
    let markets = cfg.markets();

    let path = cfg.venue_state_path();
    let snapshot = match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };

    active_venue::open(
        cfg.mode,
        &env,
        paper_cfg,
        markets,
        prices,
        snapshot.as_deref(),
    )
}

/// Write the venue's books back out.
///
/// Only paper has books of ours to write. The live venue is its own record, and
/// writing a local copy of it would create a second opinion about the account.
async fn save_venue(cfg: &BotConfig, venue: &Venue) -> Result<(), String> {
    if !venue.is_paper() {
        return Ok(());
    }
    let Some(paper) = venue.as_paper() else {
        return Ok(());
    };
    let path = cfg.venue_state_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Write-then-rename, so a crash mid-write cannot leave a truncated book.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, paper.snapshot().await)
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
    let ledger = Ledger::open(&cfg.state_dir);
    let runner = Runner {
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
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

/// Report our record against the venue's. Places no orders, writes nothing.
///
/// The exit code carries the answer: zero when the two agree, non-zero when
/// they do not, so this can be a cron check as well as something a human runs
/// after an incident.
async fn cmd_reconcile(cfg: &BotConfig) -> Result<(), String> {
    let (venue, store, ledger) = (
        open_venue(cfg)?,
        RunStore::new(&cfg.state_dir),
        Ledger::open(&cfg.state_dir),
    );
    let clock = SystemClock;
    let runner = Runner {
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
        controls_path: cfg.controls_path(),
        schedule: cfg.schedule,
        max_plan_age_minutes: cfg.max_plan_age_minutes,
    };
    let report = runner
        .inspect()
        .await
        .map_err(|e| format!("cannot reconcile: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    if report.agrees {
        eprintln!("{}", report.explain());
        Ok(())
    } else {
        Err(report.explain())
    }
}

/// Take pre-existing venue positions on as our opening state.
async fn cmd_adopt(cfg: &BotConfig, flags: &Flags) -> Result<(), String> {
    let reason = flags.need("--reason")?;
    let by = flags.need("--by")?;
    let (venue, store, ledger) = (
        open_venue(cfg)?,
        RunStore::new(&cfg.state_dir),
        Ledger::open(&cfg.state_dir),
    );
    let clock = SystemClock;
    let runner = Runner {
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
        controls_path: cfg.controls_path(),
        schedule: cfg.schedule,
        max_plan_age_minutes: cfg.max_plan_age_minutes,
    };
    let record = runner
        .adopt(&reason, &by, flags.has("--accept-unknown-fills"))
        .await
        .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&record).unwrap());
    Ok(())
}

/// Keep `marks.json` current from the venue's public feed.
///
/// One writer, one file, everybody else reads it. The dashboard holds no
/// credentials and the paper venue can run offline from the same snapshot, so
/// a single process fetching prices is both the security boundary and the
/// simplest thing that works.
///
/// Write-then-rename on every tick: a reader must never catch a half-written
/// price file and value the book against three assets and a truncated number.
async fn cmd_feed(cfg: &BotConfig, once: bool, interval_s: u64) -> Result<(), String> {
    let env = Env::load(cfg.env_path(&config_path()).as_deref());
    let source = hyperliquid::Info::new(env.api_url.as_deref().unwrap_or(hyperliquid::MAINNET))
        .map_err(|e| e.to_string())?;

    let path = cfg.marks_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    if !once {
        eprintln!("feed: writing {} every {interval_s}s", path.display());
    }

    let mut consecutive_failures = 0u32;
    loop {
        match source.marks().await {
            Ok(marks) => {
                consecutive_failures = 0;
                let body: std::collections::BTreeMap<String, String> = marks
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_string()))
                    .collect();
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, serde_json::to_string_pretty(&body).unwrap() + "\n")
                    .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
                std::fs::rename(&tmp, &path)
                    .map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
                if once {
                    println!("{} marks written to {}", body.len(), path.display());
                    return Ok(());
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                // Never delete or zero the file on a failure. A stale price is
                // visibly stale and can be reasoned about; a missing one makes
                // the whole book unvaluable, which is worse.
                eprintln!("feed: {e} (failure {consecutive_failures})");
                if once {
                    return Err(e.to_string());
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_s.max(1))).await;
    }
}

/// What venue this is pointed at, and whether it could trade.
async fn cmd_mode_show(cfg: &BotConfig) -> Result<(), String> {
    let venue = open_venue(cfg)?;
    // Derived from the key whatever mode we are in, because "is the agent I
    // configured the one the account approved" is a question you want answered
    // *before* switching to live, not by a rejected order afterwards.
    let env = Env::load(cfg.env_path(&config_path()).as_deref());
    let configured_agent =
        env.agent_key
            .as_deref()
            .map(|k| match hyperliquid::Agent::from_hex(k) {
                Ok(a) => a.address().to_string(),
                Err(e) => format!("unusable: {e}"),
            });
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "mode": cfg.mode,
            "feed": cfg.feed,
            "venue": venue.describe(),
            "agent_address": venue.agent_address(),
            "configured_agent": configured_agent,
            "moves_real_money": cfg.mode.moves_real_money(),
        }))
        .unwrap()
    );
    Ok(())
}

/// Point the bot at a different venue.
///
/// Rewrites the mode in the config file and records the change as a run, so
/// "when did this start trading real money" has an answer in the same place
/// every other operational question does. The switch is refused unless the new
/// mode can actually be opened — discovering that a live key is missing at the
/// first order rather than at the switch is the failure this prevents.
async fn cmd_mode_set(
    cfg: &BotConfig,
    config_path: &std::path::Path,
    target: &str,
    flags: &Flags,
) -> Result<(), String> {
    let reason = flags.need("--reason")?;
    let by = flags.need("--by")?;
    let mode: Mode = target.parse()?;

    if mode == cfg.mode {
        return Err(format!("already in {} mode", mode.as_str()));
    }

    // Prove the new mode works before committing to it.
    let mut probe = cfg.clone();
    probe.mode = mode;
    let venue =
        open_venue(&probe).map_err(|e| format!("cannot switch to {}: {e}", mode.as_str()))?;
    let described = venue.describe();

    // Going live starts flat by construction: the ledger describes a paper book
    // that has nothing to do with the real account, and carrying it across
    // would have us reconcile one account's history against another's balance.
    if mode.moves_real_money() || mode == Mode::LiveReadonly {
        let ledger = Ledger::open(&cfg.state_dir);
        if !ledger.is_empty().map_err(|e| e.to_string())? {
            return Err(format!(
                "the order ledger already describes a {} book. Switching venues with it in \
                 place would reconcile one account's history against another's balance. Move \
                 {} aside first, then `bot adopt` against the real account.",
                cfg.mode.as_str(),
                ledger.path().display()
            ));
        }
    }

    let text = std::fs::read_to_string(config_path)
        .map_err(|e| format!("cannot read {}: {e}", config_path.display()))?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("cannot parse the config: {e}"))?;
    doc["mode"] = serde_json::json!(mode);
    let tmp = config_path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&doc).unwrap() + "\n")
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, config_path)
        .map_err(|e| format!("cannot replace {}: {e}", config_path.display()))?;

    let store = RunStore::new(&cfg.state_dir);
    let now = time::OffsetDateTime::now_utc();
    let controls = ControlFile::read(&cfg.controls_path());
    let stamp = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let record = runner::RunRecord {
        run_id: format!("mode-{stamp}"),
        plan_id: None,
        as_of: stamp.clone(),
        recorded_at: stamp,
        outcome: "mode-changed".into(),
        detail: Some(format!(
            "{by} switched from {} to {} ({described}). Reason: {reason}",
            cfg.mode.as_str(),
            mode.as_str()
        )),
        orders_planned: 0,
        orders_submitted: 0,
        orders_skipped: 0,
        slices_completed: 0,
        slices_planned: 0,
        nav: None,
        gross_exposure: None,
        net_exposure: None,
        control_state: controls.state().into(),
        risk_checks: Vec::new(),
        slices: Vec::new(),
    };
    store.record(&record).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&record).unwrap());
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
    let ledger = Ledger::open(&cfg.state_dir);
    let runner = Runner {
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
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
