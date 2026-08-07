//! What the bot needs to know before it can do anything.
//!
//! Everything here is read from one file. There are no environment overrides
//! and no defaults for the numbers that cost money — a fee rate that quietly
//! defaults is a fee rate nobody checked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use venue::{Capabilities, Market};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    /// Free-form commentary. JSON has no comments and every number in this file
    /// is one somebody will need the reasoning for at 3am; the alternative is
    /// dropping `deny_unknown_fields`, which would make a mistyped fee rate a
    /// silent default instead of an error.
    #[serde(default)]
    pub notes: serde_json::Value,
    /// Identity registry key (bots table). Required: every artifact this
    /// process writes is namespaced by it, and when DATABASE_URL is set the
    /// process refuses to run unless this id is registered AND enabled
    /// (fail closed — identity is DB-first per the triggerlab mandate).
    pub bot_id: String,
    /// Which live venue this bot points at (venues registry key). Authority
    /// lives in `mode`; identity lives here — conflating them was the
    /// step-2 finding (docs/architecture-review-multibot.md).
    #[serde(default = "default_venue_id")]
    pub venue_id: String,
    /// Where controls, run history and venue state live.
    pub state_dir: PathBuf,
    /// `paper`, `live-readonly`, or `live`.
    ///
    /// The mode is here rather than only on the command line so that a
    /// scheduled run cannot pick a different one from an interactive one, and
    /// so the dashboard has a single place to change it.
    #[serde(default = "default_mode")]
    pub mode: crate::active_venue::Mode,
    /// Where marks come from: `hyperliquid` for the live feed, `file` for
    /// `marks.json`.
    ///
    /// A file feed means frozen prices, which is fine for a fixture and useless
    /// for a paper run — nothing moves, so there is no P&L, no slippage and no
    /// drawdown to measure.
    #[serde(default = "default_feed")]
    pub feed: String,
    pub quote_currency: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub initial_cash: Decimal,
    /// Hyperliquid perp taker, measured rather than assumed - see
    /// `config/default.toml`.
    #[serde(with = "rust_decimal::serde::str")]
    pub taker_fee_bps: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub maker_fee_bps: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub slippage_bps: Decimal,
    /// Hours between the signal and the first fill, and how many hourly slices
    /// the orders are spread over. See `runner`'s module docs for where these
    /// numbers came from.
    #[serde(default)]
    pub schedule: runner::Schedule,
    /// How long the plan file may have sat on disk when the first slice goes out.
    #[serde(default = "default_max_age")]
    pub max_plan_age_minutes: i64,
    /// How far behind its own decision moment a fill may be.
    ///
    /// A daily plan is stamped `as_of` midnight UTC, so this is measured in
    /// hours by nature, not minutes. Separate from `max_plan_age_minutes`
    /// because the two ask different questions - see `runner::check_decision_lag`.
    #[serde(default = "default_max_decision_lag")]
    pub max_decision_lag_minutes: i64,
    /// How often a run is expected. Health goes bad at a cadence and a half.
    #[serde(default = "default_cadence")]
    pub cadence_hours: i64,
    /// Where credentials live. Relative paths resolve against the config file's
    /// own directory, so a config and its secrets travel together.
    ///
    /// Left unset, the bot looks for `.env` beside the config and then in the
    /// working directory. Being explicit matters because the dashboard runs the
    /// bot with a cleared environment — a subprocess that can see the web
    /// server's environment is one the web server can steer — so the file is
    /// the only way credentials reach a run started from the page.
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    /// Per-asset market rules. Assets absent here cannot be traded at all,
    /// which is the safe direction: an unknown lot size is a rejected order,
    /// not a guessed one.
    #[serde(default)]
    pub markets: Vec<MarketConfig>,
}

fn default_mode() -> crate::active_venue::Mode {
    crate::active_venue::Mode::Paper
}
fn default_feed() -> String {
    "venue".into()
}
fn default_max_age() -> i64 {
    120
}
/// A daily book decides at midnight UTC and the cycle is cronned just after,
/// so the fill lag in normal operation is minutes. Six hours leaves room for a
/// late cron or a long execution window while still refusing a decision the
/// market has moved a full session away from.
fn default_max_decision_lag() -> i64 {
    360
}

fn default_cadence() -> i64 {
    24
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketConfig {
    pub asset: String,
    pub venue_symbol: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub tick: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub lot: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub min_notional: Decimal,
    #[serde(default = "yes")]
    pub short: bool,
}

fn yes() -> bool {
    true
}

impl BotConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read config {}: {e}", path.display()))?;
        let cfg: BotConfig = serde_json::from_str(&text)
            .map_err(|e| format!("cannot parse config {}: {e}", path.display()))?;
        // "venue" = the configured live venue's own price feed;
        // "hyperliquid" is accepted as a legacy alias for it.
        if !matches!(cfg.feed.as_str(), "venue" | "hyperliquid" | "file") {
            return Err(format!(
                "feed '{}' is not implemented. Use 'hyperliquid' for live prices, or 'file' to \
                 read a static marks.json.",
                cfg.feed
            ));
        }
        Ok(cfg)
    }

    /// The first `.env` that exists, of: the configured path, one beside the
    /// config, one in the working directory.
    pub fn env_path(&self, config_path: &Path) -> Option<PathBuf> {
        let dir = config_path.parent().unwrap_or(Path::new("."));
        let candidates = match &self.env_file {
            Some(p) if p.is_absolute() => vec![p.clone()],
            Some(p) => vec![dir.join(p)],
            None => vec![dir.join(".env"), PathBuf::from(".env")],
        };
        candidates.into_iter().find(|p| p.is_file())
    }

    pub fn controls_path(&self) -> PathBuf {
        self.state_dir.join("controls.json")
    }

    pub fn venue_state_path(&self) -> PathBuf {
        self.state_dir.join("venue-state.json")
    }

    /// Marks, written by whatever has a price feed.
    ///
    /// Deliberately a file the bot only reads. The process that trades does not
    /// decide, and it does not fetch prices either — a bot with its own feed is
    /// a bot with its own opinion about value.
    pub fn marks_path(&self) -> PathBuf {
        self.state_dir.join("marks.json")
    }

    pub fn markets(&self) -> Vec<Market> {
        self.markets
            .iter()
            .map(|m| Market {
                asset: m.asset.clone(),
                venue_symbol: m.venue_symbol.clone(),
                quote_currency: self.quote_currency.clone(),
                tick: m.tick,
                lot: m.lot,
                min_notional: m.min_notional,
                multiplier: Decimal::ONE,
                expiry: None,
                initial_margin: None,
                asset_class: "crypto".into(),
                capabilities: Capabilities {
                    stop_orders: false,
                    fractional: true,
                    short: m.short,
                    max_leverage: Decimal::ONE,
                    funding: true,
                },
            })
            .collect()
    }
}

/// Marks as `{"BTC": "64000.50"}`.
pub fn read_marks(path: &Path) -> Result<BTreeMap<String, Decimal>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read marks {}: {e}", path.display()))?;
    let raw: BTreeMap<String, String> = serde_json::from_str(&text)
        .map_err(|e| format!("cannot parse marks {}: {e}", path.display()))?;
    raw.into_iter()
        .map(|(k, v)| {
            v.parse::<Decimal>()
                .map(|d| (k.clone(), d))
                .map_err(|e| format!("mark for {k} is not a number: {e}"))
        })
        .collect()
}

fn default_venue_id() -> String {
    "hyperliquid".into()
}
