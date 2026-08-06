//! The fleet view: every registered bot, with live status (multi-bot UI,
//! migration step: "one control plane").
//!
//! Identity comes from the DB registry (DB-first, triggerlab mandate); the
//! per-bot status is read best-effort from each bot's registered state_dir.
//! Two state contracts exist until step 4 unifies records in Postgres:
//!
//! * `botstate/state.json` — the futures bot's heartbeat document.
//! * `controls.json` + `runs/*.json` — the crypto runner's files.
//!
//! Controls here are the narrow kind: HALT/RESUME for bots speaking the
//! botstate contract is a merge into their polled control.json — the same
//! file the bot's own rails read, no credentials involved. Runner-contract
//! bots keep their controls on their own credentialed `bot` binary (this
//! process only delegates for THE bot it wraps).

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
            return json!({
                "contract": "botstate",
                "mode": v.get("mode"),
                "halted": v.get("halted"),
                "net_total": v.get("net_total"),
                "trades_total": v.get("trades_total"),
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

/// GET /api/bots
pub fn list(repo_root: &Path, local_state_dir: &Path) -> Result<Value, String> {
    let bots = with_registry(|reg| Box::pin(reg.list_bots()))?;
    let local = local_state_dir.to_string_lossy();
    let rows: Vec<Value> = bots
        .into_iter()
        .map(|b| {
            let status = status_for(repo_root, b.state_dir.as_deref());
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

/// POST /api/bots/{id}/halt|resume — botstate-contract bots only.
pub fn set_halt(
    repo_root: &Path,
    bot_id: &str,
    halt: bool,
    reason: &str,
    by: &str,
) -> Result<Value, String> {
    let bot = with_registry(|reg| Box::pin(reg.bot(bot_id.to_string().leak())))?
        .ok_or_else(|| format!("unknown bot {bot_id:?}"))?;
    let Some(dir) = bot.state_dir.as_deref() else {
        return Err(format!("{bot_id} has no state_dir registered"));
    };
    let botstate = repo_root.join(dir).join("botstate");
    if !botstate.join("state.json").exists() {
        return Err(format!(
            "{bot_id} does not speak the botstate contract — use its own \
             credentialed `bot` binary for controls"
        ));
    }
    std::fs::create_dir_all(&botstate).map_err(|e| e.to_string())?;
    let path = botstate.join("control.json");
    let mut current: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    let obj = current
        .as_object_mut()
        .ok_or("control.json is not an object")?;
    obj.insert("halt".into(), json!(halt));
    obj.insert("reason".into(), json!(reason));
    obj.insert("set_by".into(), json!(by));
    std::fs::write(&path, serde_json::to_string_pretty(&current).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(current)
}


/// GET /api/bots/{id}/state — the per-bot detail document the dashboard's
/// bot view renders. Shape depends on the bot's state contract.
pub fn detail(repo_root: &Path, bot_id: &str) -> Result<Value, String> {
    let bot = with_registry(|reg| Box::pin(reg.bot(bot_id.to_string().leak())))?
        .ok_or_else(|| format!("unknown bot {bot_id:?}"))?;
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
