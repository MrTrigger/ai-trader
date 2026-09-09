//! Realised daily funding rates, read from the store.
//!
//! One file, `funding/binance_um.parquet`, produced by the recovered
//! `pull_funding.py`: per (asset, day), the sum of that day's 8h funding
//! intervals as a daily rate. Binance USD-M rates stand in for Hyperliquid's
//! own - the harness called this a proxy and so does the plan disclosure.
//!
//! A missing file is an empty table, not an error: funding features read 0.0
//! and the backtest simply carries no funding P&L, which is exactly what a
//! store without funding data can honestly claim.

use std::collections::BTreeMap;
use std::path::Path;

use arrow_array::{
    Array, Float64Array, LargeStringArray, StringArray, StringViewArray, TimestampMicrosecondArray,
};
use chrono::DateTime;
use features_crypto::FundingTable;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

pub fn load(root: &Path) -> Result<FundingTable, String> {
    let path = root.join("funding").join("binance_um.parquet");
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let mut table: FundingTable = BTreeMap::new();
    for batch in reader {
        let batch = batch.map_err(|e| e.to_string())?;
        // polars picks its own arrow string encoding; accept the three it uses.
        let asset_col = batch
            .column_by_name("asset")
            .ok_or("funding parquet: no asset column")?;
        let asset_at = |i: usize| -> Option<&str> {
            let a = asset_col.as_any();
            if let Some(v) = a.downcast_ref::<StringArray>() {
                (!v.is_null(i)).then(|| v.value(i))
            } else if let Some(v) = a.downcast_ref::<LargeStringArray>() {
                (!v.is_null(i)).then(|| v.value(i))
            } else if let Some(v) = a.downcast_ref::<StringViewArray>() {
                (!v.is_null(i)).then(|| v.value(i))
            } else {
                None
            }
        };
        let days = batch
            .column_by_name("day")
            .and_then(|c| c.as_any().downcast_ref::<TimestampMicrosecondArray>())
            .ok_or("funding parquet: no day column")?;
        let rates = batch
            .column_by_name("daily_rate")
            .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
            .ok_or("funding parquet: no daily_rate column")?;
        for i in 0..batch.num_rows() {
            let Some(asset) = asset_at(i) else { continue };
            if days.is_null(i) || rates.is_null(i) {
                continue;
            }
            let day = DateTime::from_timestamp_micros(days.value(i))
                .ok_or("funding parquet: day out of range")?
                .date_naive();
            table
                .entry(asset.to_owned())
                .or_default()
                .insert(day, rates.value(i));
        }
    }
    Ok(table)
}

/// Net a 24h price-return forecast against adverse expected funding only.
/// The feature is a sum of seven completed daily rates, including all payments
/// in each day. Positive rates charge longs; negative rates charge shorts.
pub(crate) fn net_conviction_24h(
    conviction: rust_decimal::Decimal,
    direction: plan::Direction,
    funding_7d: Option<f64>,
) -> Result<Option<rust_decimal::Decimal>, String> {
    use rust_decimal::Decimal;
    use std::str::FromStr;
    let rate = funding_7d.ok_or("funding adjustment requires a trailing funding_7d value")?;
    if !rate.is_finite() {
        return Err("funding adjustment requires finite funding_7d".into());
    }
    let daily_rate = Decimal::from_str(&rate.to_string())
        .map_err(|e| format!("funding_7d: {e}"))?
        / Decimal::from(7);
    let signed_cost = match direction {
        plan::Direction::Long => daily_rate,
        plan::Direction::Short => -daily_rate,
    };
    let net = conviction - signed_cost.max(Decimal::ZERO);
    Ok((net > Decimal::ZERO).then_some(net))
}

#[derive(Debug, Clone, PartialEq)]
pub enum NativeEstimate {
    Available {
        long_cost_24h: rust_decimal::Decimal,
    },
    Unavailable {
        observed_hours: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeForecast {
    pub as_of: chrono::DateTime<chrono::Utc>,
    pub assets: BTreeMap<String, NativeEstimate>,
}

impl NativeForecast {
    pub fn validate_date(&self, as_of: chrono::DateTime<chrono::Utc>) -> Result<(), String> {
        if self.as_of != as_of {
            return Err(format!(
                "native funding forecast dated {} does not match decision {as_of}",
                self.as_of
            ));
        }
        Ok(())
    }
}

pub(crate) fn native_forecast(
    rates: &BTreeMap<chrono::DateTime<chrono::Utc>, BTreeMap<String, rust_decimal::Decimal>>,
    as_of: chrono::DateTime<chrono::Utc>,
    assets: &std::collections::BTreeSet<String>,
) -> Result<NativeForecast, String> {
    use rust_decimal::Decimal;
    if as_of.timestamp() % 3600 != 0 || as_of.timestamp_subsec_nanos() != 0 {
        return Err("native funding forecast requires an hourly decision boundary".into());
    }
    let mut estimates = BTreeMap::new();
    for asset in assets {
        let mut sum = Decimal::ZERO;
        let mut observed_hours = 0;
        for offset in 1..=168 {
            let at = as_of - chrono::Duration::hours(offset);
            if let Some(rate) = rates.get(&at).and_then(|row| row.get(asset)) {
                sum += *rate;
                observed_hours += 1;
            }
        }
        estimates.insert(
            asset.clone(),
            if observed_hours == 168 {
                NativeEstimate::Available {
                    long_cost_24h: sum / Decimal::from(7),
                }
            } else {
                NativeEstimate::Unavailable { observed_hours }
            },
        );
    }
    Ok(NativeForecast {
        as_of,
        assets: estimates,
    })
}

pub(crate) fn native_net_conviction_24h(
    conviction: rust_decimal::Decimal,
    direction: plan::Direction,
    long_cost_24h: rust_decimal::Decimal,
) -> Option<rust_decimal::Decimal> {
    use rust_decimal::Decimal;
    if conviction <= Decimal::ZERO {
        return None;
    }
    let cost = match direction {
        plan::Direction::Long => long_cost_24h,
        plan::Direction::Short => -long_cost_24h,
    };
    let net = conviction - cost;
    (net > Decimal::ZERO).then_some(net)
}

#[cfg(test)]
mod adjustment_tests {
    use super::*;
    use plan::Direction;
    use rust_decimal::Decimal;

    #[test]
    fn native_forecast_requires_all_168_prior_hours_and_ignores_future_events() {
        use chrono::{Duration, Utc};
        let as_of: chrono::DateTime<Utc> = "2026-01-08T00:00:00Z".parse().unwrap();
        let names = ["BTC".into(), "NEW".into()].into_iter().collect();
        let mut rates: BTreeMap<_, _> = (1..=168)
            .map(|offset| {
                (
                    as_of - Duration::hours(offset),
                    BTreeMap::from([("BTC".into(), Decimal::new(1, 4))]),
                )
            })
            .collect();
        let before = native_forecast(&rates, as_of, &names).unwrap();
        assert_eq!(
            before.assets["BTC"],
            NativeEstimate::Available {
                long_cost_24h: Decimal::new(24, 4)
            }
        );
        assert_eq!(
            before.assets["NEW"],
            NativeEstimate::Unavailable { observed_hours: 0 }
        );
        for at in [
            as_of,
            as_of + Duration::hours(1),
            as_of - Duration::hours(169),
        ] {
            rates.insert(at, BTreeMap::from([("BTC".into(), Decimal::from(999))]));
        }
        assert_eq!(native_forecast(&rates, as_of, &names).unwrap(), before);
        rates.remove(&(as_of - Duration::hours(80)));
        assert_eq!(
            native_forecast(&rates, as_of, &names).unwrap().assets["BTC"],
            NativeEstimate::Unavailable {
                observed_hours: 167
            }
        );
        assert!(before.validate_date(as_of).is_ok());
        assert!(before.validate_date(as_of + Duration::hours(1)).is_err());
        assert!(before.validate_date(as_of - Duration::hours(1)).is_err());
        assert!(native_forecast(&rates, as_of + Duration::seconds(1), &names).is_err());
    }

    #[test]
    fn native_carry_rewards_favorable_and_drops_adverse_without_reviving_zero() {
        let price = Decimal::new(1, 2);
        let carry = Decimal::new(1, 3);
        assert_eq!(
            native_net_conviction_24h(price, Direction::Long, carry),
            Some(Decimal::new(9, 3))
        );
        assert_eq!(
            native_net_conviction_24h(price, Direction::Short, carry),
            Some(Decimal::new(11, 3))
        );
        assert_eq!(
            native_net_conviction_24h(price, Direction::Long, -carry),
            Some(Decimal::new(11, 3))
        );
        assert_eq!(
            native_net_conviction_24h(price, Direction::Short, -carry),
            Some(Decimal::new(9, 3))
        );
        assert_eq!(
            native_net_conviction_24h(price, Direction::Long, price),
            None
        );
        assert_eq!(
            native_net_conviction_24h(price, Direction::Short, -price * Decimal::from(2)),
            None
        );
        for direction in [Direction::Long, Direction::Short] {
            for carry in [-Decimal::ONE, Decimal::ONE] {
                assert_eq!(
                    native_net_conviction_24h(Decimal::ZERO, direction, carry),
                    None
                );
            }
        }
    }

    #[test]
    fn adverse_costs_reduce_both_sides_without_rewarding_carry() {
        let expected = Decimal::new(1, 2);
        let reduced = Decimal::new(9, 3);
        assert_eq!(
            net_conviction_24h(expected, Direction::Long, Some(0.007)).unwrap(),
            Some(reduced)
        );
        assert_eq!(
            net_conviction_24h(expected, Direction::Short, Some(-0.007)).unwrap(),
            Some(reduced)
        );
        assert_eq!(
            net_conviction_24h(expected, Direction::Long, Some(-0.007)).unwrap(),
            Some(expected)
        );
        assert_eq!(
            net_conviction_24h(expected, Direction::Short, Some(0.007)).unwrap(),
            Some(expected)
        );
        assert_eq!(
            net_conviction_24h(expected, Direction::Long, Some(0.0)).unwrap(),
            Some(expected)
        );
        assert_eq!(
            net_conviction_24h(expected, Direction::Long, Some(0.07)).unwrap(),
            None
        );
        assert_eq!(
            net_conviction_24h(expected, Direction::Short, Some(-0.14)).unwrap(),
            None
        );
    }

    #[test]
    fn absent_or_nonfinite_funding_is_rejected() {
        for value in [
            None,
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(f64::NEG_INFINITY),
        ] {
            assert!(net_conviction_24h(Decimal::ONE, Direction::Long, value).is_err());
        }
    }

    #[test]
    fn future_realized_funding_cannot_change_today_adjustment() {
        use chrono::TimeDelta;
        let first: chrono::DateTime<chrono::Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let bars: Vec<_> = (0..10)
            .map(|i| features_crypto::Bar {
                ts_utc: first + TimeDelta::days(i),
                asset: "BTC".into(),
                interval_s: 86400,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1000.0,
                quote_volume: Some(100000.0),
                trades: Some(100),
            })
            .collect();
        let cutoff = first + TimeDelta::days(6);
        let mut rates = FundingTable::from([(
            "BTC".into(),
            (0..7)
                .map(|i| ((first + TimeDelta::days(i)).date_naive(), 0.001))
                .collect(),
        )]);
        let before = features_crypto::daily(
            &bars,
            None,
            &BTreeMap::new(),
            &rates,
            features_crypto::FundingWindow::Trailing,
        )
        .unwrap();
        for i in 7..10 {
            rates
                .get_mut("BTC")
                .unwrap()
                .insert((first + TimeDelta::days(i)).date_naive(), -100.0);
        }
        let after = features_crypto::daily(
            &bars,
            None,
            &BTreeMap::new(),
            &rates,
            features_crypto::FundingWindow::Trailing,
        )
        .unwrap();
        let rate = |rows: &[features_crypto::DailyRow]| {
            rows.iter().find(|r| r.ts_utc == cutoff).unwrap().funding_7d
        };
        assert_eq!(rate(&before), rate(&after));
        assert!((rate(&before).unwrap() - 0.007).abs() < 1e-12);
        assert_eq!(
            net_conviction_24h(Decimal::new(1, 2), Direction::Long, rate(&before)).unwrap(),
            net_conviction_24h(Decimal::new(1, 2), Direction::Long, rate(&after)).unwrap()
        );
    }
}
