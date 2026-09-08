//! Replay execution scenarios. Assumed liquidity is never labelled a measurement.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub model: Model,
    pub delay_hours: u32,
    pub participation: f64,
    pub impact_coefficient: f64,
    pub spread_multiple: f64,
    pub impact_multiple: f64,
    pub half_spread_bps: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    Flat,
    Impact,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: Model::Impact,
            delay_hours: 0,
            participation: 0.01,
            impact_coefficient: 0.1,
            spread_multiple: 1.0,
            impact_multiple: 1.0,
            half_spread_bps: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.delay_hours > 23
            || !self.participation.is_finite()
            || self.participation <= 0.0
            || self.participation > 1.0
            || !self.impact_coefficient.is_finite()
            || self.impact_coefficient < 0.0
            || !self.spread_multiple.is_finite()
            || self.spread_multiple <= 0.0
            || !self.impact_multiple.is_finite()
            || self.impact_multiple <= 0.0
            || self
                .half_spread_bps
                .values()
                .any(|v| !v.is_finite() || *v < 0.0 || *v >= 10_000.0)
        {
            return Err("invalid execution scenario: delay 0..23h, participation (0,1], finite nonnegative impact/spreads and positive multipliers required".into());
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let value: Self = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        value.validate()?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Liquidity {
    pub hourly_volume: f64,
    pub hourly_volatility: f64,
}

#[derive(Debug)]
pub struct Quote {
    pub qty: Decimal,
    pub spread_bps: Decimal,
    pub impact_bps: Decimal,
}

pub type Prices = BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>>;

pub fn quote(
    cfg: &Config,
    settings: &Settings,
    asset: &str,
    qty: Decimal,
    reference: Decimal,
    liquidity: Option<Liquidity>,
    stress: Decimal,
) -> Result<Option<Quote>, String> {
    if qty <= Decimal::ZERO || reference <= Decimal::ZERO {
        return Err(format!(
            "{asset}: execution requires positive quantity and price"
        ));
    }
    let spread = settings
        .half_spread_bps
        .get(asset)
        .copied()
        .unwrap_or_else(|| cfg.costs.spread_bps.to_f64().unwrap());
    let spread_bps = decimal(spread * settings.spread_multiple)? * stress;
    if spread_bps < Decimal::ZERO || spread_bps >= Decimal::from(10_000) {
        return Err(format!("{asset}: invalid stressed spread"));
    }
    if settings.model == Model::Flat {
        return Ok(Some(Quote {
            qty,
            spread_bps,
            impact_bps: Decimal::ZERO,
        }));
    }
    let Some(liquidity) = liquidity.filter(|l| l.hourly_volume > 0.0 && l.hourly_volatility > 0.0)
    else {
        return Ok(None);
    };
    let requested = (qty * reference)
        .to_f64()
        .ok_or("unrepresentable order notional")?;
    let notional = requested.min(liquidity.hourly_volume * settings.participation);
    let impact = settings.impact_coefficient
        * liquidity.hourly_volatility
        * (notional / liquidity.hourly_volume).sqrt()
        * 10_000.0
        * settings.impact_multiple;
    let impact_bps = decimal(impact)? * stress;
    if spread_bps + impact_bps >= Decimal::from(10_000) {
        return Err(format!(
            "{asset}: execution scenario implies a nonpositive sell price"
        ));
    }
    let filled_qty = qty
        .min(decimal(notional)? / reference)
        .round_dp_with_strategy(8, rust_decimal::RoundingStrategy::ToZero);
    if filled_qty.is_zero() {
        return Ok(None);
    }
    Ok(Some(Quote {
        qty: filled_qty,
        spread_bps,
        impact_bps,
    }))
}

fn decimal(value: f64) -> Result<Decimal, String> {
    Decimal::from_f64(value).ok_or_else(|| format!("nonfinite execution value {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impact_and_capacity_depend_on_order_size_and_liquidity() {
        let cfg = Config::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/default.toml"),
        )
        .unwrap();
        let settings = Settings {
            participation: 0.1,
            ..Default::default()
        };
        let liq = Some(Liquidity {
            hourly_volume: 100_000.0,
            hourly_volatility: 0.02,
        });
        let small = quote(
            &cfg,
            &settings,
            "BTC",
            Decimal::from(10),
            Decimal::from(100),
            liq,
            Decimal::ONE,
        )
        .unwrap()
        .unwrap();
        let large = quote(
            &cfg,
            &settings,
            "BTC",
            Decimal::from(1000),
            Decimal::from(100),
            liq,
            Decimal::ONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(large.qty, Decimal::from(100));
        assert!(large.impact_bps > small.impact_bps);
        let stressed = quote(
            &cfg,
            &settings,
            "BTC",
            Decimal::from(10),
            Decimal::from(100),
            liq,
            Decimal::from(2),
        )
        .unwrap()
        .unwrap();
        assert_eq!(stressed.impact_bps, small.impact_bps * Decimal::from(2));
        assert_eq!(stressed.spread_bps, small.spread_bps * Decimal::from(2));
        assert!(quote(
            &cfg,
            &settings,
            "BTC",
            Decimal::ONE,
            Decimal::from(100),
            None,
            Decimal::ONE
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn settings_reject_future_day_execution_and_invalid_liquidity() {
        assert!(Settings {
            delay_hours: 24,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Settings {
            participation: f64::NAN,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
