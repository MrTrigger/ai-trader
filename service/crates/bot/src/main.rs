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
use runner::{ControlFile, ControlStore, Ledger, RunStore, Runner};
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
                                     close THIS BOT's open positions at market
  identity [show|migrate|register|enable|disable|remove <bot_id>]
                                     the DB identity registry (needs DATABASE_URL)
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

    // Before any gate reads the environment, not when the venue is opened.
    //
    // This was a fail-open: `require_registered` runs ahead of `open_venue`, so
    // with DATABASE_URL living only in `.env` it saw no registry, took the
    // dev-mode path, and let a DISABLED bot trade — the exact thing the
    // fail-closed gate exists to prevent. Credentials must be resolved before
    // the first decision that depends on them, not before the first order.
    let cfg_path = config_path_for_write
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    active_venue::Env::load(cfg.env_path(&cfg_path).as_deref());
    let command = rest.first().cloned().unwrap_or_default();
    let flags = Flags::parse(&rest[rest.len().min(1)..]);

    match command.as_str() {
        "run" => {
            let plan_path = flags.need("--plan")?;
            block_on(async {
                require_registered(&cfg).await?;
                cmd_run(&cfg, PathBuf::from(plan_path)).await
            })
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
        "identity" => block_on(cmd_identity(&cfg, &rest[rest.len().min(1)..])),
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

// --- operational stores -----------------------------------------------------

/// Where runs, the ledger, controls and the paper book live.
///
/// `DATABASE_URL` set → Postgres, the deployment contract: rows survive the
/// pod and are what the control plane reads. Not set → files under
/// `state_dir`, dev mode, announced loudly. There is deliberately no silent
/// fallback from DB to files: an unreachable DB is a refusal, not a
/// downgrade — a bot quietly writing files while the fleet reads the DB
/// would be invisible exactly when something is wrong.
struct Stores {
    rec: Option<Arc<records::blocking::Records>>,
    store: RunStore,
    ledger: Ledger,
    controls: ControlStore,
}

fn open_stores(cfg: &BotConfig) -> Result<Stores, String> {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            let rec = records::blocking::Records::connect(&url).map_err(|e| {
                format!(
                    "records db unreachable ({e}) — refusing to fall back to files while \
                     DATABASE_URL is set (fail closed)"
                )
            })?;
            Ok(Stores {
                store: RunStore::db(rec.clone(), &cfg.bot_id),
                ledger: Ledger::db(rec.clone(), &cfg.bot_id),
                controls: ControlStore::db(rec.clone(), &cfg.bot_id),
                rec: Some(rec),
            })
        }
        _ => {
            eprintln!(
                "bot {}: DATABASE_URL not set — file state under {} (dev mode). \
                 Deployment always sets it.",
                cfg.bot_id,
                cfg.state_dir.display()
            );
            Ok(Stores {
                rec: None,
                store: RunStore::new(&cfg.state_dir),
                ledger: Ledger::open(&cfg.state_dir),
                controls: ControlStore::File(cfg.controls_path()),
            })
        }
    }
}

// --- the venue --------------------------------------------------------------

/// Load the venue for the configured mode, restoring whatever it knew last time.
///
/// The paper venue keeps its books in a file, so without the snapshot every
/// invocation would start flat and the executor would reconcile a real
/// intention against a book that had forgotten everything. The live venue keeps
/// its own books, so there is nothing to restore.
fn open_venue(cfg: &BotConfig, rec: Option<&records::blocking::Records>) -> Result<Venue, String> {
    let env = Env::from_process();

    // Marks. A live feed is the default because a paper run on frozen prices
    // measures nothing; the file feed stays for fixtures and offline work.
    let prices: Arc<dyn PriceSource> = match cfg.feed.as_str() {
        // the configured venue's own feed ("hyperliquid" kept as a legacy alias)
        "venue" | "hyperliquid" => env.price_source()?,
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

    let snapshot = match rec {
        Some(rec) => rec
            .get_sim_state(&cfg.bot_id)
            .map_err(|e| format!("cannot read the paper book from the records db: {e}"))?,
        None => {
            let path = cfg.venue_state_path();
            match std::fs::read_to_string(&path) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            }
        }
    };

    active_venue::open(
        &cfg.venue_id,
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
async fn save_venue(
    cfg: &BotConfig,
    rec: Option<&records::blocking::Records>,
    venue: &Venue,
) -> Result<(), String> {
    if !venue.is_paper() {
        return Ok(());
    }
    let Some(paper) = venue.as_paper() else {
        return Ok(());
    };
    let snap = paper.snapshot().await;
    if let Some(rec) = rec {
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        return rec
            .put_sim_state(&cfg.bot_id, &now, &snap)
            .map_err(|e| format!("cannot write the paper book to the records db: {e}"));
    }
    let path = cfg.venue_state_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Write-then-rename, so a crash mid-write cannot leave a truncated book.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, snap).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

// --- commands ---------------------------------------------------------------

async fn cmd_run(cfg: &BotConfig, plan_path: PathBuf) -> Result<(), String> {
    let text = std::fs::read_to_string(&plan_path)
        .map_err(|e| format!("cannot read plan {}: {e}", plan_path.display()))?;
    let plan = plan::Plan::parse(&text).map_err(|e| format!("plan is not valid: {e}"))?;

    let Stores {
        rec,
        store,
        ledger,
        controls,
    } = open_stores(cfg)?;
    let venue = open_venue(cfg, rec.as_deref())?;
    let clock = SystemClock;
    let runner = Runner {
        bot_id: cfg.bot_id.clone(),
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
        controls,
        schedule: cfg.schedule,
        max_plan_age_minutes: cfg.max_plan_age_minutes,
    };

    let outcome = runner.run(&plan).await;
    save_venue(cfg, rec.as_deref(), &venue).await?;

    // Publish the status heartbeat the fleet dashboard renders. Best-effort:
    // the run's own record is already in the runs table, and a failed status
    // write must not turn a clean run into a reported failure.
    if let Some(rec) = rec.as_deref() {
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        // The canonical status envelope (schema 1) every bot publishes:
        // schema/kind/mode/state/headline uniformly, bot-specifics in detail.
        let doc = match &outcome {
            Ok(r) => serde_json::json!({
                "schema": 1,
                "kind": "runner",
                "mode": cfg.mode,
                "state": if r.control_state == "halted" { "halted" } else { "running" },
                "state_reason": if r.control_state == "halted" { Some(r.outcome.clone()) } else { None },
                "headline": { "nav": r.nav, "unit": cfg.quote_currency },
                "detail": {
                    "last_outcome": r.outcome,
                    "control_state": r.control_state,
                    "recorded_at": r.recorded_at,
                },
            }),
            Err(e) => serde_json::json!({
                "schema": 1,
                "kind": "runner",
                "mode": cfg.mode,
                "state": "halted",
                "state_reason": "run error",
                "headline": {},
                "detail": { "last_outcome": "error", "detail": e.to_string() },
            }),
        };
        if let Err(e) = rec.put_status(&cfg.bot_id, &now, &doc.to_string()) {
            eprintln!("bot {}: status write failed: {e}", cfg.bot_id);
        }
    }

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
    let st = open_stores(cfg)?;
    let controls = st.controls.read();
    let store = st.store;
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
    let store = open_stores(cfg)?.store;
    let runs = store
        .recent(limit.unwrap_or(20))
        .map_err(|e| format!("cannot read run history: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&runs).unwrap());
    Ok(())
}

async fn cmd_positions(cfg: &BotConfig) -> Result<(), String> {
    let st = open_stores(cfg)?;
    let venue = open_venue(cfg, st.rec.as_deref())?;
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
    let Stores {
        rec,
        store,
        ledger,
        controls,
    } = open_stores(cfg)?;
    let venue = open_venue(cfg, rec.as_deref())?;
    let clock = SystemClock;
    let runner = Runner {
        bot_id: cfg.bot_id.clone(),
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
        controls,
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
    let Stores {
        rec,
        store,
        ledger,
        controls,
    } = open_stores(cfg)?;
    let venue = open_venue(cfg, rec.as_deref())?;
    let clock = SystemClock;
    let runner = Runner {
        bot_id: cfg.bot_id.clone(),
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
        controls,
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
    let venue = open_venue(cfg, open_stores(cfg)?.rec.as_deref())?;
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
    // Ask the venue whether it will actually accept this agent. An unapproved
    // or expired one signs perfectly well and is rejected without explanation,
    // so the answer belongs here rather than in a failed order.
    let approval = match (&configured_agent, &env.account) {
        (Some(agent), Some(account)) if !agent.starts_with("unusable") => {
            let info =
                hyperliquid::Info::new(env.api_url.as_deref().unwrap_or(hyperliquid::MAINNET))
                    .map_err(|e| e.to_string())?;
            match info.agents(account).await {
                Ok(list) => {
                    let now = time::OffsetDateTime::now_utc();
                    match list.iter().find(|a| a.address.eq_ignore_ascii_case(agent)) {
                        Some(a) => serde_json::json!({
                            "approved": true,
                            "name": a.name,
                            "days_left": a.days_left(now),
                            "expires": a.expires_at().map(|e| e
                                .format(&time::format_description::well_known::Rfc3339)
                                .unwrap_or_default()),
                        }),
                        None => serde_json::json!({
                            "approved": false,
                            "detail": format!(
                                "the account has not approved {agent}. Generating an API wallet \
                                 is not enough - the approval is a separate signed action in the \
                                 Hyperliquid UI. Approved right now: {}.",
                                if list.is_empty() {
                                    "nothing".to_string()
                                } else {
                                    list.iter()
                                        .map(|a| format!("{} ({})", a.name, a.address))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                }
                            ),
                        }),
                    }
                }
                Err(e) => serde_json::json!({"approved": null, "detail": e.to_string()}),
            }
        }
        _ => serde_json::Value::Null,
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "mode": cfg.mode,
            "feed": cfg.feed,
            "venue": venue.describe(),
            "agent_address": venue.agent_address(),
            "configured_agent": configured_agent,
            "agent_name": env.agent_name,
            "agent_declared": env.agent_address,
            "agent_approval": approval,
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
    let stores = open_stores(cfg)?;
    let venue = open_venue(&probe, stores.rec.as_deref())
        .map_err(|e| format!("cannot switch to {}: {e}", mode.as_str()))?;
    let described = venue.describe();

    // Going live starts flat by construction: the ledger describes a paper book
    // that has nothing to do with the real account, and carrying it across
    // would have us reconcile one account's history against another's balance.
    if mode.moves_real_money() || mode == Mode::LiveReadonly {
        let ledger = &stores.ledger;
        if !ledger.is_empty().map_err(|e| e.to_string())? {
            return Err(format!(
                "the order ledger already describes a {} book. Switching venues with it in \
                 place would reconcile one account's history against another's balance. Move \
                 {} aside first, then `bot adopt` against the real account.",
                cfg.mode.as_str(),
                ledger.describe()
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

    let store = stores.store;
    let now = time::OffsetDateTime::now_utc();
    let controls = stores.controls.read();
    let stamp = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let record = runner::RunRecord {
        bot_id: Some(cfg.bot_id.clone()),
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
        bot_id: Some(cfg.bot_id.clone()),
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
    open_stores(cfg)?
        .controls
        .write(&control)
        .map_err(|e| format!("cannot write controls: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&control).unwrap());
    Ok(())
}

async fn cmd_flatten(cfg: &BotConfig, flags: &Flags) -> Result<(), String> {
    let reason = flags.need("--reason")?;
    let by = flags.need("--by")?;
    let Stores {
        rec,
        store,
        ledger,
        controls,
    } = open_stores(cfg)?;
    let venue = open_venue(cfg, rec.as_deref())?;
    let clock = SystemClock;
    let runner = Runner {
        bot_id: cfg.bot_id.clone(),
        venue: &venue,
        clock: &clock,
        store: &store,
        ledger: &ledger,
        controls,
        schedule: cfg.schedule,
        max_plan_age_minutes: cfg.max_plan_age_minutes,
    };

    let outcome = runner.flatten(&reason, &by).await;
    save_venue(cfg, rec.as_deref(), &venue).await?;

    let record = outcome.map_err(|e| format!("flatten failed: {e}"))?;
    println!("{}", serde_json::to_string_pretty(&record).unwrap());
    if record.orders_skipped > 0 {
        return Err(record
            .detail
            .unwrap_or_else(|| "some positions are still open".into()));
    }
    Ok(())
}

/// Identity is DB-first (triggerlab mandate): when DATABASE_URL is set, an
/// unregistered or disabled bot_id refuses to run — fail closed. Without a
/// DATABASE_URL this is dev mode, allowed but announced.
async fn require_registered(cfg: &BotConfig) -> Result<(), String> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!(
            "bot {}: DATABASE_URL not set — running WITHOUT the identity \
             registry (dev mode). Prod always sets it.",
            cfg.bot_id
        );
        return Ok(());
    };
    let reg = records::Registry::connect(&url)
        .await
        .map_err(|e| format!("identity registry unreachable ({e}) — fail closed"))?;
    reg.require_enabled(&cfg.bot_id)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn cmd_identity(cfg: &BotConfig, rest: &[String]) -> Result<(), String> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| "identity commands need DATABASE_URL".to_string())?;
    let reg = records::Registry::connect(&url)
        .await
        .map_err(|e| e.to_string())?;
    let sub = rest.first().map(String::as_str).unwrap_or("show");
    match sub {
        "migrate" => {
            reg.migrate().await.map_err(|e| e.to_string())?;
            println!("identity schema migrated");
        }
        "register" => {
            reg.register_bot(
                &cfg.bot_id,
                &cfg.bot_id,
                "cron",
                "crypto",
                "planner (pipeline.run)",
            )
            .await
            .map_err(|e| e.to_string())?;
            println!("registered {} (disabled — enable deliberately)", cfg.bot_id);
        }
        "enable" => {
            reg.set_enabled(&cfg.bot_id, true)
                .await
                .map_err(|e| e.to_string())?;
            println!("{} enabled", cfg.bot_id);
        }
        "disable" => {
            reg.set_enabled(&cfg.bot_id, false)
                .await
                .map_err(|e| e.to_string())?;
            println!("{} disabled", cfg.bot_id);
        }
        // Deregistering is by explicit id, never by "whatever this config says".
        // Every other subcommand acts on cfg.bot_id, and the one that destroys
        // an identity should not be reachable by pointing at the wrong config.
        "remove" => {
            let target = rest.get(1).ok_or_else(|| {
                "identity remove needs the bot id spelled out: \
                 `bot --config <file> identity remove <bot_id>`"
                    .to_string()
            })?;
            let held = reg.record_counts(target).await.map_err(|e| e.to_string())?;
            if !held.is_empty() {
                println!("{target} owns:");
                for (t, n) in &held {
                    println!("  {n:>6} in {t}");
                }
            }
            reg.remove_bot(target).await.map_err(|e| e.to_string())?;
            println!("{target} deregistered");
        }
        // "show", and anything unrecognised, lists rather than acting.
        _ => {
            for b in reg.list_bots().await.map_err(|e| e.to_string())? {
                let marker = if b.bot_id == cfg.bot_id { "*" } else { " " };
                println!(
                    "{marker} {:<24} {:<10} {:<8} enabled={} ({})",
                    b.bot_id, b.cadence, b.asset_class, b.enabled, b.decision_core
                );
            }
        }
    }
    Ok(())
}
