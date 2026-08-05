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
    /// Where controls, run history and venue state live.
    pub state_dir: PathBuf,
    /// `paper` is the only one built. A missing adapter must be an error at
    /// startup, not a surprise at the first order.
    #[serde(default = "default_venue")]
    pub venue: String,
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
    /// How old a plan may be when the first slice goes out.
    #[serde(default = "default_max_age")]
    pub max_plan_age_minutes: i64,
    /// How often a run is expected. Health goes bad at a cadence and a half.
    #[serde(default = "default_cadence")]
    pub cadence_hours: i64,
    /// Per-asset market rules. Assets absent here cannot be traded at all,
    /// which is the safe direction: an unknown lot size is a rejected order,
    /// not a guessed one.
    #[serde(default)]
    pub markets: Vec<MarketConfig>,
}

fn default_venue() -> String {
    "paper".into()
}
fn default_max_age() -> i64 {
    120
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
        if cfg.venue != "paper" {
            return Err(format!(
                "venue '{}' is not implemented. Only 'paper' exists so far; a live adapter is \
                 not something to discover missing at the first order.",
                cfg.venue
            ));
        }
        Ok(cfg)
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
                capabilities: Capabilities {
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
