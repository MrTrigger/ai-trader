//! Per-bot portfolio views from registered paths and that bot's records.
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Deserialize)]
struct ExecutorView {
    bot_id: String,
    venue_id: String,
    state_dir: PathBuf,
    quote_currency: String,
    #[serde(with = "rust_decimal::serde::str")]
    initial_cash: Decimal,
    cadence_hours: i64,
    mode: String,
}

fn validate(bot: &records::BotRow, config: &ExecutorView, root: &Path) -> Result<(), String> {
    if config.bot_id != bot.bot_id {
        return Err("registered executor config belongs to another bot".into());
    }
    if bot.state_dir.as_ref().map(|p| root.join(p)) != Some(root.join(&config.state_dir)) {
        return Err("registered state directory differs from executor config".into());
    }
    if bot.paper_only && config.mode != "paper" {
        return Err("registered bot supports paper mode only".into());
    }
    if config.initial_cash <= Decimal::ZERO
        || config.cadence_hours <= 0
        || config.quote_currency.is_empty()
    {
        return Err("executor book metadata is invalid".into());
    }
    Ok(())
}

pub fn build(
    root: &Path,
    bot: &records::BotRow,
    bindings: &[records::Binding],
) -> Result<crate::state::Snapshot, String> {
    let path = root.join(
        bot.executor_config
            .as_ref()
            .ok_or("no executor config registered for this bot")?,
    );
    let config: ExecutorView =
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid executor book metadata: {e}"))?;
    validate(bot, &config, root)?;
    let kind = match config.mode.as_str() {
        "paper" => "paper",
        "live" | "live-readonly" => "live",
        _ => return Err("invalid executor mode".into()),
    };
    records::validate_execution_binding(bot, bindings, &config.venue_id, kind)
        .map_err(|e| e.to_string())?;
    let state_dir = root.join(&config.state_dir);
    let data_root = root.join(
        bot.data_root
            .as_ref()
            .ok_or("no data root registered for this bot")?,
    );
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set")?;
    let records = records::blocking::Records::connect(&url).map_err(|e| e.to_string())?;
    let mut snapshot = crate::state::build(&crate::state::Inputs {
        state_dir: &state_dir,
        data_root: &data_root,
        initial_cash: config.initial_cash,
        quote_currency: &config.quote_currency,
        cadence_hours: config.cadence_hours,
        expectation_path: None,
        controls_enabled: false,
        bot_config: Some(&path),
        records: Some((records, bot.bot_id.clone())),
        run_limit: 200,
        fill_limit: 12,
    })?;
    let feed = crate::feed::read(
        root,
        bot.state_dir.as_deref(),
        bot.trade_protocol.as_deref(),
    );
    let warning = match feed {
        Some(feed) if feed["healthy"] != true => {
            Some("Price feed is stale or failing; displayed marks may be out of date.")
        }
        None => Some("Price feed has not published its status; mark freshness is unknown."),
        _ => None,
    };
    if let Some(note) = warning {
        snapshot.warnings.push(note.into());
        snapshot.health.ok = false;
        snapshot.health.notes.push(note.into());
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_metadata_cannot_cross_bot_or_state_directory() {
        let bot = records::BotRow {
            bot_id: "crypto-lighter".into(),
            display_name: "Lighter".into(),
            cadence: "cron".into(),
            asset_class: "crypto".into(),
            decision_core: "crypto-portfolio".into(),
            enabled: false,
            state_dir: Some("/data/lighter/state".into()),
            launch: None,
            executor_config: None,
            core_config: None,
            data_root: None,
            trade_protocol: Some("lighter".into()),
            paper_only: true,
        };
        let mut config = ExecutorView {
            bot_id: bot.bot_id.clone(),
            venue_id: "lighter".into(),
            state_dir: "/data/lighter/state".into(),
            quote_currency: "USDC".into(),
            initial_cash: Decimal::from(100000),
            cadence_hours: 24,
            mode: "paper".into(),
        };
        assert!(validate(&bot, &config, Path::new("/app")).is_ok());
        config.bot_id = "crypto-portfolio".into();
        assert!(validate(&bot, &config, Path::new("/app")).is_err());
        config.bot_id = bot.bot_id.clone();
        config.state_dir = "/data/state".into();
        assert!(validate(&bot, &config, Path::new("/app")).is_err());
        config.state_dir = "/data/lighter/state".into();
        config.mode = "live-readonly".into();
        assert!(validate(&bot, &config, Path::new("/app")).is_err());
    }
}
