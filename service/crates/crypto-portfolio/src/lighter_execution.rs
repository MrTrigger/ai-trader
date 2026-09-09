//! Native execution inputs, separate from the external bars used by the ranker.
use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Timelike;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{execution, lighter_history, Portfolio};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub daily: PathBuf,
    pub funding: PathBuf,
    pub hourly: Option<PathBuf>,
    #[serde(default)]
    pub hourly_extensions: Vec<PathBuf>,
    #[serde(default = "one_hour")]
    pub execution_hours: u32,
    #[serde(default)]
    pub valuation_hours: Option<u32>,
}

fn one_hour() -> u32 {
    1
}

impl Sources {
    fn validate(&self) -> Result<u32, String> {
        if !(1..=4).contains(&self.execution_hours) {
            return Err("native execution_hours must be between 1 and 4".into());
        }
        if self.hourly.is_none() {
            if !self.hourly_extensions.is_empty() {
                return Err("hourly extensions require a primary hourly collection".into());
            }
            if self.execution_hours != 1 || self.valuation_hours.is_some() {
                return Err(
                    "execution windows and valuation_hours require native hourly data".into(),
                );
            }
            return Ok(0);
        }
        let valuation = self.valuation_hours.unwrap_or(self.execution_hours);
        validate_window(self.execution_hours, valuation)?;
        Ok(valuation)
    }
}

fn validate_window(execution_hours: u32, valuation_hours: u32) -> Result<(), String> {
    if !(1..=4).contains(&execution_hours) || !(execution_hours..=4).contains(&valuation_hours) {
        return Err(
            "native hourly window requires 1 <= execution_hours <= valuation_hours <= 4".into(),
        );
    }
    Ok(())
}

pub struct Native {
    pub history: lighter_history::History,
    pub opens: execution::Prices,
    pub liquidity: BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
    pub planning_liquidity: BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
    pub fills: execution::Prices,
    pub hourly_execution: bool,
    pub execution_hours: u32,
    pub valuation_hours: u32,
    pub slice_liquidity: BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
    payments: BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>>,
    rates: BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>>,
}

#[derive(Deserialize)]
struct FundingPage {
    code: u16,
    resolution: String,
    fundings: Vec<Payment>,
}

#[derive(Deserialize)]
struct Payment {
    timestamp: i64,
    #[serde(default, with = "rust_decimal::serde::str")]
    value: Decimal,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    rate: Option<Decimal>,
    direction: String,
}

fn signed_payment(payment: &Payment) -> Result<Decimal, String> {
    if payment.value < Decimal::ZERO {
        return Err("Lighter payment magnitude is negative".into());
    }
    match payment.direction.as_str() {
        "long" => Ok(payment.value),
        "short" => Ok(-payment.value),
        _ => Err(format!(
            "unknown Lighter funding direction {}",
            payment.direction
        )),
    }
}

fn signed_rate(payment: &Payment) -> Result<Option<Decimal>, String> {
    let Some(rate) = payment.rate else {
        return Ok(None);
    };
    if rate < Decimal::ZERO {
        return Err("Lighter rate magnitude is negative".into());
    }
    let fractional = rate / Decimal::from(100);
    match payment.direction.as_str() {
        "long" => Ok(Some(fractional)),
        "short" => Ok(Some(-fractional)),
        _ => Err(format!(
            "unknown Lighter funding direction {}",
            payment.direction
        )),
    }
}

impl Native {
    pub fn load(sources: &Sources) -> Result<Self, String> {
        let valuation_hours = sources.validate()?;
        let history = lighter_history::History::load(&sources.daily)?;
        let completion: serde_json::Value = serde_json::from_slice(
            &std::fs::read(sources.funding.join("COMPLETE.json"))
                .map_err(|_| "native Lighter funding download is incomplete")?,
        )
        .map_err(|e| e.to_string())?;
        if completion["schema"] != "lighter-native-hourly-funding-collection-1" {
            return Err("invalid native Lighter funding completion manifest".into());
        }
        let snapshot: serde_json::Value = serde_json::from_slice(
            &std::fs::read(sources.daily.join("market-snapshot.json"))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut symbols = BTreeMap::new();
        for market in snapshot["order_books"]
            .as_array()
            .ok_or("missing Lighter markets")?
        {
            if market["market_type"] == "perp" {
                symbols.insert(
                    market["market_id"].as_u64().ok_or("invalid market id")?,
                    market["symbol"]
                        .as_str()
                        .ok_or("invalid market symbol")?
                        .to_owned(),
                );
            }
        }
        let mut payments = BTreeMap::<_, BTreeMap<_, _>>::new();
        let mut rates = BTreeMap::<_, BTreeMap<_, _>>::new();
        for entry in std::fs::read_dir(&sources.funding).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("market-") || !name.ends_with(".json") {
                continue;
            }
            let id = name
                .split('-')
                .nth(1)
                .ok_or("funding filename lacks market id")?
                .parse::<u64>()
                .map_err(|e| e.to_string())?;
            let asset = symbols
                .get(&id)
                .ok_or("funding market missing from metadata")?;
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let checksum = std::fs::read_to_string(path.with_extension("json.sha256"))
                .map_err(|e| e.to_string())?;
            if checksum.split_whitespace().next()
                != Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
            {
                return Err(format!("{} checksum mismatch", path.display()));
            }
            let page: FundingPage =
                serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {e}"))?;
            if page.code != 200 || page.resolution != "1h" {
                return Err(
                    "native Lighter funding requires hourly events; daily is subsampled".into(),
                );
            }
            for payment in page.fundings {
                if payment.timestamp % 3600 != 0 {
                    return Err("Lighter funding timestamp is not an hourly boundary".into());
                }
                let at =
                    DateTime::from_timestamp(payment.timestamp, 0).ok_or("invalid funding time")?;
                let amount = signed_payment(&payment)?;
                if let Some(rate) = signed_rate(&payment)? {
                    let prior = rates.entry(at).or_default().insert(asset.clone(), rate);
                    if prior.is_some_and(|old| old != rate) {
                        return Err(format!(
                            "conflicting Lighter funding rate for {asset} at {at}"
                        ));
                    }
                }
                let prior = payments
                    .entry(at)
                    .or_default()
                    .insert(asset.clone(), amount);
                if prior.is_some_and(|v| v != amount) {
                    return Err(format!("conflicting Lighter payment for {asset} at {at}"));
                }
            }
        }
        if payments.is_empty() {
            return Err("native funding has no hourly payments".into());
        }
        let mut opens = execution::Prices::new();
        for bar in &history.daily {
            opens
                .entry(bar.ts_utc)
                .or_default()
                .insert(bar.asset.clone(), crate::d(bar.open)?);
        }
        let liquidity = native_liquidity(&history.daily)?;
        let planning_liquidity = prior_liquidity(&history.daily, false)?;
        let mut native = Self {
            fills: execution::Prices::new(),
            hourly_execution: false,
            execution_hours: sources.execution_hours,
            valuation_hours,
            slice_liquidity: BTreeMap::new(),
            planning_liquidity,
            history,
            opens,
            liquidity,
            payments,
            rates,
        };
        if let Some(root) = &sources.hourly {
            let roots: Vec<_> = std::iter::once(root.clone())
                .chain(sources.hourly_extensions.iter().cloned())
                .collect();
            let hourly =
                crate::lighter_hourly::History::load_many(&roots, &native.history.market_ids)?;
            native.use_hourly(&hourly)?;
        }
        Ok(native)
    }

    fn use_hourly(&mut self, hourly: &crate::lighter_hourly::History) -> Result<(), String> {
        let result = hourly_execution(
            hourly,
            &self.planning_liquidity,
            self.execution_hours,
            self.valuation_hours,
        )?;
        self.opens = result.marks;
        self.fills = result.fills;
        self.liquidity = result.liquidity;
        self.slice_liquidity = result.slice_liquidity;
        self.hourly_execution = true;
        Ok(())
    }

    pub fn funding_forecast(
        &self,
        as_of: DateTime<Utc>,
        assets: &std::collections::BTreeSet<String>,
    ) -> Result<crate::funding::NativeForecast, String> {
        crate::funding::native_forecast(&self.rates, as_of, assets)
    }

    pub fn carry(
        &self,
        book: &Portfolio,
        previous: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<Decimal, String> {
        funding_carry(&self.payments, book, previous, now)
    }
}

struct HourlyExecution {
    slice_liquidity: BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
    marks: execution::Prices,
    fills: execution::Prices,
    liquidity: BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
}

fn hourly_execution(
    hourly: &crate::lighter_hourly::History,
    planning: &BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>,
    execution_hours: u32,
    valuation_hours: u32,
) -> Result<HourlyExecution, String> {
    validate_window(execution_hours, valuation_hours)?;
    let mut result = HourlyExecution {
        marks: execution::Prices::new(),
        fills: execution::Prices::new(),
        liquidity: BTreeMap::new(),
        slice_liquidity: BTreeMap::new(),
    };
    for (decision, first_bars) in hourly.bars.iter().filter(|(at, _)| at.hour() == 0) {
        let valuation = *decision + Duration::hours(i64::from(valuation_hours));
        let marks = hourly.bars.get(&valuation).ok_or_else(|| {
            format!("missing native valuation hour {valuation}; refusing to suppress earlier fills")
        })?;
        for asset in first_bars.keys() {
            let mark = marks.get(asset).filter(|b| b.open > Decimal::ZERO)
                .ok_or_else(|| format!("{asset} at {valuation}: missing positive valuation; refusing to suppress earlier fills"))?;
            result
                .marks
                .entry(*decision)
                .or_default()
                .insert(asset.clone(), mark.open);
            let Some(prior) = planning.get(decision).and_then(|v| v.get(asset)) else {
                continue;
            };
            for offset in 0..execution_hours {
                let hour = *decision + Duration::hours(i64::from(offset));
                let bar = hourly
                    .bars
                    .get(&hour)
                    .and_then(|bars| bars.get(asset))
                    .ok_or_else(|| format!("{asset} at {hour}: missing native execution hour"))?;
                let observed = bar
                    .quote_volume
                    .to_f64()
                    .ok_or("invalid hourly quote volume")?;
                let liquidity = execution::Liquidity {
                    hourly_volume: if bar.traded() {
                        prior.hourly_volume.min(observed)
                    } else {
                        0.0
                    },
                    hourly_volatility: prior.hourly_volatility,
                };
                result
                    .slice_liquidity
                    .entry(hour)
                    .or_default()
                    .insert(asset.clone(), liquidity);
                if offset == 0 {
                    result
                        .liquidity
                        .entry(*decision)
                        .or_default()
                        .insert(asset.clone(), liquidity);
                }
                if bar.traded() && bar.vwap().is_none() {
                    return Err(format!("{asset} at {hour}: unusable native VWAP"));
                }
                if let Some(vwap) = bar.vwap() {
                    result
                        .fills
                        .entry(hour + Duration::hours(1))
                        .or_default()
                        .insert(asset.clone(), vwap);
                }
            }
        }
    }
    Ok(result)
}

fn native_liquidity(
    bars: &[features_crypto::Bar],
) -> Result<BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>, String> {
    prior_liquidity(bars, true)
}

fn prior_liquidity(
    bars: &[features_crypto::Bar],
    executable: bool,
) -> Result<BTreeMap<DateTime<Utc>, BTreeMap<String, execution::Liquidity>>, String> {
    let daily = features_crypto::daily(
        bars,
        Some("BTC"),
        &BTreeMap::new(),
        &BTreeMap::new(),
        features_crypto::FundingWindow::Trailing,
    )?;
    let volumes: BTreeMap<_, _> = bars
        .iter()
        .map(|b| ((b.ts_utc, b.asset.clone()), b.volume))
        .collect();
    let mut liquidity = BTreeMap::<_, BTreeMap<_, _>>::new();
    for row in daily {
        if let (Some(adv), Some(vol)) = (row.adv_quote, row.vol_30) {
            let at = row.ts_utc + Duration::days(1);
            // Ex-post availability: a synthetic zero-trade candle has no
            // executable open. Positive current volume never changes capacity.
            let trades = volumes
                .get(&(at, row.asset.clone()))
                .is_some_and(|v| *v > 0.0);
            liquidity.entry(at).or_default().insert(
                row.asset,
                execution::Liquidity {
                    hourly_volume: if !executable || trades {
                        adv / 24.0
                    } else {
                        0.0
                    },
                    hourly_volatility: vol / (365.0_f64 * 24.0).sqrt(),
                },
            );
        }
    }
    Ok(liquidity)
}

fn funding_carry(
    payments: &BTreeMap<DateTime<Utc>, BTreeMap<String, Decimal>>,
    book: &Portfolio,
    previous: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Decimal, String> {
    let Some(previous) = previous else {
        return Ok(Decimal::ZERO);
    };
    let mut result = Decimal::ZERO;
    let mut at = previous + Duration::hours(1);
    while at <= now {
        for position in &book.positions {
            if position.qty == Decimal::ZERO {
                continue;
            }
            let amount = payments.get(&at).and_then(|p| p.get(&position.asset))
                .ok_or_else(|| format!("missing native Lighter funding for {} at {at}; refusing proxy or zero substitution", position.asset))?;
            result -= position.qty * *amount;
        }
        at += Duration::hours(1);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;
    #[test]
    fn native_rate_percent_and_per_unit_payment_have_distinct_units() {
        let mut payment: Payment = serde_json::from_value(serde_json::json!({
            "timestamp":0,"value":"0.03551304","rate":"0.0012","direction":"long"
        }))
        .unwrap();
        assert_eq!(signed_rate(&payment).unwrap(), Some(Decimal::new(12, 6)));
        assert_eq!(signed_payment(&payment).unwrap(), Decimal::new(3551304, 8));
        payment.direction = "short".into();
        assert_eq!(signed_rate(&payment).unwrap(), Some(Decimal::new(-12, 6)));
        assert_eq!(signed_payment(&payment).unwrap(), Decimal::new(-3551304, 8));
        payment.rate = None;
        assert_eq!(signed_rate(&payment).unwrap(), None);
        payment.rate = Some(-Decimal::ONE);
        assert!(signed_rate(&payment).is_err());
    }

    #[test]
    fn hourly_execution_uses_observed_capacity_vwap_and_next_hour_mark() {
        let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let bar = crate::lighter_hourly::Bar {
            ts_utc: start,
            asset: "BTC".into(),
            open: Decimal::from(100),
            high: Decimal::from(106),
            low: Decimal::from(99),
            close: Decimal::from(104),
            base_volume: Decimal::from(100),
            quote_volume: Decimal::from(10200),
        };
        let next = crate::lighter_hourly::Bar {
            ts_utc: start + Duration::hours(1),
            open: Decimal::from(104),
            ..bar.clone()
        };
        let mut hourly = crate::lighter_hourly::History {
            bars: BTreeMap::from([
                (start, BTreeMap::from([("BTC".into(), bar)])),
                (
                    start + Duration::hours(1),
                    BTreeMap::from([("BTC".into(), next)]),
                ),
            ]),
            market_ids: BTreeMap::from([("BTC".into(), 1)]),
            start,
            end_exclusive: start + Duration::hours(2),
            page_sha256: BTreeMap::new(),
            unusable_vwap: Vec::new(),
        };
        let planning = BTreeMap::from([(
            start,
            BTreeMap::from([(
                "BTC".into(),
                execution::Liquidity {
                    hourly_volume: 20000.0,
                    hourly_volatility: 0.01,
                },
            )]),
        )]);
        let result = hourly_execution(&hourly, &planning, 1, 1).unwrap();
        assert_eq!(result.marks[&start]["BTC"], Decimal::from(104));
        assert_eq!(
            result.fills[&(start + Duration::hours(1))]["BTC"],
            Decimal::from(102)
        );
        assert_eq!(result.liquidity[&start]["BTC"].hourly_volume, 10200.0);
        let bar = hourly.bars.get_mut(&start).unwrap().get_mut("BTC").unwrap();
        bar.base_volume = Decimal::ZERO;
        bar.quote_volume = Decimal::ZERO;
        let zero = hourly_execution(&hourly, &planning, 1, 1).unwrap();
        assert_eq!(zero.marks, result.marks);
        assert!(zero.fills.is_empty());
        assert_eq!(zero.liquidity[&start]["BTC"].hourly_volume, 0.0);
        assert_eq!(planning[&start]["BTC"].hourly_volume, 20000.0);
    }

    #[test]
    fn execution_window_configuration_preserves_legacy_defaults() {
        let mut source: Sources = serde_json::from_value(serde_json::json!({
            "daily":"daily", "funding":"funding"
        }))
        .unwrap();
        assert_eq!(source.execution_hours, 1);
        assert_eq!(source.validate().unwrap(), 0);
        source.valuation_hours = Some(1);
        assert!(source.validate().is_err());
        source.valuation_hours = None;
        source.execution_hours = 4;
        assert!(source.validate().is_err());
        source.hourly = Some("hourly".into());
        assert_eq!(source.validate().unwrap(), 4);
        source.valuation_hours = Some(3);
        assert!(source.validate().is_err());
        source.execution_hours = 1;
        source.valuation_hours = Some(4);
        assert_eq!(source.validate().unwrap(), 4);
        source.valuation_hours = Some(5);
        assert!(source.validate().is_err());
        source.valuation_hours = None;
        for invalid in [0, 5] {
            source.execution_hours = invalid;
            assert!(source.validate().is_err());
        }
    }

    #[test]
    fn four_hour_slices_keep_midnight_inputs_and_match_control_valuation() {
        let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut bars = BTreeMap::new();
        for h in 0..=4 {
            let at = start + Duration::hours(h);
            bars.insert(
                at,
                BTreeMap::from([(
                    "BTC".into(),
                    crate::lighter_hourly::Bar {
                        ts_utc: at,
                        asset: "BTC".into(),
                        open: Decimal::from(100 + h),
                        high: Decimal::from(110),
                        low: Decimal::from(90),
                        close: Decimal::from(100),
                        base_volume: Decimal::from((h + 1) * 100),
                        quote_volume: Decimal::from((h + 1) * 10000),
                    },
                )]),
            );
        }
        // An unusable unused hour must not invalidate either execution window.
        let mut bad = bars[&start]["BTC"].clone();
        bad.ts_utc = start + Duration::hours(15);
        bad.quote_volume = Decimal::from(999999);
        bars.insert(bad.ts_utc, BTreeMap::from([("BTC".into(), bad)]));
        let mut hourly = crate::lighter_hourly::History {
            bars,
            market_ids: BTreeMap::from([("BTC".into(), 1)]),
            start,
            end_exclusive: start + Duration::days(1),
            page_sha256: BTreeMap::new(),
            unusable_vwap: Vec::new(),
        };
        let prior = execution::Liquidity {
            hourly_volume: 25000.0,
            hourly_volatility: 0.01,
        };
        let planning = BTreeMap::from([
            (start, BTreeMap::from([("BTC".into(), prior)])),
            (
                start + Duration::hours(1),
                BTreeMap::from([(
                    "BTC".into(),
                    execution::Liquidity {
                        hourly_volume: 999999.0,
                        hourly_volatility: 0.99,
                    },
                )]),
            ),
        ]);
        let four = hourly_execution(&hourly, &planning, 4, 4).unwrap();
        let control = hourly_execution(&hourly, &planning, 1, 4).unwrap();
        assert_eq!(four.marks[&start]["BTC"], Decimal::from(104));
        assert_eq!(four.marks, control.marks);
        assert_eq!(control.fills.len(), 1);
        assert_eq!(four.fills.len(), 4);
        for (h, expected) in [10000.0, 20000.0, 25000.0, 25000.0].into_iter().enumerate() {
            let hour = start + Duration::hours(h as i64);
            assert_eq!(four.slice_liquidity[&hour]["BTC"].hourly_volume, expected);
            assert_eq!(
                four.slice_liquidity[&hour]["BTC"].hourly_volatility,
                prior.hourly_volatility
            );
            assert_eq!(
                four.fills[&(hour + Duration::hours(1))]["BTC"],
                Decimal::from(100)
            );
        }
        assert_eq!(
            four.liquidity[&start]["BTC"].hourly_volume,
            control.liquidity[&start]["BTC"].hourly_volume
        );
        let zero_at = start + Duration::hours(2);
        let bar = hourly
            .bars
            .get_mut(&zero_at)
            .unwrap()
            .get_mut("BTC")
            .unwrap();
        bar.base_volume = Decimal::ZERO;
        bar.quote_volume = Decimal::ZERO;
        let zero = hourly_execution(&hourly, &planning, 4, 4).unwrap();
        assert_eq!(zero.marks, four.marks);
        assert_eq!(zero.slice_liquidity[&zero_at]["BTC"].hourly_volume, 0.0);
        assert!(!zero.fills.contains_key(&(zero_at + Duration::hours(1))));
        for h in [0, 1, 3] {
            let hour = start + Duration::hours(h);
            assert_eq!(
                zero.slice_liquidity[&hour]["BTC"].hourly_volume,
                four.slice_liquidity[&hour]["BTC"].hourly_volume
            );
            assert_eq!(
                zero.fills[&(hour + Duration::hours(1))],
                four.fills[&(hour + Duration::hours(1))]
            );
        }
        assert_eq!(planning[&start]["BTC"].hourly_volume, 25000.0);
        let bad = hourly
            .bars
            .get_mut(&zero_at)
            .unwrap()
            .get_mut("BTC")
            .unwrap();
        bad.base_volume = Decimal::ONE;
        bad.quote_volume = Decimal::from(999999);
        assert!(hourly_execution(&hourly, &planning, 4, 4)
            .err()
            .unwrap()
            .contains("unusable native VWAP"));
        assert!(hourly_execution(&hourly, &planning, 1, 4).is_ok());
        hourly
            .bars
            .get_mut(&(start + Duration::hours(4)))
            .unwrap()
            .remove("BTC");
        assert!(hourly_execution(&hourly, &planning, 1, 4)
            .err()
            .unwrap()
            .contains("refusing to suppress earlier fills"));
    }

    #[test]
    fn completion_time_funding_charges_exactly_twenty_four_old_book_events() {
        let previous: DateTime<Utc> = "2026-01-01T01:00:00Z".parse().unwrap();
        let payments = (0..=25)
            .map(|h| {
                (
                    previous + Duration::hours(h),
                    BTreeMap::from([("BTC".into(), Decimal::ONE)]),
                )
            })
            .collect();
        let old = Portfolio {
            cash: Decimal::from(1000),
            positions: vec![Position {
                asset: "BTC".into(),
                qty: Decimal::from(10),
            }],
        };
        assert_eq!(
            funding_carry(
                &payments,
                &old,
                Some(previous),
                previous + Duration::hours(24)
            )
            .unwrap(),
            -Decimal::from(240)
        );
    }
    #[test]
    fn hourly_funding_follows_the_book_after_each_completed_slice() {
        let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let payments = (1..=4)
            .map(|hour| {
                (
                    start + Duration::hours(hour),
                    BTreeMap::from([("BTC".into(), Decimal::ONE)]),
                )
            })
            .collect();
        let mut book = Portfolio {
            cash: Decimal::from(1000),
            positions: Vec::new(),
        };
        let first = start + Duration::hours(1);
        assert_eq!(
            funding_carry(&payments, &book, None, first).unwrap(),
            Decimal::ZERO
        );
        book.positions.push(Position {
            asset: "BTC".into(),
            qty: Decimal::from(3),
        });
        assert_eq!(
            funding_carry(&payments, &book, Some(first), first + Duration::hours(1)).unwrap(),
            -Decimal::from(3)
        );
        book.positions[0].qty = Decimal::from(5);
        assert_eq!(
            funding_carry(
                &payments,
                &book,
                Some(first + Duration::hours(1)),
                first + Duration::hours(3)
            )
            .unwrap(),
            -Decimal::from(10)
        );
    }
    #[test]
    fn current_positive_volume_cannot_change_prior_day_capacity() {
        let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut bars: Vec<_> = (0..100)
            .map(|i| {
                let close = 100.0 + (i % 3) as f64;
                features_crypto::Bar {
                    ts_utc: start + Duration::days(i),
                    asset: "BTC".into(),
                    interval_s: 86400,
                    open: 100.0,
                    high: 104.0,
                    low: 99.0,
                    close,
                    volume: 100.0,
                    quote_volume: Some(10000.0),
                    trades: None,
                }
            })
            .collect();
        let at = bars.last().unwrap().ts_utc;
        let before = native_liquidity(&bars).unwrap()[&at]["BTC"];
        let planning_before = prior_liquidity(&bars, false).unwrap()[&at]["BTC"];
        assert!(before.hourly_volume > 0.0 && before.hourly_volatility > 0.0);
        bars.last_mut().unwrap().volume = 1000000.0;
        bars.last_mut().unwrap().quote_volume = Some(100000000.0);
        let after = native_liquidity(&bars).unwrap()[&at]["BTC"];
        assert_eq!(before.hourly_volume, after.hourly_volume);
        assert_eq!(before.hourly_volatility, after.hourly_volatility);
        bars.last_mut().unwrap().volume = 0.0;
        let planning_after = prior_liquidity(&bars, false).unwrap()[&at]["BTC"];
        assert_eq!(planning_before.hourly_volume, planning_after.hourly_volume);
        assert_eq!(
            planning_before.hourly_volatility,
            planning_after.hourly_volatility
        );
        assert_eq!(
            native_liquidity(&bars).unwrap()[&at]["BTC"].hourly_volume,
            0.0
        );
    }
    #[test]
    fn native_payments_use_exact_value_and_refuse_missing_hours() {
        let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut payments = BTreeMap::new();
        payments.insert(start, BTreeMap::from([("BTC".into(), Decimal::from(999))]));
        payments.insert(
            start + Duration::hours(1),
            BTreeMap::from([("BTC".into(), Decimal::from(2))]),
        );
        payments.insert(
            start + Duration::hours(2),
            BTreeMap::from([("BTC".into(), Decimal::from(-1))]),
        );
        let book = Portfolio {
            cash: Decimal::from(100),
            positions: vec![Position {
                asset: "BTC".into(),
                qty: Decimal::from(3),
            }],
        };
        assert_eq!(
            funding_carry(&payments, &book, Some(start), start + Duration::hours(2)).unwrap(),
            Decimal::from(-3)
        );
        assert!(funding_carry(&payments, &book, Some(start), start + Duration::hours(3)).is_err());
        assert_eq!(
            signed_payment(&Payment {
                timestamp: 0,
                value: Decimal::from(2),
                rate: None,
                direction: "short".into()
            })
            .unwrap(),
            Decimal::from(-2)
        );
    }
}
