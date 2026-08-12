//! What each name costs to touch, measured rather than assumed.
//!
//! The cost model shipped with one spread for every asset and an impact term
//! keyed off *daily* volume. Both are wrong in ways that only show up in the
//! thin tail: a live book capture put KAITO's spread between 4.9 and 15.3bp
//! against the flat 0.50 the model gives everything, and the cost of crossing
//! its $9k order at 17.4bp against a modelled 3.1.
//!
//! Daily volume is also the wrong denominator. An order is executed in slices,
//! each meeting one hour's liquidity, so the quantity that decides impact is
//! the volume of the hour we send in — which is why the trading hour changes
//! how large a position the thin tail can carry at all.
//!
//! This is the artefact that carries both, per asset:
//!
//! - `spread_bps`, measured from the resting book. Absent for a name never
//!   sampled, and the caller falls back to the flat model rather than guessing.
//! - `hourly_quote_volume`, the median for the hours the bot actually trades
//!   in, from the bar store.
//!
//! It is deliberately a file rather than a config block. A measurement has a
//! date and goes stale; a config value pretends not to.

use std::collections::BTreeMap;
use std::path::Path;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// One name's measured tradeability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetLiquidity {
    /// Full spread in basis points, median of the observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread_bps: Option<Decimal>,
    /// Median quote-currency volume in one of the hours this bot trades in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hourly_quote_volume: Option<Decimal>,
    /// How many observations stand behind the spread. One is a number; a
    /// dozen is a measurement.
    #[serde(default)]
    pub spread_samples: u32,
}

/// The measured book, as of a stated moment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    /// When this was measured. Carried into the plan so a stale profile is
    /// visible rather than silently authoritative.
    pub measured_at: String,
    /// The hours of the day the volume figures describe, UTC.
    #[serde(default)]
    pub hours: Vec<u32>,
    pub assets: BTreeMap<String, AssetLiquidity>,
}

impl Profile {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn spread(&self, asset: &str) -> Option<Decimal> {
        self.assets.get(asset).and_then(|a| a.spread_bps)
    }

    pub fn hourly_volume(&self, asset: &str) -> Option<Decimal> {
        self.assets.get(asset).and_then(|a| a.hourly_quote_volume)
    }

    /// How much of the book this profile can actually speak for. A profile
    /// covering three of twenty-four names should not read as a calibrated
    /// cost model.
    pub fn coverage(&self, assets: impl IntoIterator<Item = String>) -> (usize, usize) {
        let mut total = 0;
        let mut measured = 0;
        for a in assets {
            total += 1;
            if self.spread(&a).is_some() {
                measured += 1;
            }
        }
        (measured, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_never_sampled_reports_nothing_rather_than_zero() {
        let p = Profile::default();
        assert!(p.spread("KAITO").is_none());
        assert!(p.hourly_volume("KAITO").is_none());
        // Zero would be the dangerous answer: a zero spread makes every trade
        // look free, and a zero volume makes every position look uncappable.
    }

    #[test]
    fn coverage_counts_the_names_it_can_speak_for() {
        let mut p = Profile::default();
        p.assets.insert(
            "BTC".into(),
            AssetLiquidity {
                spread_bps: Some(Decimal::new(5, 1)),
                ..Default::default()
            },
        );
        p.assets.insert(
            "KAITO".into(),
            AssetLiquidity {
                // Volume without a spread: the bars gave us one and the book
                // capture never reached this name.
                hourly_quote_volume: Some(Decimal::from(1000)),
                ..Default::default()
            },
        );
        let (measured, total) = p.coverage(["BTC".to_string(), "KAITO".to_string(), "ETH".into()]);
        assert_eq!((measured, total), (1, 3));
    }

    #[test]
    fn a_profile_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("liq-{}", std::process::id()));
        let path = dir.join("profile.json");
        let mut p = Profile {
            measured_at: "2026-08-12T09:00:00Z".into(),
            hours: vec![1, 2],
            assets: BTreeMap::new(),
        };
        p.assets.insert(
            "KAITO".into(),
            AssetLiquidity {
                spread_bps: Some(Decimal::new(1533, 2)),
                hourly_quote_volume: Some(Decimal::from(180_000)),
                spread_samples: 12,
            },
        );
        p.write(&path).unwrap();
        let back = Profile::load(&path).unwrap();
        assert_eq!(back.spread("KAITO"), Some(Decimal::new(1533, 2)));
        assert_eq!(back.hours, vec![1, 2]);
        assert_eq!(back.assets["KAITO"].spread_samples, 12);
        std::fs::remove_dir_all(&dir).ok();
    }
}
