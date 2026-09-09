//! Fixed raw BTC context appended after cross-sectional rank normalization.
use chrono::NaiveDate;
use features_crypto::DailyRow;

pub const CONTRACT: &str = "btc-daily-raw-v1";
pub const FEATURES: [&str; 4] = [
    "mkt_btc_ret_7d",
    "mkt_btc_ret_30d",
    "mkt_btc_ret_90d",
    "mkt_btc_vol_30d",
];

pub fn values(row: Option<&DailyRow>, score_date: NaiveDate) -> Result<[f64; 4], String> {
    let row = row.ok_or("market context requires the completed BTC daily row")?;
    let expected = score_date.and_hms_opt(0, 0, 0).unwrap().and_utc();
    if row.asset != "BTC" || row.ts_utc != expected {
        return Err(format!(
            "market context requires BTC at {expected}, received {} at {}",
            row.asset, row.ts_utc
        ));
    }
    let mut result = [0.0; 4];
    for (i, value) in [row.ret_7, row.ret_30, row.ret_90, row.vol_30]
        .into_iter()
        .enumerate()
    {
        result[i] = value.filter(|value| value.is_finite()).ok_or_else(|| {
            format!(
                "market context {} is missing or nonfinite at {expected}",
                FEATURES[i]
            )
        })?;
    }
    if result[3] <= 0.0 {
        return Err("market context BTC volatility must be positive".into());
    }
    Ok(result)
}

pub fn ranked_count(features: &[String], contract: Option<&str>) -> Result<usize, String> {
    match contract {
        None => Ok(features.len()),
        Some(CONTRACT) if features.len() > FEATURES.len()
            && features[features.len()-FEATURES.len()..].iter().map(String::as_str).eq(FEATURES) => Ok(features.len()-FEATURES.len()),
        Some(CONTRACT) => Err("market context requires the exact four raw BTC features as a suffix after ranked inputs".into()),
        Some(other) => Err(format!("unknown market context contract {other:?}")),
    }
}
