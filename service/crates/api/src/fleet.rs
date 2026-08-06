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
    let reason = reason.to_string();
    let by = by.to_string();
    let bot_id = bot_id.to_string();
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
                Some(row) => serde_json::from_str::<Value>(&row.payload)
                    .unwrap_or_else(|_| json!({})),
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
            let state = if halt { "halted" } else { "running" };
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
            reg.set_control(&bot_id, state, &reason, &by, &payload.to_string())
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
    let (bot, status, control, fills, runs) = with_registry(move |reg| {
        Box::pin(async move {
            let bot = reg.bot(&id).await?;
            let status = reg.get_status(&id).await?;
            let control = reg.current_control(&id).await?;
            let fills = reg.recent_fills(&id, 500).await?;
            let runs = reg.recent_runs(&id, 20).await?;
            Ok((bot, status, control, fills, runs))
        })
    })?;
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
