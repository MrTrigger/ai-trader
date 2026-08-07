//! The fleet view: every registered bot, with live status (multi-bot UI,
//! migration step: "one control plane").
//!
//! Identity AND operational records come from Postgres (steps 1 and 4):
//! `bot_status` for the live document, `fills`/`runs` for history,
//! `control_events` for the control word. Every bot publishes to the same
//! tables whatever language it is written in, so this module renders one
//! contract, not per-bot dialects. Reading a bot's `state_dir` files
//! remains only as a dev fallback for bots that have not yet written a
//! status row — a pod has no files at all.
//!
//! Controls: HALT/RESUME appends a `control_events` row (reason + name
//! required, merged over the current payload so per-sleeve switches
//! survive). Bots read the newest row at their own gates; absent rows mean
//! halted. No credentials are involved anywhere in this process.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Blocking bridge over the async registry: the dashboard is a localhost
/// lens, one short-lived runtime per request is fine and keeps this crate
/// synchronous.
fn with_registry<T>(
    f: impl FnOnce(
        &records::Registry,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, records::RecordsError>> + '_>,
    >,
) -> Result<T, String> {
    let url = std::env::var("DATABASE_URL").map_err(|_| {
        "DATABASE_URL is not set — the fleet view needs the identity registry".to_string()
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let reg = records::Registry::connect(&url)
            .await
            .map_err(|e| e.to_string())?;
        f(&reg).await.map_err(|e| e.to_string())
    })
}

fn heartbeat_age_seconds(iso: &str) -> Option<i64> {
    let ts =
        time::OffsetDateTime::parse(iso, &time::format_description::well_known::Rfc3339).ok()?;
    Some((time::OffsetDateTime::now_utc() - ts).whole_seconds())
}

/// Best-effort live status for one bot from its state_dir.
fn status_for(repo_root: &Path, state_dir: Option<&str>) -> Value {
    let Some(dir) = state_dir else {
        return json!({ "contract": "none", "note": "no state_dir registered" });
    };
    let dir = repo_root.join(dir);
    let botstate = dir.join("botstate/state.json");
    if let Ok(text) = std::fs::read_to_string(&botstate) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let age = v
                .get("heartbeat_utc")
                .and_then(|h| h.as_str())
                .and_then(heartbeat_age_seconds);
            let headline = v.get("headline").cloned().unwrap_or(Value::Null);
            return json!({
                "contract": dialect_of(&v),
                "mode": v.get("mode"),
                "halted": halted_of(&v),
                "net_total": headline.get("net").or_else(|| v.get("net_total")),
                "trades_total": headline.get("fills").or_else(|| v.get("trades_total")),
                "heartbeat_age_seconds": age,
            });
        }
    }
    // runner contract: controls + newest run record
    let controls = dir.join("controls.json");
    let mut out = json!({ "contract": "runner" });
    if let Ok(text) = std::fs::read_to_string(&controls) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            out["kill_switch"] = v.get("kill_switch").cloned().unwrap_or(Value::Null);
            out["paused"] = v.get("paused").cloned().unwrap_or(Value::Null);
        }
    }
    let runs = dir.join("runs");
    if let Ok(entries) = std::fs::read_dir(&runs) {
        let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        names.sort();
        if let Some(last) = names.last() {
            if let Ok(text) = std::fs::read_to_string(last) {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    out["last_run"] = json!({
                        "outcome": v.get("outcome"),
                        "recorded_at": v.get("recorded_at"),
                    });
                }
            }
        }
    }
    out
}

/// Which detail view a published status document gets. Canonical envelopes
/// (schema 1) say so via `kind`; legacy documents are sniffed by shape.
fn dialect_of(payload: &Value) -> &'static str {
    if payload.get("schema").is_some() {
        return match payload.get("kind").and_then(|k| k.as_str()) {
            Some("futures-book") => "botstate",
            _ => "runner",
        };
    }
    if payload.get("sleeves").is_some() {
        "botstate"
    } else {
        "runner"
    }
}

/// Whether a status document reports itself halted, and why.
fn halted_of(doc: &Value) -> Value {
    if doc.get("schema").is_some() {
        if doc.get("state").and_then(|s| s.as_str()) == Some("halted") {
            return doc.get("state_reason").cloned().unwrap_or(json!(true));
        }
        return Value::Null;
    }
    doc.get("halted").cloned().unwrap_or(Value::Null)
}

/// Status summary from a bot's DB rows, or `None` if it never published one.
fn status_from_rows(
    status: Option<&records::StatusRow>,
    control: Option<&records::ControlRow>,
    last_run: Option<&Value>,
) -> Option<Value> {
    let s = status?;
    let doc: Value = serde_json::from_str(&s.payload).unwrap_or(Value::Null);
    let mut out = json!({
        "contract": dialect_of(&doc),
        "source": "db",
        "heartbeat_age_seconds": s.heartbeat_age_seconds,
        "mode": doc.get("mode"),
        "halted": halted_of(&doc),
    });
    if let Some(f) = doc.pointer("/detail/feed") {
        out["feed"] = f.clone();
    }
    if let Some(h) = doc.get("headline") {
        out["net_total"] = h.get("net").cloned().unwrap_or(Value::Null);
        out["trades_total"] = h.get("fills").cloned().unwrap_or(Value::Null);
        out["headline"] = h.clone();
    }
    if let Some(c) = control {
        let cf: Value = serde_json::from_str(&c.payload).unwrap_or(Value::Null);
        let halted = if cf.get("schema").is_some() {
            json!(cf.get("state").and_then(|s| s.as_str()) != Some("running"))
        } else {
            cf.get("kill_switch").cloned().unwrap_or(Value::Null)
        };
        out["kill_switch"] = halted;
        out["control_state"] = json!(c.state);
    }
    if let Some(r) = last_run {
        out["last_run"] = json!({
            "outcome": r.get("outcome"),
            "recorded_at": r.get("recorded_at"),
        });
    }
    Some(out)
}

/// GET /api/bots
pub fn list(repo_root: &Path, local_state_dir: &Path) -> Result<Value, String> {
    let data = with_registry(|reg| {
        Box::pin(async move {
            let bots = reg.list_bots().await?;
            let mut rows = Vec::new();
            for b in bots {
                let status = reg.get_status(&b.bot_id).await?;
                let control = reg.current_control(&b.bot_id).await?;
                let last_run = reg
                    .recent_runs(&b.bot_id, 1)
                    .await?
                    .into_iter()
                    .next()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok());
                rows.push((b, status, control, last_run));
            }
            Ok(rows)
        })
    })?;
    let local = local_state_dir.to_string_lossy();
    let rows: Vec<Value> = data
        .into_iter()
        .map(|(b, status, control, last_run)| {
            let status = status_from_rows(status.as_ref(), control.as_ref(), last_run.as_ref())
                .unwrap_or_else(|| status_for(repo_root, b.state_dir.as_deref()));
            let is_local = b
                .state_dir
                .as_deref()
                .map(|d| local.ends_with(d) || local.contains(d))
                .unwrap_or(false);
            json!({
                "local": is_local,
                "bot_id": b.bot_id,
                "display_name": b.display_name,
                "cadence": b.cadence,
                "asset_class": b.asset_class,
                "decision_core": b.decision_core,
                "enabled": b.enabled,
                "state_dir": b.state_dir,
                "status": status,
            })
        })
        .collect();
    Ok(json!({ "available": true, "bots": rows }))
}

/// POST /api/bots/{id}/halt|resume — one control path for every bot.
///
/// Appends a `control_events` row whose payload merges over the current
/// one, so per-sleeve switches and other bot-specific keys survive a
/// halt/resume cycle. Both dialects are written (`halt` for the futures
/// contract, `kill_switch` for the runner's ControlFile) — each bot reads
/// its own key from the same row.
pub fn set_halt(
    _repo_root: &Path,
    bot_id: &str,
    halt: bool,
    reason: &str,
    by: &str,
) -> Result<Value, String> {
    set_state(bot_id, if halt { "halted" } else { "running" }, reason, by)
}

/// Write a control state. Three of them, because "stop the bot" is two
/// different instructions: `halted` opens nothing new and leaves the book
/// alone; `stopped` also closes it.
pub fn set_state(bot_id: &str, state: &str, reason: &str, by: &str) -> Result<Value, String> {
    // Only these three, spelled out: a typo must not become a control word
    // the bots then read as "not running" and obey forever.
    if !matches!(state, "running" | "halted" | "stopped") {
        return Err(format!(
            "unknown control state {state:?} — running, halted or stopped"
        ));
    }
    let reason = reason.to_string();
    let by = by.to_string();
    let bot_id = bot_id.to_string();
    let state = state.to_string();
    with_registry(move |reg| {
        Box::pin(async move {
            if reg.bot(&bot_id).await?.is_none() {
                // Same failure shape as RecordsError::UnknownBot, but we
                // want the check before writing a control row for a typo.
                return Err(records::RecordsError::UnknownBot(bot_id.clone()));
            }
            // Overrides survive the halt/resume cycle. A legacy payload's
            // bot-specific keys are lifted into `overrides` once, here.
            let current = match reg.current_control(&bot_id).await? {
                Some(row) => {
                    serde_json::from_str::<Value>(&row.payload).unwrap_or_else(|_| json!({}))
                }
                None => json!({}),
            };
            let overrides = if current.get("schema").is_some() {
                current.get("overrides").cloned().unwrap_or(Value::Null)
            } else {
                let lifted: serde_json::Map<String, Value> =
                    ["sleeves", "instrument", "sizing", "account"]
                        .iter()
                        .filter_map(|k| current.get(*k).map(|v| (k.to_string(), v.clone())))
                        .collect();
                if lifted.is_empty() {
                    Value::Null
                } else {
                    Value::Object(lifted)
                }
            };
            let now = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            let mut payload = json!({
                "schema": 1,
                "state": state,
                "reason": reason,
                "set_by": by,
                "set_at": now,
                "bot_id": bot_id,
            });
            if !overrides.is_null() {
                payload["overrides"] = overrides;
            }
            reg.set_control(&bot_id, &state, &reason, &by, &payload.to_string())
                .await?;
            Ok(payload)
        })
    })
}

/// GET /api/bots/{id}/state — the per-bot detail document the dashboard's
/// bot view renders. Served from the records DB; a bot that has never
/// published a status row falls back to its state_dir files (dev only).
pub fn detail(repo_root: &Path, bot_id: &str) -> Result<Value, String> {
    let id = bot_id.to_string();
    let (bot, status, control, fills, runs, bindings, accounts) = with_registry(move |reg| {
        Box::pin(async move {
            let bot = reg.bot(&id).await?;
            let status = reg.get_status(&id).await?;
            let control = reg.current_control(&id).await?;
            let fills = reg.recent_fills(&id, 500).await?;
            let runs = reg.recent_runs(&id, 20).await?;
            let bindings = reg.bindings(&id).await?;
            let accounts = reg.list_accounts().await?;
            Ok((bot, status, control, fills, runs, bindings, accounts))
        })
    })?;
    // Which broker this bot trades through, and what else it could. The
    // selection lives in the registry, so it survives restarts and the UI
    // is only ever a view of it.
    let trade = bindings.iter().find(|b| b.scope == "trade");
    let asset_class = bot
        .as_ref()
        .map(|b| b.asset_class.clone())
        .unwrap_or_default();
    let broker = json!({
        "account_id": trade.map(|b| b.account_id.clone()),
        "venue_id": trade.map(|b| b.venue_id.clone()),
        "protocol": trade.map(|b| b.protocol.clone()),
        "kind": trade.map(|b| b.account_kind.clone()),
        "credential_ref": trade.map(|b| b.credential_ref.clone()),
        // Only brokers that can trade what this bot trades. Offering a
        // futures book a crypto venue is not a harmless extra row: it is a
        // one-click way to bind a bot to something that cannot fill it.
        "options": accounts
            .iter()
            .filter(|a| a.asset_classes.iter().any(|c| c == &asset_class))
            .map(|a| json!({
                "account_id": a.account_id,
                "venue_id": a.venue_id,
                "protocol": a.protocol,
                "kind": a.kind,
                "credential_ref": a.credential_ref,
            }))
            .collect::<Vec<_>>(),
    });
    let bot = bot.ok_or_else(|| format!("unknown bot {bot_id:?}"))?;

    if let Some(srow) = status {
        let state: Value = serde_json::from_str(&srow.payload).unwrap_or(Value::Null);
        if dialect_of(&state) == "botstate" {
            // Chronological, like the journal file the shape came from.
            let fills: Vec<Value> = fills
                .into_iter()
                .rev()
                .filter_map(|t| serde_json::from_str(&t).ok())
                .collect();
            return Ok(json!({
                "contract": "botstate",
                "source": "db",
                "display_name": bot.display_name,
                "cadence": bot.cadence,
                "asset_class": bot.asset_class,
                "decision_core": bot.decision_core,
                "enabled": bot.enabled,
                "heartbeat_age_seconds": srow.heartbeat_age_seconds,
                "broker": broker,
                "state": state,
                "fills": fills,
            }));
        }
        let controls: Value = control
            .map(|c| serde_json::from_str(&c.payload).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        let runs: Vec<Value> = runs
            .into_iter()
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        return Ok(json!({
            "contract": "runner",
            "source": "db",
            "display_name": bot.display_name,
            "cadence": bot.cadence,
            "asset_class": bot.asset_class,
            "enabled": bot.enabled,
            "heartbeat_age_seconds": srow.heartbeat_age_seconds,
            "broker": broker,
            "controls": controls,
            "runs": runs,
        }));
    }

    // Dev fallback: no status row yet — read the registered state_dir.
    let Some(dir) = bot.state_dir.as_deref() else {
        return Ok(json!({ "contract": "none" }));
    };
    let dir = repo_root.join(dir);
    let botstate = dir.join("botstate");
    if botstate.join("state.json").exists() {
        let state: Value = std::fs::read_to_string(botstate.join("state.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let fills: Vec<Value> = std::fs::read_to_string(botstate.join("journal.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let tail = fills.len().saturating_sub(500);
        return Ok(json!({
            "contract": "botstate",
            "display_name": bot.display_name,
            "cadence": bot.cadence,
            "asset_class": bot.asset_class,
            "decision_core": bot.decision_core,
            "enabled": bot.enabled,
            "state": state,
            "fills": fills[tail..],
        }));
    }
    // runner contract: controls + recent runs
    let controls: Value = std::fs::read_to_string(dir.join("controls.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let mut runs: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("runs")) {
        let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        names.sort();
        for path in names.iter().rev().take(20) {
            if let Some(v) = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
            {
                runs.push(v);
            }
        }
    }
    Ok(json!({
        "contract": "runner",
        "display_name": bot.display_name,
        "cadence": bot.cadence,
        "asset_class": bot.asset_class,
        "enabled": bot.enabled,
        "controls": controls,
        "runs": runs,
    }))
}

/// GET /api/fleet/overview — everything the fleet landing page draws, in one
/// round trip: per-bot equity/P&L series, a merged action feed, and the same
/// identity rows `list` returns.
///
/// Series come from wherever a bot actually records history today — DB runs
/// when they exist, `runs/*.json` files for runner bots that predate step 4,
/// the status document's daily nets for the futures book. Each series says
/// which *kind* of number it is (`nav` or `pnl`) so the page can baseline NAVs
/// to zero before combining; adding a NAV to a P&L would manufacture money.
pub fn overview(repo_root: &Path, local_state_dir: &Path) -> Result<Value, String> {
    let data = with_registry(|reg| {
        Box::pin(async move {
            let bots = reg.list_bots().await?;
            let mut rows = Vec::new();
            for b in bots {
                let status = reg.get_status(&b.bot_id).await?;
                let control = reg.current_control(&b.bot_id).await?;
                let runs: Vec<Value> = reg
                    .recent_runs(&b.bot_id, 500)
                    .await?
                    .into_iter()
                    .filter_map(|t| serde_json::from_str(&t).ok())
                    .collect();
                let fills: Vec<Value> = reg
                    .recent_fills(&b.bot_id, 500)
                    .await?
                    .into_iter()
                    .filter_map(|t| serde_json::from_str(&t).ok())
                    .collect();
                let binding = reg
                    .bindings(&b.bot_id)
                    .await?
                    .into_iter()
                    .find(|x| x.scope == "trade");
                rows.push((b, status, control, runs, fills, binding));
            }
            Ok(rows)
        })
    })?;

    let local = local_state_dir.to_string_lossy();
    let mut bots_out = Vec::new();
    let mut feed: Vec<Value> = Vec::new();

    for (b, status, control, db_runs, db_fills, binding) in data {
        // Runs: DB first, files as the pre-step-4 fallback.
        let mut runs = db_runs;
        if runs.is_empty() {
            if let Some(dir) = b.state_dir.as_deref() {
                runs = read_run_files(&repo_root.join(dir));
            }
        }

        let status_doc: Option<Value> = status
            .as_ref()
            .and_then(|s| serde_json::from_str(&s.payload).ok());
        let dialect = status_doc.as_ref().map(dialect_of).unwrap_or("runner");

        // The series, and what its numbers mean.
        let (series, series_kind) = if dialect == "botstate" {
            (botstate_series(status_doc.as_ref(), &db_fills), "pnl")
        } else {
            let pts: Vec<Value> = runs
                .iter()
                .rev() // files/DB arrive newest-first; a curve reads oldest-first
                .filter_map(|r| {
                    let nav = r.get("nav")?.as_str()?.parse::<f64>().ok()?;
                    let at = r.get("recorded_at")?.as_str()?;
                    Some(json!([at, nav]))
                })
                .collect();
            (pts, "nav")
        };

        // Feed: run outcomes, futures fills, and the newest control word.
        for r in runs.iter().take(12) {
            feed.push(json!({
                "at": r.get("recorded_at"),
                "bot_id": b.bot_id,
                "kind": r.get("outcome"),
                "text": r.get("detail").and_then(|d| d.as_str())
                    .map(|d| d.chars().take(160).collect::<String>())
                    .unwrap_or_else(|| format!(
                        "{} · {} order(s)",
                        r.get("outcome").and_then(|o| o.as_str()).unwrap_or("run"),
                        r.get("orders_submitted").and_then(|o| o.as_u64()).unwrap_or(0)
                    )),
            }));
        }
        for f in db_fills.iter().take(12) {
            feed.push(json!({
                "at": f.get("at").or_else(|| f.get("ts")).or_else(|| f.get("time")),
                "bot_id": b.bot_id,
                "kind": "fill",
                "text": format!(
                    "filled {} {} @ {}",
                    f.get("side").and_then(|s| s.as_str()).unwrap_or(""),
                    f.get("qty").map(|q| q.to_string()).unwrap_or_default(),
                    f.get("price").map(|p| p.to_string()).unwrap_or_default()
                ),
            }));
        }
        if let Some(c) = &control {
            feed.push(json!({
                "at": c.at,
                "bot_id": b.bot_id,
                "kind": c.state,
                "text": format!("{} by {} · {}", c.state, c.set_by, c.reason),
            }));
        }

        let st = status_from_rows(status.as_ref(), control.as_ref(), runs.first())
            .unwrap_or_else(|| status_for(repo_root, b.state_dir.as_deref()));
        let is_local = b
            .state_dir
            .as_deref()
            .map(|d| local.ends_with(d) || local.contains(d))
            .unwrap_or(false);
        bots_out.push(json!({
            "local": is_local,
            "bot_id": b.bot_id,
            "display_name": b.display_name,
            "cadence": b.cadence,
            "asset_class": b.asset_class,
            "decision_core": b.decision_core,
            "enabled": b.enabled,
            "status": st,
            "broker": json!({
                "venue_id": binding.as_ref().map(|x| x.venue_id.clone()),
                "protocol": binding.as_ref().map(|x| x.protocol.clone()),
                "account_id": binding.as_ref().map(|x| x.account_id.clone()),
                "kind": binding.as_ref().map(|x| x.account_kind.clone()),
            }),
            "series": series,
            "series_kind": series_kind,
        }));
    }

    // Newest first, capped: this is a glance, not an archive.
    feed.sort_by(|a, b| {
        b.get("at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(a.get("at").and_then(|v| v.as_str()).unwrap_or(""))
    });
    feed.truncate(40);

    Ok(json!({ "available": true, "bots": bots_out, "feed": feed }))
}

/// Run records from a runner bot's `runs/` directory, newest first.
fn read_run_files(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir.join("runs")) {
        let mut names: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        names.sort();
        for path in names.iter().rev() {
            if let Some(v) = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
            {
                out.push(v);
            }
        }
    }
    out
}

/// Cumulative P&L for a futures-book bot: its daily nets if the status
/// document carries them, else a cumsum over recorded fills' nets.
fn botstate_series(doc: Option<&Value>, fills: &[Value]) -> Vec<Value> {
    if let Some(daily) = doc
        .and_then(|d| d.get("detail"))
        .and_then(|d| d.get("recent_daily_net"))
        .and_then(|d| d.as_array())
    {
        let mut cum = 0.0;
        let mut out = Vec::new();
        for item in daily {
            // Either [session, net] pairs or bare numbers, depending on age.
            let (label, net) = match item {
                Value::Array(pair) if pair.len() == 2 => (
                    pair[0].as_str().unwrap_or("").to_string(),
                    pair[1].as_f64().unwrap_or(0.0),
                ),
                Value::Number(n) => (String::new(), n.as_f64().unwrap_or(0.0)),
                _ => continue,
            };
            cum += net;
            out.push(json!([label, cum]));
        }
        if !out.is_empty() {
            return out;
        }
    }
    let mut cum = 0.0;
    let mut out = Vec::new();
    for f in fills.iter().rev() {
        let net = f.get("net").and_then(|n| n.as_f64()).or_else(|| {
            f.get("net")
                .and_then(|n| n.as_str())
                .and_then(|s| s.parse().ok())
        });
        let Some(net) = net else { continue };
        cum += net;
        out.push(json!([
            f.get("at")
                .or_else(|| f.get("ts"))
                .cloned()
                .unwrap_or(Value::Null),
            cum
        ]));
    }
    out
}

/// GET /api/bots/{id}/logs — what the bot said, newest first.
pub fn logs(bot_id: &str, limit: i64) -> Result<Value, String> {
    let id = bot_id.to_string();
    let rows = with_registry(move |reg| Box::pin(async move { reg.recent_log(&id, limit).await }))?;
    Ok(json!({
        "lines": rows
            .into_iter()
            .map(|(at, level, line)| json!({ "at": at, "level": level, "line": line }))
            .collect::<Vec<_>>(),
    }))
}

/// POST /api/bots/{id}/venue — point the bot's trade scope at another
/// broker account.
///
/// The selection is registry state, so it persists across restarts and is
/// what the bot reads when it next starts. It deliberately does NOT reach
/// into a running process: a bot that switched venue mid-session would
/// hold a position on one broker and reconcile against another. The switch
/// is recorded as a run row — same place `mode set` records itself — so
/// "when did this start trading through AMP" has an answer.
pub fn set_venue(bot_id: &str, account_id: &str, reason: &str, by: &str) -> Result<Value, String> {
    let (bot_id, account_id) = (bot_id.to_string(), account_id.to_string());
    let (reason, by) = (reason.to_string(), by.to_string());
    with_registry(move |reg| {
        Box::pin(async move {
            if reg.bot(&bot_id).await?.is_none() {
                return Err(records::RecordsError::UnknownBot(bot_id.clone()));
            }
            let accounts = reg.list_accounts().await?;
            let Some(account) = accounts.iter().find(|a| a.account_id == account_id) else {
                // Not a registry error variant, but the same fail-closed
                // spirit: an unknown account must never become a binding.
                return Err(records::RecordsError::UnknownBot(format!(
                    "account {account_id:?} is not registered"
                )));
            };
            let previous = reg
                .bindings(&bot_id)
                .await?
                .into_iter()
                .find(|b| b.scope == "trade")
                .map(|b| format!("{} ({})", b.account_id, b.venue_id));
            reg.set_trade_binding(&bot_id, &account_id).await?;

            let now = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
            let record = json!({
                "bot_id": bot_id,
                "run_id": format!("venue-{now}"),
                "recorded_at": now,
                "as_of": now,
                "outcome": "venue-changed",
                "detail": format!(
                    "{by} pointed the trade binding at {} ({}, {}) from {}. Reason: {reason}.                      Takes effect when the bot next starts.",
                    account.account_id,
                    account.venue_id,
                    account.kind,
                    previous.as_deref().unwrap_or("nothing"),
                ),
                "orders_planned": 0, "orders_submitted": 0, "orders_skipped": 0,
                "slices_completed": 0, "slices_planned": 0,
                "control_state": "unchanged",
                "risk_checks": [], "slices": [],
            });
            reg.record_run(
                &bot_id,
                record["run_id"].as_str().expect("set above"),
                &now,
                "venue-changed",
                &record.to_string(),
            )
            .await?;
            Ok(json!({
                "bot_id": bot_id,
                "account_id": account.account_id,
                "venue_id": account.venue_id,
                "kind": account.kind,
                "previous": previous,
                "note": "takes effect when the bot next starts",
            }))
        })
    })
}
